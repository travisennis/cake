//! Request-only projection and session index for oversized tool outputs.

use std::collections::{HashMap, VecDeque};
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

const TOOL_OUTPUT_PREVIEW_HALF_BYTES: usize = TOOL_OUTPUT_PREVIEW_BYTES / 2;

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
    pub output: String,
    /// Number of lines in `output`.
    pub line_count: usize,
}

impl ToolOutputEntry {
    /// Build an entry from canonical output text.
    pub fn new(handle: String, tool_name: String, call_id: String, output: String) -> Self {
        let line_count = line_count(&output);
        Self {
            handle,
            tool_name,
            call_id,
            output,
            line_count,
        }
    }
}

/// Current session's projected output index.
///
/// The index is rebuilt from canonical conversation history before a tool
/// batch runs. It is not persisted and never contains a filesystem path,
/// blob digest, or session identifier that the model can supply.
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

/// Whether one item contains a projectable output that needs a request view.
pub fn has_candidate_output(items: &[ConversationItem]) -> bool {
    let names = output_names(items);
    items.iter().enumerate().any(|(index, item)| {
        matches!(
            item,
            ConversationItem::FunctionCallOutput { output, images, .. }
                if images.is_empty()
                    && output.len() >= TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES
                    && is_projectable_tool(names[index].as_deref().unwrap_or_default())
        )
    })
}

/// Rewrite eligible oversized outputs in a provider projection.
///
/// `history` supplies canonical output text and stable raw indexes. `projected`
/// is the request view, which may contain checkpoints and raw suffixes. An
/// output without a matching raw record stays inline because its retrieval
/// handle would not survive replay.
pub fn project_request(history: &[ConversationItem], projected: &mut [ConversationItem]) -> bool {
    let mut handles = raw_handles(history);
    let names = output_names(projected);
    let mut changed = false;
    for (index, item) in projected.iter_mut().enumerate() {
        let ConversationItem::FunctionCallOutput {
            call_id,
            output,
            timestamp,
            images,
        } = item
        else {
            continue;
        };
        let handle = handles
            .get_mut(&output_key(call_id, timestamp.as_ref(), output))
            .and_then(VecDeque::pop_front);
        let Some(handle) = handle else {
            continue;
        };
        if !images.is_empty()
            || output.len() < TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES
            || !is_projectable_tool(names[index].as_deref().unwrap_or_default())
        {
            continue;
        }
        let summary = summarize(
            names[index].as_deref().unwrap_or("unknown"),
            call_id,
            &handle,
            output,
        );
        *output = summary;
        changed = true;
    }
    changed
}

/// Build the current session's retrieval index from canonical history.
pub fn collect_entries(history: &[ConversationItem]) -> Vec<ToolOutputEntry> {
    let names = output_names(history);
    let mut entries = Vec::new();
    for (index, item) in history.iter().enumerate() {
        let ConversationItem::FunctionCallOutput {
            call_id,
            output,
            timestamp,
            images,
        } = item
        else {
            continue;
        };
        let Some(tool_name) = names[index].as_deref() else {
            continue;
        };
        if !images.is_empty()
            || output.len() < TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES
            || !is_projectable_tool(tool_name)
        {
            continue;
        }
        entries.push(ToolOutputEntry::new(
            handle(call_id, timestamp.as_ref(), output),
            tool_name.to_string(),
            call_id.clone(),
            output.clone(),
        ));
    }
    entries
}

/// Number of logical lines in `text`.
pub fn line_count(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    text.bytes().filter(|byte| *byte == b'\n').count() + usize::from(!text.ends_with('\n'))
}

/// Deterministic retrieval handle for one canonical output identity.
fn handle(call_id: &str, timestamp: Option<&DateTime<Utc>>, output: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(call_id.as_bytes());
    hasher.update([0]);
    if let Some(timestamp) = timestamp {
        hasher.update(timestamp.timestamp_millis().to_be_bytes());
    }
    hasher.update(output.as_bytes());
    let digest = hasher.finalize();
    let mut prefix = [0u8; 16];
    prefix.copy_from_slice(&digest[..16]);
    format!("result-{}", hex::encode(prefix))
}

/// Stable matching key for a function-call output across a checkpoint copy.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct OutputKey {
    call_id: String,
    timestamp_ms: Option<i64>,
    digest: [u8; 32],
}

fn output_key(call_id: &str, timestamp: Option<&DateTime<Utc>>, output: &str) -> OutputKey {
    let digest = Sha256::digest(output.as_bytes());
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&digest);
    OutputKey {
        call_id: call_id.to_string(),
        timestamp_ms: timestamp.map(DateTime::timestamp_millis),
        digest: bytes,
    }
}

fn raw_handles(history: &[ConversationItem]) -> HashMap<OutputKey, VecDeque<String>> {
    let mut handles: HashMap<OutputKey, VecDeque<String>> = HashMap::new();
    for item in history {
        let ConversationItem::FunctionCallOutput {
            call_id,
            output,
            timestamp,
            ..
        } = item
        else {
            continue;
        };
        handles
            .entry(output_key(call_id, timestamp.as_ref(), output))
            .or_default()
            .push_back(handle(call_id, timestamp.as_ref(), output));
    }
    handles
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

fn summarize(tool_name: &str, call_id: &str, handle: &str, output: &str) -> String {
    let original_bytes = output.len();
    let lines = line_count(output);
    let head = utf8_head(output, TOOL_OUTPUT_PREVIEW_HALF_BYTES);
    let tail = utf8_tail(output, TOOL_OUTPUT_PREVIEW_HALF_BYTES);
    let omitted = original_bytes
        .saturating_sub(head.len())
        .saturating_sub(tail.len());
    format!(
        "[externalized tool result: {tool_name}; call_id={call_id}; original={original_bytes} bytes, \
         {lines} lines; handle={handle}; omitted={omitted} bytes]\n\
         The full result is retained in this session. Use ReadToolOutput with this handle and an \
         optional start_line/end_line range or literal search.\n\
         --- head ---\n{head}\n--- tail ---\n{tail}\n[end externalized tool result]"
    )
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
