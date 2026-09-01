//! Session persistence and transcript record types.
//!
//! These are domain types used to describe persisted JSONL session records
//! and the live stream-json output. They are backend-agnostic and live with
//! the other domain types in `crate::types`.

use std::fmt;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::types::conversation::{ConversationItem, ReasoningContent, ReasoningSummary, Role};
use crate::types::usage::Usage;

/// Snapshot of git repository state captured when a session file is created.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct GitState {
    pub repository_url: Option<String>,
    pub branch: Option<String>,
    pub commit_hash: Option<String>,
}

/// Subtype of a task completion record.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskCompleteSubtype {
    Success,
    ErrorDuringExecution,
    ErrorOutputSchema,
    Interrupted,
    CutOff,
    /// The agent loop stopped because a user-configured `max_turns` or
    /// `max_tool_calls` limit was reached.
    LimitExceeded,
}

/// Outcome of a completed task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskOutcome {
    Success {
        result: Option<String>,
    },
    ErrorDuringExecution {
        error: String,
    },
    /// The final response could not be made valid against the caller's
    /// `--output-schema` (refusal, truncation, or correction exhaustion).
    ErrorOutputSchema {
        error: String,
    },
    Interrupted,
    /// The model's response was cut off or incomplete — no final assistant
    /// message was produced (e.g. truncated during reasoning, empty response,
    /// or the model stopped without a concluding message).
    CutOff {
        detail: String,
    },
    /// The agent loop stopped because a user-configured `max_turns` or
    /// `max_tool_calls` limit was reached. `result` carries the partial
    /// result (the last assistant message, if any) so completed work is not
    /// discarded.
    LimitExceeded {
        /// The settings key that fired: `max_turns` or `max_tool_calls`.
        limit: String,
        /// Human-readable detail naming the limit.
        detail: String,
        /// The last assistant message produced before the limit fired, if any.
        result: Option<String>,
    },
}

impl TaskOutcome {
    pub const fn is_error(&self) -> bool {
        !matches!(self, Self::Success { .. })
    }
}

const fn task_outcome_subtype(outcome: &TaskOutcome) -> TaskCompleteSubtype {
    match outcome {
        TaskOutcome::Success { .. } => TaskCompleteSubtype::Success,
        TaskOutcome::ErrorDuringExecution { .. } => TaskCompleteSubtype::ErrorDuringExecution,
        TaskOutcome::ErrorOutputSchema { .. } => TaskCompleteSubtype::ErrorOutputSchema,
        TaskOutcome::Interrupted => TaskCompleteSubtype::Interrupted,
        TaskOutcome::CutOff { .. } => TaskCompleteSubtype::CutOff,
        TaskOutcome::LimitExceeded { .. } => TaskCompleteSubtype::LimitExceeded,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit: Option<&'a str>,
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
    #[serde(default)]
    limit: Option<String>,
}

impl Serialize for TaskOutcome {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let fields = match self {
            Self::Success { result } => TaskOutcomeFields {
                subtype: task_outcome_subtype(self),
                is_error: self.is_error(),
                result: result.as_deref(),
                error: None,
                limit: None,
            },
            Self::ErrorDuringExecution { error }
            | Self::ErrorOutputSchema { error }
            | Self::CutOff { detail: error } => TaskOutcomeFields {
                subtype: task_outcome_subtype(self),
                is_error: self.is_error(),
                result: None,
                error: Some(error),
                limit: None,
            },
            Self::Interrupted => TaskOutcomeFields {
                subtype: task_outcome_subtype(self),
                is_error: self.is_error(),
                result: None,
                error: None,
                limit: None,
            },
            Self::LimitExceeded {
                limit,
                detail,
                result,
            } => TaskOutcomeFields {
                subtype: task_outcome_subtype(self),
                is_error: self.is_error(),
                result: result.as_deref(),
                error: Some(detail),
                limit: Some(limit),
            },
        };

        fields.serialize(serializer)
    }
}

impl OwnedTaskOutcomeFields {
    fn validate_consistency<E: serde::de::Error>(&self) -> Result<(), E> {
        let expected_success = matches!(self.subtype, TaskCompleteSubtype::Success);
        let expected_is_error = !expected_success;
        if self
            .is_error
            .is_some_and(|is_error| is_error != expected_is_error)
            || self
                .success
                .is_some_and(|success| success != expected_success)
        {
            return Err(E::custom(
                "task completion outcome fields do not match subtype",
            ));
        }
        if self.is_error.is_none() && self.success.is_none() {
            return Err(E::custom("task completion outcome requires is_error"));
        }
        Ok(())
    }
}

