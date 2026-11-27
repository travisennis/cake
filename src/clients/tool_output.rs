//! Shared tool-output retention, request projection, and session retrieval.
//!
//! A tool result is admitted once: full text up to the hard ceiling is shared
//! with canonical history, oversized results that would exceed the session
//! budget become explicit not-retained previews, and eligible retained results
//! are summarized once when appended or restored. Request construction and
//! compaction estimates consume the cached summary, so a growing session never
//! re-hashes canonical tool text per request.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::config::blob_store::{
    TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES, TOOL_OUTPUT_RETENTION_CEILING_BYTES,
    TOOL_OUTPUT_SESSION_BUDGET_BYTES,
};
use crate::types::ConversationItem;

/// Registered name of the tool that retrieves a projected output.
pub const READ_TOOL_OUTPUT_NAME: &str = "ReadToolOutput";

/// Total number of preview bytes in a projected summary.
pub const TOOL_OUTPUT_PREVIEW_BYTES: usize = 4 * 1024;

/// Marker every appended `PreToolUse` and `PostToolUse` context follows.
pub const HOOK_CONTEXT_MARKER: &str = "\n\nAdditional hook context:\n";

/// Markers that identify a producer-truncated or non-retained result.
///
/// Historical sessions can contain these markers without the shared admission
/// metadata, so projection refuses them and never claims that the full result
/// is retained.
const TRUNCATION_MARKERS: &[&str] = &[
    "[tool result not retained:",
    "[Output too long —",
    "[Captured output not retained —",
    "[Output not retained —",
    "[... mcp output truncated at",
    "[... toolbox output truncated at",
    "[... output truncated at",
    "[Oldest output was dropped:",
];

const TOOL_OUTPUT_PREVIEW_HALF_BYTES: usize = TOOL_OUTPUT_PREVIEW_BYTES / 2;

/// One cached summary for an oversized canonical tool output.
#[derive(Debug, Clone)]
pub struct ToolOutputProjection {
    /// Opaque retrieval handle shown to the model.
    pub handle: String,
    /// Registered tool name that produced the output.
    pub tool_name: String,
    /// Exact model-facing replacement text.
    pub summary: Arc<str>,
    /// Digest of the canonical output identity for checkpoint copies.
    pub output_digest: [u8; 32],
}

/// One canonical tool output retained for session-scoped retrieval.
#[derive(Debug, Clone)]
pub struct ToolOutputEntry {
    /// Opaque retrieval handle shown to the model.
    pub handle: String,
    /// Registered tool name that produced the output.
    pub tool_name: String,
    /// Provider call id that produced the output.
    pub call_id: String,
    /// Full canonical UTF-8 output text, shared with the conversation item.
    pub output: Arc<str>,
    /// Number of lines in `output`.
    pub line_count: usize,
}

impl ToolOutputEntry {
    fn new(handle: String, tool_name: String, call_id: String, output: Arc<str>) -> Self {
        Self {
            handle,
            tool_name,
            call_id,
            line_count: line_count(&output),
            output,
        }
    }
}

/// Current session's projected output index.
///
/// The index is built from canonical conversation history when a session is
/// restored and extended when an eligible result is appended. It is not
/// persisted and never contains a filesystem path, blob digest, or session
/// identifier that the model can supply.
#[derive(Default)]
pub struct ToolOutputStore {
    entries: Mutex<HashMap<String, Arc<ToolOutputEntry>>>,
}

impl fmt::Debug for ToolOutputStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len();
        formatter
            .debug_struct("ToolOutputStore")
            .field("entries", &entries)
            .finish()
    }
}

impl ToolOutputStore {
    /// Replace every indexed entry with a fresh projection of the session.
    pub fn replace(&self, entries: Vec<ToolOutputEntry>) {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.clear();
        guard.extend(
            entries
                .into_iter()
                .map(|entry| (entry.handle.clone(), Arc::new(entry))),
        );
    }

    /// Add one newly appended output to the current session index.
    pub fn insert(&self, entry: ToolOutputEntry) {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(entry.handle.clone(), Arc::new(entry));
    }

    /// Look up one retrieval handle in the current session.
    pub fn get(&self, handle: &str) -> Option<Arc<ToolOutputEntry>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(handle)
            .cloned()
    }
}

/// Whether a tool's output is in the phase-one projection set.
pub fn is_projectable_tool(name: &str) -> bool {
    name == "Bash" || name == "BashSession" || name.starts_with("mcp__") || name.starts_with("tb__")
}

