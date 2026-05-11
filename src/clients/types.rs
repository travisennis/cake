use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::config::ReasoningEffort;
use crate::models::Role;

/// Snapshot of git repository state captured when a session file is created.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct GitState {
    pub repository_url: Option<String>,
    pub branch: Option<String>,
    pub commit_hash: Option<String>,
}

// =============================================================================
// Reasoning Content (preserved for API round-tripping)
// =============================================================================

/// A content item within a reasoning output, preserved verbatim for echoing
/// back to the API in multi-turn conversations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningContent {
    #[serde(rename = "type")]
    pub content_type: ReasoningContentKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Protocol-defined kind for reasoning content items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReasoningContentKind {
    ReasoningText,
    SummaryText,
    Unknown(String),
}

impl ReasoningContentKind {
    pub const fn as_str(&self) -> &str {
        match self {
            Self::ReasoningText => "reasoning_text",
            Self::SummaryText => "summary_text",
            Self::Unknown(value) => value.as_str(),
        }
    }
}

impl From<&str> for ReasoningContentKind {
    fn from(value: &str) -> Self {
        match value {
            "reasoning_text" => Self::ReasoningText,
            "summary_text" => Self::SummaryText,
            other => Self::Unknown(other.to_string()),
        }
    }
}

impl From<String> for ReasoningContentKind {
    fn from(value: String) -> Self {
        match value.as_str() {
            "reasoning_text" => Self::ReasoningText,
            "summary_text" => Self::SummaryText,
            _ => Self::Unknown(value),
        }
    }
}

impl Serialize for ReasoningContentKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ReasoningContentKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Self::from)
    }
}

// =============================================================================
// Conversation Item Enum (for Responses API input/output)
// =============================================================================

/// Represents a single item in the conversation history, mapping directly to
/// the Responses API input/output array format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationItem {
    Message {
        role: Role,
        content: String,
        /// Assistant message ID (required for assistant messages in input)
        id: Option<String>,
        /// "completed" or "incomplete" (required for assistant messages in input)
        status: Option<String>,
        /// Timestamp when this item was created
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<DateTime<Utc>>,
    },
    FunctionCall {
        id: String,
        call_id: String,
        name: String,
        arguments: String,
        /// Timestamp when this item was created
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<DateTime<Utc>>,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
        /// Timestamp when this item was created
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<DateTime<Utc>>,
    },
    Reasoning {
        id: String,
        summary: Vec<String>,
        /// Opaque encrypted reasoning content that must be echoed back to the
        /// API for multi-turn conversations with reasoning models.
        #[serde(skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<String>,
        /// Original content array from the API response (e.g., `reasoning_text` items).
        /// Must be echoed back so the router can reconstruct `reasoning_content`
        /// for Chat Completions providers like Moonshot AI.
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<Vec<ReasoningContent>>,
        /// Timestamp when this item was created
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<DateTime<Utc>>,
    },
}

/// Typed Responses API input item serialized into the request `input` array.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ResponsesApiInputItem<'a> {
    Message {
        role: &'a str,
        content: Vec<ResponsesMessageContent<'a>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<&'a str>,
    },
    FunctionCall {
        id: &'a str,
        call_id: &'a str,
        name: &'a str,
        arguments: &'a str,
    },
    FunctionCallOutput {
        call_id: &'a str,
        output: &'a str,
    },
    Reasoning {
        id: &'a str,
        summary: Vec<ResponsesReasoningSummary<'a>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<&'a [ReasoningContent]>,
    },
}

/// Content block used by Responses API message input.
#[derive(Debug, Serialize)]
pub(super) struct ResponsesMessageContent<'a> {
    #[serde(rename = "type")]
    content_type: &'static str,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    annotations: Option<Vec<serde_json::Value>>,
}

/// Summary block used by Responses API reasoning input.
#[derive(Debug, Serialize)]
pub(super) struct ResponsesReasoningSummary<'a> {
    #[serde(rename = "type")]
    summary_type: &'static str,
    text: &'a str,
}

impl<'a> From<&'a ConversationItem> for ResponsesApiInputItem<'a> {
    fn from(item: &'a ConversationItem) -> Self {
        match item {
            ConversationItem::Message {
                role,
                content,
                id,
                status,
                ..
            } => {
                let content_type = if matches!(role, Role::Assistant) {
                    "output_text"
                } else {
                    "input_text"
                };
                let annotations =
                    matches!(role, Role::Assistant).then(Vec::<serde_json::Value>::new);

                Self::Message {
                    role: role.as_str(),
                    content: vec![ResponsesMessageContent {
                        content_type,
                        text: content,
                        annotations,
                    }],
                    id: id.as_deref(),
                    status: status.as_deref(),
                }
            },
            ConversationItem::FunctionCall {
                id,
                call_id,
                name,
                arguments,
                ..
            } => Self::FunctionCall {
                id,
                call_id,
                name,
                arguments,
            },
            ConversationItem::FunctionCallOutput {
                call_id, output, ..
            } => Self::FunctionCallOutput { call_id, output },
            ConversationItem::Reasoning {
                id,
                summary,
                encrypted_content,
                content,
                ..
            } => Self::Reasoning {
                id,
                summary: summary
                    .iter()
                    .map(|text| ResponsesReasoningSummary {
                        summary_type: "summary_text",
                        text,
                    })
                    .collect(),
                encrypted_content: encrypted_content.as_deref(),
                content: content.as_deref(),
            },
        }
    }
}

impl ConversationItem {
    pub(super) fn to_api_input_item(&self) -> ResponsesApiInputItem<'_> {
        ResponsesApiInputItem::from(self)
    }

    /// Convert this item to JSON format for API input.
    #[cfg(test)]
    pub(super) fn to_api_input(&self) -> serde_json::Value {
        serde_json::to_value(self.to_api_input_item()).unwrap_or_else(|error| {
            serde_json::json!({
                "type": "error",
                "error": format!("failed to serialize Responses API input: {error}")
            })
        })
    }
}

// =============================================================================
// Session Record Enum (for unified JSONL schema)
// =============================================================================