fn required_task_field<E: serde::de::Error>(
    value: Option<String>,
    subtype: &str,
    field: &str,
) -> Result<String, E> {
    value.ok_or_else(|| {
        E::custom(format!(
            "task completion {subtype} outcome requires {field}"
        ))
    })
}

impl<'de> Deserialize<'de> for TaskOutcome {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = OwnedTaskOutcomeFields::deserialize(deserializer)?;
        fields.validate_consistency::<D::Error>()?;

        match fields.subtype {
            TaskCompleteSubtype::Success => Ok(Self::Success {
                result: fields.result,
            }),
            TaskCompleteSubtype::ErrorDuringExecution => Ok(Self::ErrorDuringExecution {
                error: required_task_field::<D::Error>(
                    fields.error,
                    "error_during_execution",
                    "error",
                )?,
            }),
            TaskCompleteSubtype::ErrorOutputSchema => Ok(Self::ErrorOutputSchema {
                error: required_task_field::<D::Error>(
                    fields.error,
                    "error_output_schema",
                    "error",
                )?,
            }),
            TaskCompleteSubtype::Interrupted => Ok(Self::Interrupted),
            TaskCompleteSubtype::CutOff => Ok(Self::CutOff {
                detail: required_task_field::<D::Error>(fields.error, "cut_off", "error")?,
            }),
            TaskCompleteSubtype::LimitExceeded => Ok(Self::LimitExceeded {
                limit: required_task_field::<D::Error>(fields.limit, "limit_exceeded", "limit")?,
                detail: required_task_field::<D::Error>(fields.error, "limit_exceeded", "error")?,
                result: fields.result,
            }),
        }
    }
}

/// Error returned when the agent loop completes without a final assistant
/// message (e.g. cut off during reasoning, empty response, or the model
/// stopped without a concluding message).
#[derive(Debug, Clone)]
pub struct CutOffError {
    pub detail: String,
}

impl CutOffError {
    pub const fn new(detail: String) -> Self {
        Self { detail }
    }
}

impl fmt::Display for CutOffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for CutOffError {}

/// Error returned when the agent loop stops because a user-configured
/// `max_turns` or `max_tool_calls` limit was reached.
///
/// `result` carries the partial result (the last assistant message, if any)
/// so completed work is surfaced rather than discarded.
#[derive(Debug, Clone)]
pub struct LimitExceededError {
    /// The settings key that fired: `max_turns` or `max_tool_calls`.
    pub limit: String,
    /// Human-readable detail naming the limit.
    pub detail: String,
    /// The last assistant message produced before the limit fired, if any.
    pub result: Option<String>,
}

impl LimitExceededError {
    pub const fn new(limit: String, detail: String, result: Option<String>) -> Self {
        Self {
            limit,
            detail,
            result,
        }
    }
}

impl fmt::Display for LimitExceededError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)?;
        if let Some(result) = &self.result {
            write!(f, "\n\nPartial result:\n{result}")?;
        }
        Ok(())
    }
}

impl std::error::Error for LimitExceededError {}

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

/// Declared safety of replaying a tool call after an interrupted execution.
///
/// The declaration is snapshotted in tool-call records. A missing declaration
/// in a historical record is treated as [`Self::Never`].
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReplaySafety {
    /// Re-execution is safe when the current declaration also says `safe`.
    Safe,
    /// Re-execution must not happen automatically.
    Never,
}

/// Shared data for `FunctionCall` records in both `StreamRecord` and `SessionRecord`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FunctionCallData {
    pub id: String,
    pub call_id: String,
    pub name: String,
    pub arguments: String,
    /// Error message when `arguments` is not valid JSON, indicating the model
    /// emitted malformed tool arguments. Present only when parsing fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments_parse_error: Option<String>,
    /// Tool replay declaration captured when Cake handled the call. Absent on
    /// historical records written before replay declarations were added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<ReplaySafety>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
}

/// Shared data for `FunctionCallOutput` records in both `StreamRecord` and `SessionRecord`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FunctionCallOutputData {
    pub call_id: String,
    pub output: String,
    /// The replay declaration associated with the tool call that produced this
    /// output. Synthetic recovery outputs leave this absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<ReplaySafety>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
}