/// One tool result after the shared retention boundary admits it.
#[derive(Debug)]
pub struct AdmittedOutput {
    /// Text stored in canonical history and the retrieval index.
    pub output: Arc<str>,
    /// Full-text bytes this result adds to the session budget, or zero when
    /// the result is smaller than the externalization threshold.
    pub retained_bytes: usize,
}

/// Apply the one hard ceiling and per-session budget to a tool result.
///
/// A result above the ceiling, or an oversized result that would exceed the
/// session budget, becomes a bounded head/tail preview carrying an explicit
/// `not retained` marker. The preview is shorter than the projection
/// threshold, so it never receives a retrieval handle. `ReadToolOutput`
/// results are never previewed: the tool bounds its own output and must remain
/// usable at the budget edge, though its bytes still count toward the budget.
pub fn admit_output(tool_name: &str, output: String, retained_bytes: usize) -> AdmittedOutput {
    let original_bytes = output.len();
    if tool_name == READ_TOOL_OUTPUT_NAME {
        return AdmittedOutput {
            output: Arc::from(output),
            retained_bytes: if original_bytes >= TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES {
                original_bytes
            } else {
                0
            },
        };
    }
    if original_bytes > TOOL_OUTPUT_RETENTION_CEILING_BYTES {
        let reason = format!(
            "output exceeds the {TOOL_OUTPUT_RETENTION_CEILING_BYTES} byte retention ceiling"
        );
        return AdmittedOutput {
            output: Arc::from(not_retained_preview(tool_name, &output, &reason)),
            retained_bytes: 0,
        };
    }
    let counts_toward_budget = original_bytes >= TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES;
    if counts_toward_budget
        && retained_bytes.saturating_add(original_bytes) > TOOL_OUTPUT_SESSION_BUDGET_BYTES
    {
        let reason = format!(
            "retaining it would exceed the {TOOL_OUTPUT_SESSION_BUDGET_BYTES} byte session budget \
             (already {retained_bytes} bytes)"
        );
        return AdmittedOutput {
            output: Arc::from(not_retained_preview(tool_name, &output, &reason)),
            retained_bytes: 0,
        };
    }
    AdmittedOutput {
        output: Arc::from(output),
        retained_bytes: if counts_toward_budget {
            original_bytes
        } else {
            0
        },
    }
}

/// Sum the oversized tool-output bytes retained by canonical history.
pub fn retained_bytes(items: &[ConversationItem]) -> usize {
    items
        .iter()
        .filter_map(|item| match item {
            ConversationItem::FunctionCallOutput { output, .. }
                if output.len() >= TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES =>
            {
                Some(output.len())
            },
            _ => None,
        })
        .sum()
}

/// Build a bounded preview that explicitly says the result was not retained.
pub fn not_retained_preview(tool_name: &str, output: &str, reason: &str) -> String {
    let original_bytes = output.len();
    let lines = line_count(output);
    let (head, tail) = if output.len() <= TOOL_OUTPUT_PREVIEW_BYTES {
        (output, "")
    } else {
        (
            utf8_head(output, TOOL_OUTPUT_PREVIEW_HALF_BYTES),
            utf8_tail(output, TOOL_OUTPUT_PREVIEW_HALF_BYTES),
        )
    };
    format!(
        "[tool result not retained: {tool_name}; captured={original_bytes} bytes, {lines} lines; {reason}]\n\
         This result was not retained in the session, so no ReadToolOutput handle is available. \
         Re-run the tool with a narrower request.\n\
         --- head ---\n{head}\n--- tail ---\n{tail}\n[end not retained tool result]"
    )
}

/// Compute the cached projection for every eligible raw-history output.
pub fn project_history(items: &[ConversationItem]) -> Vec<Option<ToolOutputProjection>> {
    let names = output_names(items);
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let ConversationItem::FunctionCallOutput {
                call_id,
                output,
                timestamp,
                images,
            } = item
            else {
                return None;
            };
            if !images.is_empty() {
                return None;
            }
            let name = names[index].as_deref()?;
            project_output(name, call_id, timestamp.as_ref(), output)
        })
        .collect()
}

/// Build one retrieval entry from an item and its cached projection.
pub fn entry_for(
    item: &ConversationItem,
    projection: &ToolOutputProjection,
) -> Option<ToolOutputEntry> {
    let ConversationItem::FunctionCallOutput {
        call_id, output, ..
    } = item
    else {
        return None;
    };
    Some(ToolOutputEntry::new(
        projection.handle.clone(),
        projection.tool_name.clone(),
        call_id.clone(),
        Arc::clone(output),
    ))
}