/// Subtype of a task completion record.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskCompleteSubtype {
    Success,
    ErrorDuringExecution,
}

/// Outcome of a completed task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskOutcome {
    Success { result: Option<String> },
    ErrorDuringExecution { error: String },
}

impl TaskOutcome {
    pub const fn subtype(&self) -> TaskCompleteSubtype {
        match self {
            Self::Success { .. } => TaskCompleteSubtype::Success,
            Self::ErrorDuringExecution { .. } => TaskCompleteSubtype::ErrorDuringExecution,
        }
    }

    pub const fn is_error(&self) -> bool {
        !matches!(self, Self::Success { .. })
    }
}

#[derive(Serialize)]
struct TaskOutcomeFields<'a> {
    subtype: TaskCompleteSubtype,
    is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<&'a str>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

#[derive(Deserialize)]
struct OwnedTaskOutcomeFields {
    subtype: TaskCompleteSubtype,
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    is_error: Option<bool>,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

impl Serialize for TaskOutcome {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let fields = match self {
            Self::Success { result } => TaskOutcomeFields {
                subtype: self.subtype(),
                is_error: self.is_error(),
                result: result.as_deref(),
                error: None,
            },
            Self::ErrorDuringExecution { error } => TaskOutcomeFields {
                subtype: self.subtype(),
                is_error: self.is_error(),
                result: None,
                error: Some(error),
            },
        };

        fields.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TaskOutcome {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = OwnedTaskOutcomeFields::deserialize(deserializer)?;
        let expected_success = matches!(fields.subtype, TaskCompleteSubtype::Success);
        let expected_is_error = !expected_success;
        if fields
            .is_error
            .is_some_and(|is_error| is_error != expected_is_error)
            || fields
                .success
                .is_some_and(|success| success != expected_success)
        {
            return Err(serde::de::Error::custom(
                "task completion outcome fields do not match subtype",
            ));
        }
        if fields.is_error.is_none() && fields.success.is_none() {
            return Err(serde::de::Error::custom(
                "task completion outcome requires is_error",
            ));
        }

        match fields.subtype {
            TaskCompleteSubtype::Success => Ok(Self::Success {
                result: fields.result,
            }),
            TaskCompleteSubtype::ErrorDuringExecution => Ok(Self::ErrorDuringExecution {
                error: fields.error.ok_or_else(|| {
                    serde::de::Error::custom(
                        "task completion error_during_execution outcome requires error",
                    )
                })?,
            }),
        }
    }
}

// =============================================================================
// Shared inner structs for SessionRecord / StreamRecord variant data
// =============================================================================

/// Shared data for `TaskStart` records in both `StreamRecord` and `SessionRecord`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TaskStartData {
    pub session_id: String,
    pub task_id: String,
    pub timestamp: DateTime<Utc>,
}

/// Shared data for `Message` records in both `StreamRecord` and `SessionRecord`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MessageData {
    pub role: Role,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
}

/// Shared data for `FunctionCall` records in both `StreamRecord` and `SessionRecord`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FunctionCallData {
    pub id: String,
    pub call_id: String,
    pub name: String,
    pub arguments: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
}

/// Shared data for `FunctionCallOutput` records in both `StreamRecord` and `SessionRecord`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FunctionCallOutputData {
    pub call_id: String,
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
}

/// Shared data for `Reasoning` records in both `StreamRecord` and `SessionRecord`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ReasoningData {
    pub id: String,
    pub summary: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ReasoningContent>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
}

/// Shared data for `TaskComplete` records in both `StreamRecord` and `SessionRecord`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TaskCompleteData {
    #[serde(flatten)]
    pub outcome: TaskOutcome,
    pub duration_ms: u64,
    pub turn_count: u32,
    pub tool_call_count: u32,
    pub session_id: String,
    pub task_id: String,
    pub usage: Usage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_denials: Option<Vec<String>>,
}

// =============================================================================
// Session Record Enum (for unified JSONL schema)
// =============================================================================

/// A single line in an append-only JSONL session file.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionRecord {
    /// First line of every persisted session file.
    SessionMeta {
        format_version: u32,
        session_id: String,
        /// Timestamp when the session was created.
        timestamp: DateTime<Utc>,
        working_directory: PathBuf,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        tools: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cake_version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt: Option<String>,
        #[serde(default)]
        git: GitState,
    },

    TaskStart(TaskStartData),

    /// Initial prompt context used for one invocation.
    ///
    /// These records are append-only audit entries. They are intentionally not
    /// replayed from session history; each invocation rebuilds fresh prompt
    /// context from current AGENTS.md files, skills, and environment state.
    PromptContext {
        session_id: String,
        task_id: String,
        role: Role,
        content: String,
        timestamp: DateTime<Utc>,
    },

    Message(MessageData),

    FunctionCall(FunctionCallData),

    FunctionCallOutput(FunctionCallOutputData),

    SkillActivated {
        session_id: String,
        task_id: String,
        timestamp: DateTime<Utc>,
        name: String,
        path: PathBuf,
    },

    HookEvent {
        timestamp: DateTime<Utc>,
        task_id: String,
        event: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        source: Option<String>,
        source_file: PathBuf,
        command: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        duration_ms: u64,
        decision: String,
        fail_closed: bool,
        stdout: String,
        stderr: String,
    },

    Reasoning(ReasoningData),

    TaskComplete(TaskCompleteData),
}

/// A single line in `--output-format stream-json` output for the current task.
///
/// This intentionally excludes `session_meta`, so live stream output cannot be
/// mistaken for a complete resumable session file.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamRecord {
    TaskStart(TaskStartData),

    Message(MessageData),

    FunctionCall(FunctionCallData),

    FunctionCallOutput(FunctionCallOutputData),

    Reasoning(ReasoningData),

    TaskComplete(TaskCompleteData),
}