/// Shared data for `Reasoning` records in both `StreamRecord` and `SessionRecord`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ReasoningData {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<Vec<ReasoningSummary>>,
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

/// Bounded terminal outcome of one provider attempt.
///
/// This vocabulary is shared by provider-attempt telemetry and session usage
/// audit records. It describes the provider request boundary, not the later
/// agent-loop result classification.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApiAttemptTerminalClass {
    /// The request completed and produced a parseable provider response.
    Completed,
    /// The request exceeded the configured HTTP deadline.
    Timeout,
    /// The provider attempt future was cancelled before it completed.
    Cancelled,
    /// The request phase failed (connect or stale connection).
    Transport,
    /// The provider returned a non-2xx HTTP response.
    Http,
    /// The 2xx body could not be decoded or parsed into a provider response.
    BodyParse,
    /// The provider accepted the request, then emitted a terminal
    /// `response.failed` event.
    ResponseFailed,
}

/// Token usage for one provider attempt.
///
/// Session-only audit record: persisted to the session file but never emitted
/// to stream-json output. Per-attempt usage lets a resumed session know the
/// current context size before the next provider request and makes token cost
/// reconstructible even when a response is retried or discarded.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TurnUsageData {
    pub session_id: String,
    pub task_id: String,
    /// 1-based index of the logical agent turn.
    pub turn: u32,
    pub usage: Usage,
    pub timestamp: DateTime<Utc>,
    /// 1-based provider-attempt ordinal. Absent on the original successful
    /// single-attempt shape for compatibility with existing records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// Provider-attempt terminal class. Absent on the original successful
    /// single-attempt shape for compatibility with existing records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_class: Option<ApiAttemptTerminalClass>,
}

/// Shared data for `HookEvent` records in both `StreamRecord` and `SessionRecord`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HookEventData {
    pub timestamp: DateTime<Utc>,
    pub task_id: String,
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input_summary: Option<String>,
    pub source_file: PathBuf,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub decision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_decision: Option<String>,
    pub fail_closed: bool,
    pub stdout: String,
    pub stderr: String,
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

    HookEvent(HookEventData),

    Reasoning(ReasoningData),

    TaskComplete(TaskCompleteData),

    /// Per-attempt token usage (session-only; not emitted to stream-json).
    TurnUsage(TurnUsageData),
}

/// Machine-readable category for a `replay_error` stream record.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplayErrorKind {
    /// The `--output-format` is not `stream-json`.
    OutputFormat,
    /// The session UUID argument is not a valid UUID.
    InvalidUuid,
    /// No session file exists for the UUID.
    SessionNotFound,
    /// The session file is unreadable or its records are corrupt.
    Corrupt,
    /// The session file's format version is unsupported.
    UnsupportedFormat,
    /// The session file could not be opened (permission denied).
    Permission,
}

/// A single line in `--output-format stream-json` output.
///
/// Live streams emit only task-scoped records and intentionally exclude
/// `session_meta` and `prompt_context`, so live output cannot be mistaken for a
/// complete resumable session file. `cake replay` reuses the same vocabulary
/// and additionally emits `session_meta`, `prompt_context`, `skill_activated`,
/// and, on failure, `replay_error`. New variants and fields are additive.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamRecord {
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

    HookEvent(HookEventData),

    Reasoning(ReasoningData),

    TaskComplete(TaskCompleteData),

    /// Structured failure emitted by `cake replay` before the process exits
    /// non-zero, so stream-json parsers can learn why replay failed without
    /// waiting for the exit code. Never emitted by live streams.
    ReplayError {
        /// Session UUID being replayed; absent when the UUID was invalid.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        /// Machine-readable failure category.
        kind: ReplayErrorKind,
        /// Human-readable failure detail.
        error: String,
        /// Process exit code that accompanies this failure.
        exit_code: u8,
    },
}