/// Build the retrieval index for a restored raw history.
pub fn entries_for(
    items: &[ConversationItem],
    projections: &[Option<ToolOutputProjection>],
) -> Vec<ToolOutputEntry> {
    items
        .iter()
        .zip(projections)
        .filter_map(|(item, projection)| {
            projection
                .as_ref()
                .and_then(|projection| entry_for(item, projection))
        })
        .collect()
}

/// Number of logical lines in `text`.
pub fn line_count(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    text.bytes().filter(|byte| *byte == b'\n').count() + usize::from(!text.ends_with('\n'))
}

pub fn project_output(
    tool_name: &str,
    call_id: &str,
    timestamp: Option<&DateTime<Utc>>,
    output: &str,
) -> Option<ToolOutputProjection> {
    if output.len() < TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES
        || !is_projectable_tool(tool_name)
        || TRUNCATION_MARKERS
            .iter()
            .any(|marker| output.contains(marker))
    {
        return None;
    }
    let digest = output_digest(call_id, timestamp, output);
    let handle = handle_from_digest(&digest);
    let summary: Arc<str> = Arc::from(summarize(tool_name, call_id, &handle, output));
    Some(ToolOutputProjection {
        handle,
        tool_name: tool_name.to_string(),
        summary,
        output_digest: digest,
    })
}

/// Digest of one canonical output identity.
pub fn output_digest(call_id: &str, timestamp: Option<&DateTime<Utc>>, output: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(call_id.as_bytes());
    hasher.update([0]);
    if let Some(timestamp) = timestamp {
        hasher.update(timestamp.timestamp_millis().to_be_bytes());
    }
    hasher.update(output.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&digest);
    bytes
}

fn handle_from_digest(digest: &[u8; 32]) -> String {
    let mut prefix = [0u8; 16];
    prefix.copy_from_slice(&digest[..16]);
    format!("result-{}", hex::encode(prefix))
}

fn output_names(items: &[ConversationItem]) -> Vec<Option<String>> {
    let mut names = vec![None; items.len()];
    let mut open: HashMap<&str, &str> = HashMap::new();
    for (index, item) in items.iter().enumerate() {
        match item {
            ConversationItem::FunctionCall { call_id, name, .. } => {
                open.insert(call_id, name);
            },
            ConversationItem::FunctionCallOutput { call_id, .. } => {
                names[index] = open.remove(call_id.as_str()).map(str::to_string);
            },
            ConversationItem::Message { .. } | ConversationItem::Reasoning { .. } => {},
        }
    }
    names
}

fn split_hook_context(output: &str) -> (&str, Option<&str>) {
    output
        .find(HOOK_CONTEXT_MARKER)
        .map_or((output, None), |index| {
            match (output.get(..index), output.get(index..)) {
                (Some(head), Some(hook)) => (head, Some(hook)),
                _ => (output, None),
            }
        })
}

fn summarize(tool_name: &str, call_id: &str, handle: &str, output: &str) -> String {
    let original_bytes = output.len();
    let lines = line_count(output);
    let (body, hook) = split_hook_context(output);
    let (head, tail) = if body.len() <= TOOL_OUTPUT_PREVIEW_BYTES {
        (body, "")
    } else {
        (
            utf8_head(body, TOOL_OUTPUT_PREVIEW_HALF_BYTES),
            utf8_tail(body, TOOL_OUTPUT_PREVIEW_HALF_BYTES),
        )
    };
    let omitted = body
        .len()
        .saturating_sub(head.len())
        .saturating_sub(tail.len());
    let hook_detail = hook.map_or_else(String::new, |hook| {
        format!("; hook_context={} bytes retained in full", hook.len())
    });
    let mut summary = format!(
        "[externalized tool result: {tool_name}; call_id={call_id}; original={original_bytes} bytes, \
         {lines} lines; handle={handle}; omitted={omitted} bytes{hook_detail}]\n\
         The full result is retained in this session. Use ReadToolOutput with this handle and an \
         optional start_line/end_line range, start_byte/end_byte window, or literal search.\n\
         --- head ---\n{head}\n--- tail ---\n{tail}"
    );
    if let Some(hook) = hook {
        summary.push_str("\n--- hook context (retained in full) ---");
        summary.push_str(hook);
    }
    summary.push_str("\n[end externalized tool result]");
    summary
}

fn utf8_head(text: &str, max_bytes: usize) -> &str {
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or_default()
}

fn utf8_tail(text: &str, max_bytes: usize) -> &str {
    let mut start = text.len().saturating_sub(max_bytes);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    text.get(start..).unwrap_or_default()
}
