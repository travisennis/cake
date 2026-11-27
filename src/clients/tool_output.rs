//! Cached request projection and session index for oversized tool outputs.
//!
//! An eligible output is summarized once when it is appended or restored.
//! Request construction and compaction estimates consume the cached summary,
//! so a growing session never re-hashes canonical tool text per request.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::config::blob_store::TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES;
use crate::types::ConversationItem;

/// Registered name of the tool that retrieves a projected output.
pub const READ_TOOL_OUTPUT_NAME: &str = "ReadToolOutput";

/// Total number of preview bytes in a projected summary.
pub const TOOL_OUTPUT_PREVIEW_BYTES: usize = 4 * 1024;

/// Marker every appended `PreToolUse` and `PostToolUse` context follows.
pub const HOOK_CONTEXT_MARKER: &str = "\n\nAdditional hook context:\n";

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
    /// Full canonical UTF-8 output text.
    pub output: Arc<str>,
    /// Number of lines in `output`.
    pub line_count: usize,
}

impl ToolOutputEntry {
    fn new(handle: String, tool_name: String, call_id: String, output: &str) -> Self {
        Self {
            handle,
            tool_name,
            call_id,
            output: Arc::from(output),
            line_count: line_count(output),
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
    name == "Bash" || name.starts_with("mcp__") || name.starts_with("tb__")
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
        output,
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
    if output.len() < TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES || !is_projectable_tool(tool_name)
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
         The result returned to Cake is retained in this session. Use ReadToolOutput with this handle and an \
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
