//! Session persistence and transcript record types.
//!
//! These are domain types used to describe persisted JSONL session records
//! and the live stream-json output. They are backend-agnostic and live with
//! the other domain types in `crate::types`.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::types::conversation::{ConversationItem, ReasoningContent, Role};
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
    Interrupted,
}

/// Outcome of a completed task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskOutcome {
    Success { result: Option<String> },
    ErrorDuringExecution { error: String },
    Interrupted,
}

impl TaskOutcome {
    pub const fn subtype(&self) -> TaskCompleteSubtype {
        match self {
            Self::Success { .. } => TaskCompleteSubtype::Success,
            Self::ErrorDuringExecution { .. } => TaskCompleteSubtype::ErrorDuringExecution,
            Self::Interrupted => TaskCompleteSubtype::Interrupted,
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
            Self::Interrupted => TaskOutcomeFields {
                subtype: self.subtype(),
                is_error: self.is_error(),
                result: None,
                error: None,
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
            TaskCompleteSubtype::Interrupted => Ok(Self::Interrupted),
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<Vec<String>>,
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

    HookEvent(HookEventData),

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
            StreamRecord::HookEvent(d) => Self::HookEvent(d),
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
            | Self::HookEvent(_)
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
            | Self::HookEvent(_)
            | Self::TaskComplete(_) => None,
        }
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
