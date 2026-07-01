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
fn task_outcome_serializes_interrupted() {
    let record = StreamRecord::TaskComplete(TaskCompleteData {
        outcome: TaskOutcome::Interrupted,
        duration_ms: 500,
        turn_count: 1,
        tool_call_count: 0,
        session_id: "session-1".to_string(),
        task_id: "task-1".to_string(),
        usage: Usage::default(),
        permission_denials: None,
    });

    let json = serde_json::to_value(&record).unwrap();
    assert_eq!(json["type"], "task_complete");
    assert_eq!(json["subtype"], "interrupted");
    assert_eq!(json["is_error"], true);
    assert!(json.get("result").is_none() || json["result"].is_null());
    assert!(json.get("error").is_none() || json["error"].is_null());
    assert!(json.get("success").is_none());
}

#[test]
fn task_outcome_deserializes_interrupted() {
    let json = serde_json::json!({
        "type": "task_complete",
        "subtype": "interrupted",
        "is_error": true,
        "duration_ms": 500,
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
            outcome: TaskOutcome::Interrupted,
            ..
        })
    ));
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