impl From<StreamRecord> for SessionRecord {
    fn from(record: StreamRecord) -> Self {
        match record {
            StreamRecord::TaskStart(d) => Self::TaskStart(d),
            StreamRecord::Message(d) => Self::Message(d),
            StreamRecord::FunctionCall(d) => Self::FunctionCall(d),
            StreamRecord::FunctionCallOutput(d) => Self::FunctionCallOutput(d),
            StreamRecord::Reasoning(d) => Self::Reasoning(d),
            StreamRecord::TaskComplete(d) => Self::TaskComplete(d),
        }
    }
}

impl StreamRecord {
    /// Convert a `ConversationItem` into its corresponding `StreamRecord` variant.
    pub fn from_conversation_item(item: &ConversationItem) -> Self {
        match item {
            ConversationItem::Message {
                role,
                content,
                id,
                status,
                timestamp,
            } => Self::Message(MessageData {
                role: *role,
                content: content.clone(),
                id: id.clone(),
                status: status.clone(),
                timestamp: *timestamp,
            }),
            ConversationItem::FunctionCall {
                id,
                call_id,
                name,
                arguments,
                timestamp,
            } => Self::FunctionCall(FunctionCallData {
                id: id.clone(),
                call_id: call_id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
                timestamp: *timestamp,
            }),
            ConversationItem::FunctionCallOutput {
                call_id,
                output,
                timestamp,
            } => Self::FunctionCallOutput(FunctionCallOutputData {
                call_id: call_id.clone(),
                output: output.clone(),
                timestamp: *timestamp,
            }),
            ConversationItem::Reasoning {
                id,
                summary,
                encrypted_content,
                content,
                timestamp,
            } => Self::Reasoning(ReasoningData {
                id: id.clone(),
                summary: summary.clone(),
                encrypted_content: encrypted_content.clone(),
                content: content.clone(),
                timestamp: *timestamp,
            }),
        }
    }
}

impl SessionRecord {
    /// Fill legacy omissions that are no longer absent in newly written sessions.
    pub(crate) fn normalize_legacy_fields(&mut self, fallback_timestamp: DateTime<Utc>) {
        match self {
            Self::Message(MessageData { timestamp, .. })
            | Self::FunctionCall(FunctionCallData { timestamp, .. })
            | Self::FunctionCallOutput(FunctionCallOutputData { timestamp, .. })
            | Self::Reasoning(ReasoningData { timestamp, .. }) => {
                timestamp.get_or_insert(fallback_timestamp);
            },
            Self::SessionMeta { .. }
            | Self::TaskStart(_)
            | Self::PromptContext { .. }
            | Self::SkillActivated { .. }
            | Self::HookEvent { .. }
            | Self::TaskComplete(_) => {},
        }
    }

    /// Convert a `SessionRecord` back into a `ConversationItem`, if applicable.
    /// Returns `None` for session metadata and task boundary records, which have no
    /// `ConversationItem` equivalent.
    pub fn to_conversation_item(&self) -> Option<ConversationItem> {
        match self {
            Self::Message(MessageData {
                role,
                content,
                id,
                status,
                timestamp,
            }) => Some(ConversationItem::Message {
                role: *role,
                content: content.clone(),
                id: id.clone(),
                status: status.clone(),
                timestamp: *timestamp,
            }),
            Self::FunctionCall(FunctionCallData {
                id,
                call_id,
                name,
                arguments,
                timestamp,
            }) => Some(ConversationItem::FunctionCall {
                id: id.clone(),
                call_id: call_id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
                timestamp: *timestamp,
            }),
            Self::FunctionCallOutput(FunctionCallOutputData {
                call_id,
                output,
                timestamp,
            }) => Some(ConversationItem::FunctionCallOutput {
                call_id: call_id.clone(),
                output: output.clone(),
                timestamp: *timestamp,
            }),
            Self::Reasoning(ReasoningData {
                id,
                summary,
                encrypted_content,
                content,
                timestamp,
            }) => Some(ConversationItem::Reasoning {
                id: id.clone(),
                summary: summary.clone(),
                encrypted_content: encrypted_content.clone(),
                content: content.clone(),
                timestamp: *timestamp,
            }),
            Self::SessionMeta { .. }
            | Self::TaskStart(_)
            | Self::PromptContext { .. }
            | Self::SkillActivated { .. }
            | Self::HookEvent { .. }
            | Self::TaskComplete(_) => None,
        }
    }
}

// =============================================================================
// API Request/Response DTOs (internal to clients)
// =============================================================================

#[derive(Clone, Serialize)]
pub(super) struct ProviderConfig {
    pub(super) only: Vec<String>,
}

#[derive(Clone, Serialize)]
pub(super) struct ReasoningConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) effort: Option<ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) max_tokens: Option<u32>,
}

#[derive(Serialize)]
pub(super) struct Request<'a> {
    pub(super) model: &'a str,
    pub(super) input: Vec<ResponsesApiInputItem<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) instructions: Option<&'a str>,
    pub(super) temperature: Option<f32>,
    pub(super) top_p: Option<f32>,
    pub(super) max_output_tokens: Option<u32>,
    pub(super) tools: Option<Vec<super::tools::Tool>>,
    pub(super) tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) provider: Option<ProviderConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reasoning: Option<ReasoningConfig>,
}

#[derive(Deserialize, Debug)]
pub(super) struct ApiResponse {
    pub(super) id: Option<String>,
    pub(super) output: Vec<OutputMessage>,
    pub(super) usage: Option<ApiUsage>,
    #[expect(dead_code)]
    pub(super) status: Option<String>,
    #[expect(dead_code)]
    pub(super) error: Option<ApiError>,
}

#[derive(Deserialize, Debug, Clone)]
pub(super) struct OutputMessage {
    #[serde(rename = "type")]
    pub(super) msg_type: String,
    pub(super) id: Option<String>,
    pub(super) call_id: Option<String>,
    pub(super) name: Option<String>,
    pub(super) arguments: Option<String>,
    #[expect(dead_code)]
    pub(super) role: Option<String>,
    pub(super) status: Option<String>,
    pub(super) content: Option<Vec<OutputContent>>,
    /// Opaque encrypted reasoning content returned by reasoning models.
    pub(super) encrypted_content: Option<String>,
    /// Top-level summary strings on reasoning items (alternative to content-based summaries).
    pub(super) summary: Option<Vec<String>>,
}