impl From<StreamRecord> for SessionRecord {
    fn from(record: StreamRecord) -> Self {
        match record {
            StreamRecord::SessionMeta {
                format_version,
                session_id,
                timestamp,
                working_directory,
                model,
                tools,
                cake_version,
                system_prompt,
                git,
            } => Self::SessionMeta {
                format_version,
                session_id,
                timestamp,
                working_directory,
                model,
                tools,
                cake_version,
                system_prompt,
                git,
            },
            StreamRecord::TaskStart(d) => Self::TaskStart(d),
            StreamRecord::PromptContext {
                session_id,
                task_id,
                role,
                content,
                timestamp,
            } => Self::PromptContext {
                session_id,
                task_id,
                role,
                content,
                timestamp,
            },
            StreamRecord::Message(d) => Self::Message(d),
            StreamRecord::FunctionCall(d) => Self::FunctionCall(d),
            StreamRecord::FunctionCallOutput(d) => Self::FunctionCallOutput(d),
            StreamRecord::SkillActivated {
                session_id,
                task_id,
                timestamp,
                name,
                path,
            } => Self::SkillActivated {
                session_id,
                task_id,
                timestamp,
                name,
                path,
            },
            StreamRecord::HookEvent(d) => Self::HookEvent(d),
            StreamRecord::Reasoning(d) => Self::Reasoning(d),
            StreamRecord::TaskComplete(d) => Self::TaskComplete(d),
            // `replay_error` is emitted only by `cake replay`, which never
            // writes to a session file; there is no persisted counterpart.
            StreamRecord::ReplayError { .. } => {
                unreachable!("replay_error stream records are never persisted to a session file")
            },
        }
    }
}

impl From<SessionRecord> for StreamRecord {
    fn from(record: SessionRecord) -> Self {
        match record {
            SessionRecord::SessionMeta {
                format_version,
                session_id,
                timestamp,
                working_directory,
                model,
                tools,
                cake_version,
                system_prompt,
                git,
            } => Self::SessionMeta {
                format_version,
                session_id,
                timestamp,
                working_directory,
                model,
                tools,
                cake_version,
                system_prompt,
                git,
            },
            SessionRecord::TaskStart(d) => Self::TaskStart(d),
            SessionRecord::PromptContext {
                session_id,
                task_id,
                role,
                content,
                timestamp,
            } => Self::PromptContext {
                session_id,
                task_id,
                role,
                content,
                timestamp,
            },
            SessionRecord::Message(d) => Self::Message(d),
            SessionRecord::FunctionCall(d) => Self::FunctionCall(d),
            SessionRecord::FunctionCallOutput(d) => Self::FunctionCallOutput(d),
            SessionRecord::SkillActivated {
                session_id,
                task_id,
                timestamp,
                name,
                path,
            } => Self::SkillActivated {
                session_id,
                task_id,
                timestamp,
                name,
                path,
            },
            SessionRecord::HookEvent(d) => Self::HookEvent(d),
            SessionRecord::Reasoning(d) => Self::Reasoning(d),
            SessionRecord::TaskComplete(d) => Self::TaskComplete(d),
            // `turn_usage` is a session-only audit record with no stream-json
            // counterpart; `cake replay` filters it before conversion.
            SessionRecord::TurnUsage(_) => {
                unreachable!("turn_usage records are session-only and have no stream counterpart")
            },
        }
    }
}

impl StreamRecord {
    /// Convert a [`ConversationItem`] into its corresponding `StreamRecord`
    /// variant without a replay declaration.
    pub fn from_conversation_item(item: &ConversationItem) -> Self {
        Self::from_conversation_item_with_replay(item, None)
    }

    /// Convert a [`ConversationItem`] into a stream record and attach the
    /// registry's execution-time replay declaration to tool call/output data.
    /// The declaration is metadata only and is not part of the provider-facing
    /// conversation item.
    pub fn from_conversation_item_with_replay(
        item: &ConversationItem,
        replay: Option<ReplaySafety>,
    ) -> Self {
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
                arguments_parse_error: serde_json::from_str::<serde_json::Value>(arguments)
                    .err()
                    .map(|e| e.to_string()),
                replay,
                timestamp: *timestamp,
            }),
            ConversationItem::FunctionCallOutput {
                call_id,
                output,
                timestamp,
            } => Self::FunctionCallOutput(FunctionCallOutputData {
                call_id: call_id.clone(),
                output: output.clone(),
                replay,
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
            | Self::HookEvent(_)
            | Self::TaskComplete(_)
            | Self::TurnUsage(_) => {},
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
                arguments_parse_error: _,
                replay: _,
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
                replay: _,
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
            | Self::HookEvent(_)
            | Self::TaskComplete(_)
            | Self::TurnUsage(_) => None,
        }
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
