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
mod tests {
    use super::*;
    use crate::config::session::CURRENT_FORMAT_VERSION;
    use crate::types::conversation::ReasoningContentKind;
    use crate::types::usage::{InputTokensDetails, OutputTokensDetails};

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

    fn hook_event_data_with_optional_fields() -> HookEventData {
        HookEventData {
            timestamp: fixed_timestamp(),
            task_id: fixed_task_id(),
            event: "post_tool_use".to_string(),
            source: Some("Bash".to_string()),
            call_id: Some("call-1".to_string()),
            tool_name: Some("Bash".to_string()),
            tool_input_summary: Some("just ci".to_string()),
            source_file: PathBuf::from("/workspace/cake/.cake/hooks/post-tool-use.sh"),
            command: "./post-tool-use.sh".to_string(),
            exit_code: Some(0),
            duration_ms: 42,
            decision: "none".to_string(),
            resolved_decision: Some("allow".to_string()),
            fail_closed: false,
            stdout: "ok".to_string(),
            stderr: String::new(),
        }
    }

    fn assert_conversation_item_stream_session_roundtrip(item: &ConversationItem) {
        let stream_record = StreamRecord::from_conversation_item(item);
        let session_record = SessionRecord::from(stream_record);
        let restored = session_record.to_conversation_item().unwrap();
        assert_eq!(*item, restored);
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
            summary: Some(vec!["step 1".to_string()]),
            encrypted_content: None,
            content: None,
            timestamp: None,
        };
        let json = stream_json_for(&item);
        assert_eq!(json["type"], "reasoning");
        assert_eq!(json["summary"][0], "step 1");
    }

    #[test]
    fn reasoning_without_summary_omits_summary_and_roundtrips() {
        let item = ConversationItem::Reasoning {
            id: "r-1".to_string(),
            summary: None,
            encrypted_content: None,
            content: Some(vec![ReasoningContent {
                content_type: ReasoningContentKind::ReasoningText,
                text: Some("preserved reasoning".to_string()),
            }]),
            timestamp: None,
        };

        let stream_json = stream_json_for(&item);
        assert_eq!(stream_json["type"], "reasoning");
        assert!(stream_json.get("summary").is_none());

        let stream_record: StreamRecord = serde_json::from_value(stream_json).unwrap();
        let session_record = SessionRecord::from(stream_record);
        let session_json = serde_json::to_value(&session_record).unwrap();
        assert!(session_json.get("summary").is_none());

        let restored = serde_json::from_value::<SessionRecord>(session_json)
            .unwrap()
            .to_conversation_item()
            .unwrap();
        assert_eq!(restored, item);
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
                summary: Some(vec!["step 1".to_string()]),
                encrypted_content: Some("gAAAAABencrypted...".to_string()),
                content: None,
                timestamp: Some(timestamp_at("2026-05-10T00:00:04Z")),
            },
            ConversationItem::Reasoning {
                id: "reasoning-content".to_string(),
                summary: Some(vec!["step 1".to_string(), "step 2".to_string()]),
                encrypted_content: None,
                content: Some(vec![ReasoningContent {
                    content_type: ReasoningContentKind::ReasoningText,
                    text: Some("deep analysis".to_string()),
                }]),
                timestamp: Some(timestamp_at("2026-05-10T00:00:05Z")),
            },
            ConversationItem::Reasoning {
                id: "reasoning-both".to_string(),
                summary: Some(vec!["step 1".to_string()]),
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
            summary: Some(vec!["step 1".to_string(), "step 2".to_string()]),
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
            summary: Some(vec!["step 1".to_string()]),
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
        let record = SessionRecord::HookEvent(hook_event_data_with_optional_fields());

        insta::assert_json_snapshot!(
            "session_json_hook_event_with_optional_fields",
            session_record_json(record)
        );
    }

    #[test]
    fn snapshot_stream_json_hook_event_with_optional_fields() {
        let record = StreamRecord::HookEvent(hook_event_data_with_optional_fields());

        insta::assert_json_snapshot!(
            "stream_record_json_hook_event_with_optional_fields",
            serde_json::to_value(record).unwrap()
        );
    }

    #[test]
    fn snapshot_session_json_hook_event_without_optional_fields() {
        let record = SessionRecord::HookEvent(HookEventData {
            timestamp: fixed_timestamp(),
            task_id: fixed_task_id(),
            event: "session_start".to_string(),
            source: None,
            call_id: None,
            tool_name: None,
            tool_input_summary: None,
            source_file: PathBuf::from("/workspace/cake/.cake/hooks/session-start.sh"),
            command: "./session-start.sh".to_string(),
            exit_code: None,
            duration_ms: 17,
            decision: "none".to_string(),
            resolved_decision: Some("none".to_string()),
            fail_closed: true,
            stdout: String::new(),
            stderr: "no exit code".to_string(),
        });

        insta::assert_json_snapshot!(
            "session_json_hook_event_without_optional_fields",
            session_record_json(record)
        );
    }

    #[test]
    fn deserialize_legacy_hook_event_without_correlation_fields() {
        let record: SessionRecord = serde_json::from_value(serde_json::json!({
            "type": "hook_event",
            "timestamp": "2026-05-10T12:34:56Z",
            "task_id": fixed_task_id(),
            "event": "PostToolUse",
            "source": "Bash",
            "source_file": "/workspace/cake/.cake/hooks/post-tool-use.sh",
            "command": "./post-tool-use.sh",
            "exit_code": 0,
            "duration_ms": 42,
            "decision": "none",
            "fail_closed": false,
            "stdout": "ok",
            "stderr": ""
        }))
        .unwrap();

        match record {
            SessionRecord::HookEvent(HookEventData {
                call_id,
                tool_name,
                tool_input_summary,
                resolved_decision,
                ..
            }) => {
                assert!(call_id.is_none());
                assert!(tool_name.is_none());
                assert!(tool_input_summary.is_none());
                assert!(resolved_decision.is_none());
            },
            other => panic!("expected hook_event, got {other:?}"),
        }
    }
}