#[derive(Deserialize, Debug, Clone)]
pub(super) struct OutputContent {
    #[serde(rename = "type")]
    pub(super) content_type: String,
    pub(super) text: Option<String>,
}

/// Usage statistics for API calls
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub input_tokens_details: InputTokensDetails,
    pub output_tokens: u64,
    pub output_tokens_details: OutputTokensDetails,
    pub total_tokens: u64,
}

/// Details about input tokens
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct InputTokensDetails {
    pub cached_tokens: u64,
}

/// Details about output tokens
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct OutputTokensDetails {
    pub reasoning_tokens: u64,
}

/// Internal usage struct for API response deserialization (with optional fields)
#[derive(Deserialize, Debug, Clone, Default)]
pub(super) struct ApiUsage {
    pub(super) input_tokens: Option<u64>,
    pub(super) input_tokens_details: Option<ApiInputTokensDetails>,
    pub(super) output_tokens: Option<u64>,
    pub(super) output_tokens_details: Option<ApiOutputTokensDetails>,
    pub(super) total_tokens: Option<u64>,
}

/// Internal input tokens details for API response deserialization
#[derive(Deserialize, Debug, Clone, Default)]
pub(super) struct ApiInputTokensDetails {
    pub(super) cached_tokens: Option<u64>,
}

/// Internal output tokens details for API response deserialization
#[derive(Deserialize, Debug, Clone, Default)]
pub(super) struct ApiOutputTokensDetails {
    pub(super) reasoning_tokens: Option<u64>,
}

#[expect(dead_code)]
#[derive(Deserialize, Debug)]
pub(super) struct ApiError {
    pub(super) code: Option<String>,
    pub(super) message: String,
    pub(super) metadata: Option<serde_json::Value>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::config::session::CURRENT_FORMAT_VERSION;

    fn stream_json_for(item: &ConversationItem) -> serde_json::Value {
        serde_json::to_value(StreamRecord::from_conversation_item(item)).unwrap()
    }

    fn session_json_for(item: &ConversationItem) -> serde_json::Value {
        let stream_record = StreamRecord::from_conversation_item(item);
        let session_record = SessionRecord::from(stream_record);
        serde_json::to_value(session_record).unwrap()
    }

    fn fixed_timestamp() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-10T12:34:56Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn timestamp_at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn session_record_json(record: SessionRecord) -> serde_json::Value {
        serde_json::to_value(record).unwrap()
    }

    fn fixed_session_id() -> String {
        "550e8400-e29b-41d4-a716-446655440000".to_string()
    }

    fn fixed_task_id() -> String {
        "550e8400-e29b-41d4-a716-446655440001".to_string()
    }

    fn assert_conversation_item_stream_session_roundtrip(item: &ConversationItem) {
        let stream_record = StreamRecord::from_conversation_item(item);
        let session_record = SessionRecord::from(stream_record);
        let restored = session_record.to_conversation_item().unwrap();
        assert_eq!(*item, restored);
    }

    #[test]
    fn reasoning_content_kind_serializes_known_values() {
        let json = serde_json::to_value(ReasoningContent {
            content_type: ReasoningContentKind::ReasoningText,
            text: Some("thinking".to_string()),
        })
        .unwrap();

        assert_eq!(json["type"], "reasoning_text");
        assert_eq!(json["text"], "thinking");
    }

    #[test]
    fn reasoning_content_kind_preserves_unknown_values() {
        let content: ReasoningContent = serde_json::from_value(serde_json::json!({
            "type": "provider_specific_reasoning",
            "text": "opaque"
        }))
        .unwrap();

        assert_eq!(
            content.content_type,
            ReasoningContentKind::Unknown("provider_specific_reasoning".to_string())
        );

        let json = serde_json::to_value(content).unwrap();
        assert_eq!(json["type"], "provider_specific_reasoning");
        assert_eq!(json["text"], "opaque");
    }

    #[test]
    fn to_api_input_user_message() {
        let item = ConversationItem::Message {
            role: Role::User,
            content: "Hello".to_string(),
            id: None,
            status: None,
            timestamp: None,
        };
        let json = item.to_api_input();
        assert_eq!(json["type"], "message");
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"][0]["type"], "input_text");
        assert_eq!(json["content"][0]["text"], "Hello");
    }

    #[test]
    fn task_outcome_serializes_canonical_task_complete_fields() {
        let record = StreamRecord::TaskComplete(TaskCompleteData {
            outcome: TaskOutcome::Success {
                result: Some("done".to_string()),
            },
            duration_ms: 10,
            turn_count: 1,
            tool_call_count: 2,
            session_id: "session-1".to_string(),
            task_id: "task-1".to_string(),
            usage: Usage::default(),
            permission_denials: None,
        });

        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["type"], "task_complete");
        assert_eq!(json["subtype"], "success");
        assert_eq!(json["is_error"], false);
        assert_eq!(json["result"], "done");
        assert!(json.get("success").is_none());
        assert!(json.get("error").is_none());
    }

    #[test]
    fn task_outcome_deserializes_legacy_success_field() {
        let json = serde_json::json!({
            "type": "task_complete",
            "subtype": "success",
            "success": true,
            "is_error": false,
            "duration_ms": 10,
            "turn_count": 1,
            "tool_call_count": 0,
            "session_id": "session-1",
            "task_id": "task-1",
            "usage": Usage::default()
        });

        let record = serde_json::from_value::<StreamRecord>(json).unwrap();
        assert!(matches!(
            record,
            StreamRecord::TaskComplete(TaskCompleteData {
                outcome: TaskOutcome::Success { .. },
                ..
            })
        ));
    }

    #[test]
    fn task_outcome_deserializes_legacy_success_only_field() {
        let json = serde_json::json!({
            "type": "task_complete",
            "subtype": "success",
            "success": true,
            "duration_ms": 10,
            "turn_count": 1,
            "tool_call_count": 0,
            "session_id": "session-1",
            "task_id": "task-1",
            "usage": Usage::default()
        });

        let record = serde_json::from_value::<StreamRecord>(json).unwrap();
        assert!(matches!(
            record,
            StreamRecord::TaskComplete(TaskCompleteData {
                outcome: TaskOutcome::Success { .. },
                ..
            })
        ));
    }

    #[test]
    fn task_outcome_rejects_inconsistent_legacy_success_field() {
        let json = serde_json::json!({
            "type": "task_complete",
            "subtype": "success",
            "success": false,
            "is_error": false,
            "duration_ms": 10,
            "turn_count": 1,
            "tool_call_count": 0,
            "session_id": "session-1",
            "task_id": "task-1",
            "usage": Usage::default()
        });

        let err = serde_json::from_value::<StreamRecord>(json).unwrap_err();
        assert!(
            err.to_string()
                .contains("outcome fields do not match subtype")
        );
    }

    #[test]
    fn to_api_input_assistant_message_uses_output_text() {
        let item = ConversationItem::Message {
            role: Role::Assistant,
            content: "Hi".to_string(),
            id: Some("msg-1".to_string()),
            status: Some("completed".to_string()),
            timestamp: None,
        };
        let json = item.to_api_input();
        assert_eq!(json["role"], "assistant");
        assert_eq!(json["content"][0]["type"], "output_text");
        assert_eq!(json["content"][0]["text"], "Hi");
        assert_eq!(json["id"], "msg-1");
        assert_eq!(json["status"], "completed");
    }

    #[test]
    fn to_api_input_system_message() {
        let item = ConversationItem::Message {
            role: Role::System,
            content: "You are helpful".to_string(),
            id: None,
            status: None,
            timestamp: None,
        };
        let json = item.to_api_input();
        assert_eq!(json["role"], "system");
        assert_eq!(json["content"][0]["type"], "input_text");
    }

    #[test]
    fn to_api_input_tool_message() {
        let item = ConversationItem::Message {
            role: Role::Tool,
            content: "tool result".to_string(),
            id: None,
            status: None,
            timestamp: None,
        };
        let json = item.to_api_input();
        assert_eq!(json["role"], "tool");
        assert_eq!(json["content"][0]["type"], "input_text");
    }

    #[test]
    fn to_api_input_function_call() {
        let item = ConversationItem::FunctionCall {
            id: "fc-1".to_string(),
            call_id: "call-1".to_string(),
            name: "bash".to_string(),
            arguments: r#"{"cmd":"ls"}"#.to_string(),
            timestamp: None,
        };
        let json = item.to_api_input();
        assert_eq!(json["type"], "function_call");
        assert_eq!(json["id"], "fc-1");
        assert_eq!(json["call_id"], "call-1");
        assert_eq!(json["name"], "bash");
        assert_eq!(json["arguments"], r#"{"cmd":"ls"}"#);
    }

    #[test]
    fn to_api_input_function_call_output() {
        let item = ConversationItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: "file.txt".to_string(),
            timestamp: None,
        };
        let json = item.to_api_input();
        assert_eq!(json["type"], "function_call_output");
        assert_eq!(json["call_id"], "call-1");
        assert_eq!(json["output"], "file.txt");
    }

    #[test]
    fn to_api_input_reasoning() {
        let item = ConversationItem::Reasoning {
            id: "r-1".to_string(),
            summary: vec!["thinking...".to_string()],
            encrypted_content: None,
            content: None,
            timestamp: None,
        };
        let json = item.to_api_input();
        assert_eq!(json["type"], "reasoning");
        assert_eq!(json["id"], "r-1");
        assert_eq!(json["summary"][0]["type"], "summary_text");
        assert_eq!(json["summary"][0]["text"], "thinking...");
    }

    #[test]
    fn to_api_input_reasoning_multiple_summaries() {
        let item = ConversationItem::Reasoning {
            id: "r-2".to_string(),
            summary: vec!["step 1".to_string(), "step 2".to_string()],
            encrypted_content: None,
            content: None,
            timestamp: None,
        };
        let json = item.to_api_input();
        assert_eq!(json["summary"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn to_api_input_reasoning_with_encrypted_content() {
        let item = ConversationItem::Reasoning {
            id: "r-1".to_string(),
            summary: vec!["thinking...".to_string()],
            encrypted_content: Some("gAAAAABencrypted...".to_string()),
            content: None,
            timestamp: None,
        };
        let json = item.to_api_input();
        assert_eq!(json["type"], "reasoning");
        assert_eq!(json["encrypted_content"], "gAAAAABencrypted...");
    }

    #[test]
    fn to_api_input_reasoning_without_encrypted_content_omits_field() {
        let item = ConversationItem::Reasoning {
            id: "r-1".to_string(),
            summary: vec!["thinking...".to_string()],
            encrypted_content: None,
            content: None,
            timestamp: None,
        };
        let json = item.to_api_input();
        assert!(json.get("encrypted_content").is_none());
    }

    #[test]
    fn to_api_input_reasoning_with_content() {
        let item = ConversationItem::Reasoning {
            id: "r-1".to_string(),
            summary: vec!["thinking...".to_string()],
            encrypted_content: None,
            timestamp: None,
            content: Some(vec![ReasoningContent {
                content_type: ReasoningContentKind::ReasoningText,
                text: Some("deep thoughts".to_string()),
            }]),
        };
        let json = item.to_api_input();
        assert_eq!(json["content"][0]["type"], "reasoning_text");
        assert_eq!(json["content"][0]["text"], "deep thoughts");
    }

    #[test]
    fn stream_record_json_message() {
        let item = ConversationItem::Message {
            role: Role::User,
            content: "Hello".to_string(),
            id: None,
            status: None,
            timestamp: None,
        };
        let json = stream_json_for(&item);
        assert_eq!(json["type"], "message");
        assert_eq!(json["content"], "Hello");
    }

    #[test]
    fn stream_record_json_message_with_id_and_status() {
        let item = ConversationItem::Message {
            role: Role::Assistant,
            content: "Response".to_string(),
            id: Some("msg-123".to_string()),
            status: Some("completed".to_string()),
            timestamp: None,
        };
        let json = stream_json_for(&item);
        assert_eq!(json["id"], "msg-123");
        assert_eq!(json["status"], "completed");
    }

    #[test]
    fn stream_record_json_reasoning_uses_plain_summary() {
        let item = ConversationItem::Reasoning {
            id: "r-1".to_string(),
            summary: vec!["step 1".to_string()],
            encrypted_content: None,
            content: None,
            timestamp: None,
        };
        let json = stream_json_for(&item);
        assert_eq!(json["type"], "reasoning");
        // Streaming format uses plain strings, not objects
        assert_eq!(json["summary"][0], "step 1");
    }

    #[test]
    fn stream_record_json_function_call() {
        let item = ConversationItem::FunctionCall {
            id: "fc-1".to_string(),
            call_id: "call-1".to_string(),
            name: "bash".to_string(),
            arguments: r#"{"cmd":"ls"}"#.to_string(),
            timestamp: None,
        };
        let json = stream_json_for(&item);
        assert_eq!(json["type"], "function_call");
        assert_eq!(json["name"], "bash");
    }

    #[test]
    fn stream_record_json_function_call_output() {
        let item = ConversationItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: "result".to_string(),
            timestamp: None,
        };
        let json = stream_json_for(&item);
        assert_eq!(json["type"], "function_call_output");
        assert_eq!(json["output"], "result");
    }

    #[test]
    fn conversation_item_serialization_roundtrip() {
        let items = vec![
            ConversationItem::Message {
                role: Role::User,
                content: "test".to_string(),
                id: None,
                status: None,
                timestamp: None,
            },
            ConversationItem::FunctionCall {
                id: "fc".to_string(),
                call_id: "call".to_string(),
                name: "tool".to_string(),
                arguments: "{}".to_string(),
                timestamp: None,
            },
            ConversationItem::FunctionCallOutput {
                call_id: "call".to_string(),
                output: "out".to_string(),
                timestamp: None,
            },
            ConversationItem::Reasoning {
                id: "r".to_string(),
                summary: vec!["s".to_string()],
                encrypted_content: None,
                content: None,
                timestamp: None,
            },
        ];

        for item in items {
            let json = serde_json::to_string(&item).unwrap();
            let deserialized: ConversationItem = serde_json::from_str(&json).unwrap();
            assert_eq!(json, serde_json::to_string(&deserialized).unwrap());
        }
    }

    #[test]
    fn conversation_items_roundtrip_through_stream_and_session_records() {
        let items = vec![
            ConversationItem::Message {
                role: Role::User,
                content: "plain user message".to_string(),
                id: None,
                status: None,
                timestamp: None,
            },
            ConversationItem::Message {
                role: Role::Assistant,
                content: "assistant response".to_string(),
                id: Some("msg-assistant-1".to_string()),
                status: Some("completed".to_string()),
                timestamp: Some(timestamp_at("2026-05-10T00:00:00Z")),
            },
            ConversationItem::Message {
                role: Role::System,
                content: "system instruction".to_string(),
                id: Some("msg-system-1".to_string()),
                status: Some("completed".to_string()),
                timestamp: Some(timestamp_at("2026-05-10T00:00:01Z")),
            },
            ConversationItem::FunctionCall {
                id: "fc-1".to_string(),
                call_id: "call-1".to_string(),
                name: "bash".to_string(),
                arguments: r#"{"cmd":"ls"}"#.to_string(),
                timestamp: Some(timestamp_at("2026-05-10T00:00:02Z")),
            },
            ConversationItem::FunctionCallOutput {
                call_id: "call-1".to_string(),
                output: "file.txt".to_string(),
                timestamp: Some(timestamp_at("2026-05-10T00:00:03Z")),
            },
            ConversationItem::Reasoning {
                id: "reasoning-encrypted".to_string(),
                summary: vec!["step 1".to_string()],
                encrypted_content: Some("gAAAAABencrypted...".to_string()),
                content: None,
                timestamp: Some(timestamp_at("2026-05-10T00:00:04Z")),
            },
            ConversationItem::Reasoning {
                id: "reasoning-content".to_string(),
                summary: vec!["step 1".to_string(), "step 2".to_string()],
                encrypted_content: None,
                content: Some(vec![ReasoningContent {
                    content_type: ReasoningContentKind::ReasoningText,
                    text: Some("deep analysis".to_string()),
                }]),
                timestamp: Some(timestamp_at("2026-05-10T00:00:05Z")),
            },
            ConversationItem::Reasoning {
                id: "reasoning-both".to_string(),
                summary: vec!["step 1".to_string()],
                encrypted_content: Some("gAAAAABencrypted...".to_string()),
                content: Some(vec![
                    ReasoningContent {
                        content_type: ReasoningContentKind::SummaryText,
                        text: Some("summary".to_string()),
                    },
                    ReasoningContent {
                        content_type: ReasoningContentKind::Unknown(
                            "provider_specific_reasoning".to_string(),
                        ),
                        text: Some("opaque".to_string()),
                    },
                ]),
                timestamp: Some(timestamp_at("2026-05-10T00:00:06Z")),
            },
        ];

        for item in &items {
            assert_conversation_item_stream_session_roundtrip(item);
        }
    }

    #[test]
    fn usage_default_values() {
        let usage = Usage::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.total_tokens, 0);
        assert_eq!(usage.input_tokens_details.cached_tokens, 0);
        assert_eq!(usage.output_tokens_details.reasoning_tokens, 0);
    }

    #[test]
    fn usage_serialization() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
            input_tokens_details: InputTokensDetails { cached_tokens: 20 },
            output_tokens_details: OutputTokensDetails {
                reasoning_tokens: 10,
            },
        };
        let json = serde_json::to_string(&usage).unwrap();
        assert!(json.contains("\"input_tokens\":100"));
        assert!(json.contains("\"output_tokens\":50"));
        assert!(json.contains("\"total_tokens\":150"));
    }

    #[test]
    fn provider_config_serialization() {
        let config = ProviderConfig {
            only: vec!["Fireworks".to_string(), "Moonshot AI".to_string()],
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("\"only\":["));
        assert!(json.contains("\"Fireworks\""));
        assert!(json.contains("\"Moonshot AI\""));
    }

    #[test]
    fn provider_config_single_provider() {
        let config = ProviderConfig {
            only: vec!["OpenAI".to_string()],
        };
        let json = serde_json::to_string(&config).unwrap();
        let expected = r#"{"only":["OpenAI"]}"#;
        assert_eq!(json, expected);
    }

    #[test]
    fn prompt_context_records_are_audit_only() {
        let record = SessionRecord::PromptContext {
            session_id: "session-1".to_string(),
            task_id: "task-1".to_string(),
            role: Role::Developer,
            content: "mutable context".to_string(),
            timestamp: DateTime::parse_from_rfc3339("2026-05-03T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };

        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["type"], "prompt_context");
        assert_eq!(json["role"], "developer");
        assert_eq!(json["content"], "mutable context");
        assert!(record.to_conversation_item().is_none());
    }

    #[test]
    fn snapshot_user_message() {
        let item = ConversationItem::Message {
            role: Role::User,
            content: "Hello".to_string(),
            id: None,
            status: None,
            timestamp: None,
        };
        insta::assert_json_snapshot!("to_api_input_user_message", item.to_api_input());
    }

    #[test]
    fn snapshot_assistant_message_with_id_and_status() {
        let item = ConversationItem::Message {
            role: Role::Assistant,
            content: "Hi there".to_string(),
            id: Some("msg-1".to_string()),
            status: Some("completed".to_string()),
            timestamp: None,
        };
        insta::assert_json_snapshot!(
            "to_api_input_assistant_message_with_id_and_status",
            item.to_api_input()
        );
    }

    #[test]
    fn snapshot_system_message() {
        let item = ConversationItem::Message {
            role: Role::System,
            content: "You are cake".to_string(),
            id: None,
            status: None,
            timestamp: None,
        };
        insta::assert_json_snapshot!("to_api_input_system_message", item.to_api_input());
    }

    #[test]
    fn snapshot_function_call() {
        let item = ConversationItem::FunctionCall {
            id: "fc-1".to_string(),
            call_id: "call-1".to_string(),
            name: "bash".to_string(),
            arguments: r#"{"cmd":"ls"}"#.to_string(),
            timestamp: None,
        };
        insta::assert_json_snapshot!("to_api_input_function_call", item.to_api_input());
    }

    #[test]
    fn snapshot_function_call_output() {
        let item = ConversationItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: "file.txt\nother.txt".to_string(),
            timestamp: None,
        };
        insta::assert_json_snapshot!("to_api_input_function_call_output", item.to_api_input());
    }

    #[test]
    fn snapshot_reasoning_with_summary() {
        let item = ConversationItem::Reasoning {
            id: "r-1".to_string(),
            summary: vec!["thinking...".to_string()],
            encrypted_content: None,
            content: None,
            timestamp: None,
        };
        insta::assert_json_snapshot!("to_api_input_reasoning_with_summary", item.to_api_input());
    }

    #[test]
    fn snapshot_reasoning_with_encrypted_content() {
        let item = ConversationItem::Reasoning {
            id: "r-1".to_string(),
            summary: vec!["thinking...".to_string()],
            encrypted_content: Some("gAAAAABencrypted...".to_string()),
            content: None,
            timestamp: None,
        };
        insta::assert_json_snapshot!(
            "to_api_input_reasoning_with_encrypted_content",
            item.to_api_input()
        );
    }

    #[test]
    fn snapshot_reasoning_with_content_array() {
        let item = ConversationItem::Reasoning {
            id: "r-1".to_string(),
            summary: vec!["thinking...".to_string()],
            encrypted_content: None,
            content: Some(vec![ReasoningContent {
                content_type: ReasoningContentKind::ReasoningText,
                text: Some("deep analysis".to_string()),
            }]),
            timestamp: None,
        };
        insta::assert_json_snapshot!(
            "to_api_input_reasoning_with_content_array",
            item.to_api_input()
        );
    }

    #[test]
    fn snapshot_stream_record_json_message_with_id_and_status() {
        let item = ConversationItem::Message {
            role: Role::Assistant,
            content: "Response".to_string(),
            id: Some("msg-123".to_string()),
            status: Some("completed".to_string()),
            timestamp: None,
        };
        insta::assert_json_snapshot!(
            "stream_record_json_message_with_id_and_status",
            stream_json_for(&item)
        );
    }

    #[test]
    fn snapshot_stream_record_json_reasoning_plain_summary() {
        let item = ConversationItem::Reasoning {
            id: "r-1".to_string(),
            summary: vec!["step 1".to_string(), "step 2".to_string()],
            encrypted_content: None,
            content: None,
            timestamp: None,
        };
        insta::assert_json_snapshot!(
            "stream_record_json_reasoning_plain_summary",
            stream_json_for(&item)
        );
    }

    #[test]
    fn snapshot_stream_record_json_function_call() {
        let item = ConversationItem::FunctionCall {
            id: "fc-1".to_string(),
            call_id: "call-1".to_string(),
            name: "bash".to_string(),
            arguments: r#"{"cmd":"ls"}"#.to_string(),
            timestamp: None,
        };
        insta::assert_json_snapshot!("stream_record_json_function_call", stream_json_for(&item));
    }

    #[test]
    fn snapshot_stream_record_json_function_call_output() {
        let item = ConversationItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: "result".to_string(),
            timestamp: None,
        };
        insta::assert_json_snapshot!(
            "stream_record_json_function_call_output",
            stream_json_for(&item)
        );
    }

    #[test]
    fn snapshot_session_json_message_with_id_and_status() {
        let item = ConversationItem::Message {
            role: Role::Assistant,
            content: "Response".to_string(),
            id: Some("msg-123".to_string()),
            status: Some("completed".to_string()),
            timestamp: Some(timestamp_at("2026-05-10T00:00:00Z")),
        };
        insta::assert_json_snapshot!(
            "session_json_message_with_id_and_status",
            session_json_for(&item)
        );
    }

    #[test]
    fn snapshot_session_json_reasoning_with_content() {
        let item = ConversationItem::Reasoning {
            id: "r-1".to_string(),
            summary: vec!["step 1".to_string()],
            encrypted_content: Some("gAAAAABencrypted...".to_string()),
            content: Some(vec![ReasoningContent {
                content_type: ReasoningContentKind::ReasoningText,
                text: Some("deep analysis".to_string()),
            }]),
            timestamp: Some(timestamp_at("2026-05-10T00:00:00Z")),
        };
        insta::assert_json_snapshot!(
            "session_json_reasoning_with_content",
            session_json_for(&item)
        );
    }

    #[test]
    fn snapshot_session_json_function_call() {
        let item = ConversationItem::FunctionCall {
            id: "fc-1".to_string(),
            call_id: "call-1".to_string(),
            name: "bash".to_string(),
            arguments: r#"{"cmd":"ls"}"#.to_string(),
            timestamp: Some(timestamp_at("2026-05-10T00:00:00Z")),
        };
        insta::assert_json_snapshot!("session_json_function_call", session_json_for(&item));
    }

    #[test]
    fn snapshot_session_json_function_call_output() {
        let item = ConversationItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: "result".to_string(),
            timestamp: Some(timestamp_at("2026-05-10T00:00:00Z")),
        };
        insta::assert_json_snapshot!("session_json_function_call_output", session_json_for(&item));
    }

    #[test]
    fn snapshot_session_json_session_meta() {
        let record = SessionRecord::SessionMeta {
            format_version: CURRENT_FORMAT_VERSION,
            session_id: fixed_session_id(),
            timestamp: fixed_timestamp(),
            working_directory: PathBuf::from("/workspace/cake"),
            model: Some("gpt-5.4".to_string()),
            tools: vec!["bash".to_string(), "read".to_string(), "edit".to_string()],
            cake_version: Some("1.2.3-test".to_string()),
            system_prompt: Some("You are cake.".to_string()),
            git: GitState {
                repository_url: Some("https://example.com/cake.git".to_string()),
                branch: Some("main".to_string()),
                commit_hash: Some("abcdef1234567890".to_string()),
            },
        };

        insta::assert_json_snapshot!("session_json_session_meta", session_record_json(record));
    }

    #[test]
    fn snapshot_session_json_task_start() {
        let record = SessionRecord::TaskStart(TaskStartData {
            session_id: fixed_session_id(),
            task_id: fixed_task_id(),
            timestamp: fixed_timestamp(),
        });

        insta::assert_json_snapshot!("session_json_task_start", session_record_json(record));
    }

    #[test]
    fn snapshot_session_json_task_complete() {
        let record = SessionRecord::TaskComplete(TaskCompleteData {
            outcome: TaskOutcome::ErrorDuringExecution {
                error: "tool failed".to_string(),
            },
            duration_ms: 1_250,
            turn_count: 3,
            tool_call_count: 5,
            session_id: fixed_session_id(),
            task_id: fixed_task_id(),
            usage: Usage {
                input_tokens: 100,
                input_tokens_details: InputTokensDetails { cached_tokens: 25 },
                output_tokens: 50,
                output_tokens_details: OutputTokensDetails {
                    reasoning_tokens: 10,
                },
                total_tokens: 150,
            },
            permission_denials: Some(vec!["bash: rm -rf /".to_string()]),
        });

        insta::assert_json_snapshot!("session_json_task_complete", session_record_json(record));
    }

    #[test]
    fn snapshot_session_json_prompt_context() {
        let record = SessionRecord::PromptContext {
            session_id: fixed_session_id(),
            task_id: fixed_task_id(),
            role: Role::Developer,
            content: "Use the project instructions.".to_string(),
            timestamp: fixed_timestamp(),
        };

        insta::assert_json_snapshot!("session_json_prompt_context", session_record_json(record));
    }

    #[test]
    fn snapshot_session_json_skill_activated() {
        let record = SessionRecord::SkillActivated {
            session_id: fixed_session_id(),
            task_id: fixed_task_id(),
            timestamp: fixed_timestamp(),
            name: "debugging-cake".to_string(),
            path: PathBuf::from("/workspace/cake/.agents/skills/debugging-cake/SKILL.md"),
        };

        insta::assert_json_snapshot!("session_json_skill_activated", session_record_json(record));
    }

    #[test]
    fn snapshot_session_json_hook_event_with_optional_fields() {
        let record = SessionRecord::HookEvent {
            timestamp: fixed_timestamp(),
            task_id: fixed_task_id(),
            event: "post_tool_use".to_string(),
            source: Some("Bash".to_string()),
            source_file: PathBuf::from("/workspace/cake/.cake/hooks/post-tool-use.sh"),
            command: "./post-tool-use.sh".to_string(),
            exit_code: Some(0),
            duration_ms: 42,
            decision: "allow".to_string(),
            fail_closed: false,
            stdout: "ok".to_string(),
            stderr: String::new(),
        };

        insta::assert_json_snapshot!(
            "session_json_hook_event_with_optional_fields",
            session_record_json(record)
        );
    }

    #[test]
    fn snapshot_session_json_hook_event_without_optional_fields() {
        let record = SessionRecord::HookEvent {
            timestamp: fixed_timestamp(),
            task_id: fixed_task_id(),
            event: "session_start".to_string(),
            source: None,
            source_file: PathBuf::from("/workspace/cake/.cake/hooks/session-start.sh"),
            command: "./session-start.sh".to_string(),
            exit_code: None,
            duration_ms: 17,
            decision: "allow".to_string(),
            fail_closed: true,
            stdout: String::new(),
            stderr: "no exit code".to_string(),
        };

        insta::assert_json_snapshot!(
            "session_json_hook_event_without_optional_fields",
            session_record_json(record)
        );
    }
}
