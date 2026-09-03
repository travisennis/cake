use super::*;
use crate::config::session::CURRENT_FORMAT_VERSION;
use crate::types::conversation::{ReasoningContentKind, ReasoningSummary};
use crate::types::usage::{InputTokensDetails, OutputTokensDetails};

fn stream_json_for(item: &ConversationItem) -> serde_json::Value {
    serde_json::to_value(StreamRecord::from_conversation_item(item)).unwrap()
}

fn stream_json_for_with_replay(item: &ConversationItem, replay: ReplaySafety) -> serde_json::Value {
    serde_json::to_value(StreamRecord::from_conversation_item_with_replay(
        item,
        Some(replay),
    ))
    .unwrap()
}

fn session_json_for(item: &ConversationItem) -> serde_json::Value {
    let stream_record = StreamRecord::from_conversation_item(item);
    let session_record = SessionRecord::from(stream_record);
    serde_json::to_value(session_record).unwrap()
}

fn session_json_for_with_replay(
    item: &ConversationItem,
    replay: ReplaySafety,
) -> serde_json::Value {
    let stream_record = StreamRecord::from_conversation_item_with_replay(item, Some(replay));
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
        tool_input_summary: Some("just check".to_string()),
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
fn task_outcome_serializes_error_output_schema() {
    let record = StreamRecord::TaskComplete(TaskCompleteData {
        outcome: TaskOutcome::ErrorOutputSchema {
            error: "\"summary\" is a required property".to_string(),
        },
        duration_ms: 500,
        turn_count: 3,
        tool_call_count: 0,
        session_id: "session-1".to_string(),
        task_id: "task-1".to_string(),
        usage: Usage::default(),
        permission_denials: None,
    });

    let json = serde_json::to_value(&record).unwrap();
    assert_eq!(json["type"], "task_complete");
    assert_eq!(json["subtype"], "error_output_schema");
    assert_eq!(json["is_error"], true);
    assert_eq!(json["error"], "\"summary\" is a required property");
    assert!(json.get("result").is_none() || json["result"].is_null());
    assert!(json.get("success").is_none());
}

#[test]
fn task_outcome_deserializes_error_output_schema() {
    let json = serde_json::json!({
        "type": "task_complete",
        "subtype": "error_output_schema",
        "is_error": true,
        "error": "validation detail",
        "duration_ms": 500,
        "turn_count": 3,
        "tool_call_count": 0,
        "session_id": "session-1",
        "task_id": "task-1",
        "usage": Usage::default()
    });

    let record = serde_json::from_value::<StreamRecord>(json).unwrap();
    assert!(matches!(
        record,
        StreamRecord::TaskComplete(TaskCompleteData {
            outcome: TaskOutcome::ErrorOutputSchema { error },
            ..
        }) if error == "validation detail"
    ));
}

#[test]
fn task_outcome_error_output_schema_requires_error() {
    let json = serde_json::json!({
        "type": "task_complete",
        "subtype": "error_output_schema",
        "is_error": true,
        "duration_ms": 500,
        "turn_count": 3,
        "tool_call_count": 0,
        "session_id": "session-1",
        "task_id": "task-1",
        "usage": Usage::default()
    });

    let error = serde_json::from_value::<StreamRecord>(json).unwrap_err();
    assert!(error.to_string().contains("requires error"));
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
fn task_outcome_serializes_cut_off() {
    let record = StreamRecord::TaskComplete(TaskCompleteData {
        outcome: TaskOutcome::CutOff {
            detail: "The model's response was cut off during reasoning.".to_string(),
        },
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
    assert_eq!(json["subtype"], "cut_off");
    assert_eq!(json["is_error"], true);
    assert_eq!(
        json["error"],
        "The model's response was cut off during reasoning."
    );
    assert!(json.get("result").is_none() || json["result"].is_null());
    assert!(json.get("success").is_none());
}

#[test]
fn task_outcome_deserializes_cut_off() {
    let json = serde_json::json!({
        "type": "task_complete",
        "subtype": "cut_off",
        "is_error": true,
        "error": "The model's response was cut off during reasoning.",
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
            outcome: TaskOutcome::CutOff { detail },
            ..
        }) if detail == "The model's response was cut off during reasoning."
    ));
}

#[test]
fn task_outcome_cut_off_requires_error() {
    let json = serde_json::json!({
        "type": "task_complete",
        "subtype": "cut_off",
        "is_error": true,
        "duration_ms": 500,
        "turn_count": 1,
        "tool_call_count": 0,
        "session_id": "session-1",
        "task_id": "task-1",
        "usage": Usage::default()
    });

    let err = serde_json::from_value::<StreamRecord>(json).unwrap_err();
    assert!(err.to_string().contains("requires error"));
}

#[test]
fn task_outcome_serializes_limit_exceeded() {
    let record = StreamRecord::TaskComplete(TaskCompleteData {
        outcome: TaskOutcome::LimitExceeded {
            limit: "max_turns".to_string(),
            detail: "max_turns limit exceeded after 5 turns (max_turns = 5)".to_string(),
            result: Some("partial work".to_string()),
        },
        duration_ms: 500,
        turn_count: 5,
        tool_call_count: 3,
        session_id: "session-1".to_string(),
        task_id: "task-1".to_string(),
        usage: Usage::default(),
        permission_denials: None,
    });

    let json = serde_json::to_value(&record).unwrap();
    assert_eq!(json["type"], "task_complete");
    assert_eq!(json["subtype"], "limit_exceeded");
    assert_eq!(json["is_error"], true);
    assert_eq!(json["limit"], "max_turns");
    assert_eq!(
        json["error"],
        "max_turns limit exceeded after 5 turns (max_turns = 5)"
    );
    assert_eq!(json["result"], "partial work");
    assert!(json.get("success").is_none());
}

#[test]
fn task_outcome_deserializes_limit_exceeded() {
    let json = serde_json::json!({
        "type": "task_complete",
        "subtype": "limit_exceeded",
        "is_error": true,
        "limit": "max_tool_calls",
        "error": "max_tool_calls limit exceeded after 2 tool calls (max_tool_calls = 3)",
        "result": "partial work",
        "duration_ms": 500,
        "turn_count": 2,
        "tool_call_count": 2,
        "session_id": "session-1",
        "task_id": "task-1",
        "usage": Usage::default()
    });

    let record = serde_json::from_value::<StreamRecord>(json).unwrap();
    assert!(matches!(
        record,
        StreamRecord::TaskComplete(TaskCompleteData {
            outcome: TaskOutcome::LimitExceeded { limit, detail, result },
            ..
        }) if limit == "max_tool_calls"
            && detail == "max_tool_calls limit exceeded after 2 tool calls (max_tool_calls = 3)"
            && result.as_deref() == Some("partial work")
    ));
}

#[test]
fn task_outcome_limit_exceeded_requires_limit_and_error() {
    let json = serde_json::json!({
        "type": "task_complete",
        "subtype": "limit_exceeded",
        "is_error": true,
        "duration_ms": 500,
        "turn_count": 1,
        "tool_call_count": 0,
        "session_id": "session-1",
        "task_id": "task-1",
        "usage": Usage::default()
    });

    let err = serde_json::from_value::<StreamRecord>(json).unwrap_err();
    assert!(err.to_string().contains("requires limit"));

    let json = serde_json::json!({
        "type": "task_complete",
        "subtype": "limit_exceeded",
        "is_error": true,
        "limit": "max_turns",
        "duration_ms": 500,
        "turn_count": 1,
        "tool_call_count": 0,
        "session_id": "session-1",
        "task_id": "task-1",
        "usage": Usage::default()
    });

    let err = serde_json::from_value::<StreamRecord>(json).unwrap_err();
    assert!(err.to_string().contains("requires error"));
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
fn stream_record_json_reasoning_uses_typed_summary() {
    let item = ConversationItem::Reasoning {
        id: "r-1".to_string(),
        summary: Some(vec![ReasoningSummary::summary_text("step 1")]),
        encrypted_content: None,
        content: None,
        timestamp: None,
    };
    let json = stream_json_for(&item);
    assert_eq!(json["type"], "reasoning");
    assert_eq!(json["summary"][0]["type"], "summary_text");
    assert_eq!(json["summary"][0]["text"], "step 1");
}

#[test]
fn stream_record_json_reasoning_loads_legacy_string_summary() {
    let record: StreamRecord = serde_json::from_value(serde_json::json!({
        "type": "reasoning",
        "id": "r-legacy",
        "summary": ["old provider summary"],
        "timestamp": "2026-05-10T12:34:56Z"
    }))
    .unwrap();

    let item = SessionRecord::from(record).to_conversation_item().unwrap();
    let ConversationItem::Reasoning { summary, .. } = item else {
        panic!("expected reasoning conversation item");
    };
    assert_eq!(
        summary.unwrap()[0],
        ReasoningSummary::summary_text("old provider summary")
    );
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
fn unknown_persisted_replay_declarations_fail_closed() {
    let function_call: SessionRecord = serde_json::from_value(serde_json::json!({
        "type": "function_call",
        "id": "fc-1",
        "call_id": "call-1",
        "name": "Read",
        "arguments": "{}",
        "replay": "future-value"
    }))
    .unwrap();
    let SessionRecord::FunctionCall(data) = function_call else {
        panic!("expected function call record");
    };
    assert_eq!(data.replay, None);

    let function_call_output: SessionRecord = serde_json::from_value(serde_json::json!({
        "type": "function_call_output",
        "call_id": "call-1",
        "output": "result",
        "replay": { "unexpected": true }
    }))
    .unwrap();
    let SessionRecord::FunctionCallOutput(data) = function_call_output else {
        panic!("expected function call output record");
    };
    assert_eq!(data.replay, None);
}

#[test]
fn unknown_stream_replay_declaration_fails_closed() {
    let record: StreamRecord = serde_json::from_value(serde_json::json!({
        "type": "function_call",
        "id": "fc-1",
        "call_id": "call-1",
        "name": "Read",
        "arguments": "{}",
        "replay": "future-value"
    }))
    .unwrap();
    let StreamRecord::FunctionCall(data) = record else {
        panic!("expected function call stream record");
    };
    assert_eq!(data.replay, None);
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
fn stream_record_json_function_call_valid_arguments_has_no_parse_error() {
    let item = ConversationItem::FunctionCall {
        id: "fc-1".to_string(),
        call_id: "call-1".to_string(),
        name: "bash".to_string(),
        arguments: r#"{"cmd":"ls"}"#.to_string(),
        timestamp: None,
    };
    let json = stream_json_for(&item);
    assert!(json.get("arguments_parse_error").is_none());
}

#[test]
fn stream_record_json_function_call_malformed_arguments_has_parse_error() {
    let item = ConversationItem::FunctionCall {
        id: "fc-1".to_string(),
        call_id: "call-1".to_string(),
        name: "Edit".to_string(),
        arguments: r#"{"edits": [{"new_text": "x"}],<"#.to_string(),
        timestamp: None,
    };
    let json = stream_json_for(&item);
    assert_eq!(json["type"], "function_call");
    assert!(json.get("arguments_parse_error").is_some());
    let error = json["arguments_parse_error"].as_str().unwrap();
    assert!(
        !error.is_empty(),
        "arguments_parse_error should be non-empty"
    );

    // The enclosing StreamRecord must remain valid JSON even though the
    // nested arguments string is not.
    let line = serde_json::to_string(&StreamRecord::from_conversation_item(&item)).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(parsed["arguments"], r#"{"edits": [{"new_text": "x"}],<"#);
}

#[test]
fn function_call_hostile_argument_content_roundtrips() {
    // Valid JSON containing quotes, backslashes, newlines, and a control char.
    let arguments = serde_json::json!({
        "text": "hello\nworld\t\"quoted\"\\backslash\u{0001}"
    })
    .to_string();
    let item = ConversationItem::FunctionCall {
        id: "fc-1".to_string(),
        call_id: "call-1".to_string(),
        name: "bash".to_string(),
        arguments,
        timestamp: Some(timestamp_at("2026-05-10T00:00:00Z")),
    };
    assert_conversation_item_stream_session_roundtrip(&item);

    // Stream-json line is valid JSON and the nested arguments parse cleanly.
    let line = serde_json::to_string(&StreamRecord::from_conversation_item(&item)).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert!(parsed.get("arguments_parse_error").is_none());
    let nested: serde_json::Value =
        serde_json::from_str(parsed["arguments"].as_str().unwrap()).unwrap();
    assert_eq!(
        nested["text"],
        "hello\nworld\t\"quoted\"\\backslash\u{0001}"
    );
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
            summary: Some(vec![ReasoningSummary::summary_text("step 1")]),
            encrypted_content: Some("gAAAAABencrypted...".to_string()),
            content: None,
            timestamp: Some(timestamp_at("2026-05-10T00:00:04Z")),
        },
        ConversationItem::Reasoning {
            id: "reasoning-content".to_string(),
            summary: Some(vec![
                ReasoningSummary::summary_text("step 1"),
                ReasoningSummary::summary_text("step 2"),
            ]),
            encrypted_content: None,
            content: Some(vec![ReasoningContent {
                content_type: ReasoningContentKind::ReasoningText,
                text: Some("deep analysis".to_string()),
            }]),
            timestamp: Some(timestamp_at("2026-05-10T00:00:05Z")),
        },
        ConversationItem::Reasoning {
            id: "reasoning-both".to_string(),
            summary: Some(vec![ReasoningSummary::summary_text("step 1")]),
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
        summary: Some(vec![
            ReasoningSummary::summary_text("step 1"),
            ReasoningSummary::summary_text("step 2"),
        ]),
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
fn snapshot_stream_record_json_function_call_with_replay() {
    let item = ConversationItem::FunctionCall {
        id: "fc-1".to_string(),
        call_id: "call-1".to_string(),
        name: "Read".to_string(),
        arguments: r#"{"path":"README.md"}"#.to_string(),
        timestamp: Some(timestamp_at("2026-05-10T00:00:00Z")),
    };
    insta::assert_json_snapshot!(
        "stream_record_json_function_call_with_replay",
        stream_json_for_with_replay(&item, ReplaySafety::Safe)
    );
}

#[test]
fn snapshot_session_json_function_call_output_with_replay() {
    let item = ConversationItem::FunctionCallOutput {
        call_id: "call-1".to_string(),
        output: "result".to_string(),
        timestamp: Some(timestamp_at("2026-05-10T00:00:00Z")),
    };
    insta::assert_json_snapshot!(
        "session_json_function_call_output_with_replay",
        session_json_for_with_replay(&item, ReplaySafety::Safe)
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
        summary: Some(vec![ReasoningSummary::summary_text("step 1")]),
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
fn snapshot_session_json_function_call_with_replay() {
    let item = ConversationItem::FunctionCall {
        id: "fc-1".to_string(),
        call_id: "call-1".to_string(),
        name: "Read".to_string(),
        arguments: r#"{"path":"README.md"}"#.to_string(),
        timestamp: Some(timestamp_at("2026-05-10T00:00:00Z")),
    };
    insta::assert_json_snapshot!(
        "session_json_function_call_with_replay",
        session_json_for_with_replay(&item, ReplaySafety::Safe)
    );
}

#[test]
fn snapshot_session_json_session_meta() {
    let record = SessionRecord::SessionMeta {
        format_version: CURRENT_FORMAT_VERSION,
        session_id: fixed_session_id(),
        timestamp: fixed_timestamp(),
        working_directory: PathBuf::from("/workspace/cake"),
        model: Some("gpt-5.4".to_string()),
        model_config: Some("test".to_string()),
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
            input_tokens_details: InputTokensDetails {
                cached_tokens: 25,
                cache_write_tokens: 0,
            },
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
fn snapshot_session_json_turn_usage() {
    let record = SessionRecord::TurnUsage(TurnUsageData {
        session_id: fixed_session_id(),
        task_id: fixed_task_id(),
        turn: 2,
        usage: Usage {
            input_tokens: 1200,
            input_tokens_details: InputTokensDetails {
                cached_tokens: 300,
                cache_write_tokens: 0,
            },
            output_tokens: 100,
            output_tokens_details: OutputTokensDetails {
                reasoning_tokens: 40,
            },
            total_tokens: 1300,
        },
        timestamp: fixed_timestamp(),
        attempt: Some(2),
        terminal_class: Some(ApiAttemptTerminalClass::ResponseFailed),
    });

    insta::assert_json_snapshot!("session_json_turn_usage", session_record_json(record));
}

#[test]
fn legacy_turn_usage_shape_deserializes_without_attempt_metadata() {
    let record = serde_json::from_value::<SessionRecord>(serde_json::json!({
        "type": "turn_usage",
        "session_id": fixed_session_id(),
        "task_id": fixed_task_id(),
        "turn": 1,
        "usage": {
            "input_tokens": 10,
            "input_tokens_details": {
                "cached_tokens": 0,
                "cache_write_tokens": 0
            },
            "output_tokens": 5,
            "output_tokens_details": {
                "reasoning_tokens": 0
            },
            "total_tokens": 15
        },
        "timestamp": "2026-05-10T12:34:56Z"
    }))
    .unwrap();

    assert!(matches!(
        record,
        SessionRecord::TurnUsage(TurnUsageData {
            attempt: None,
            terminal_class: None,
            ..
        })
    ));
}

#[test]
fn turn_usage_without_attempt_metadata_keeps_legacy_shape() {
    let record = SessionRecord::TurnUsage(TurnUsageData {
        session_id: fixed_session_id(),
        task_id: fixed_task_id(),
        turn: 1,
        usage: Usage::default(),
        timestamp: fixed_timestamp(),
        attempt: None,
        terminal_class: None,
    });
    let json = session_record_json(record);

    assert!(json.get("attempt").is_none());
    assert!(json.get("terminal_class").is_none());
}

#[test]
fn snapshot_session_json_limit_exceeded() {
    let record = SessionRecord::TaskComplete(TaskCompleteData {
        outcome: TaskOutcome::LimitExceeded {
            limit: "max_turns".to_string(),
            detail: "max_turns limit exceeded after 5 turns (max_turns = 5)".to_string(),
            result: Some("I'll inspect the workspace.".to_string()),
        },
        duration_ms: 1_250,
        turn_count: 5,
        tool_call_count: 3,
        session_id: fixed_session_id(),
        task_id: fixed_task_id(),
        usage: Usage {
            input_tokens: 100,
            input_tokens_details: InputTokensDetails {
                cached_tokens: 25,
                cache_write_tokens: 0,
            },
            output_tokens: 50,
            output_tokens_details: OutputTokensDetails {
                reasoning_tokens: 10,
            },
            total_tokens: 150,
        },
        permission_denials: None,
    });

    insta::assert_json_snapshot!("session_json_limit_exceeded", session_record_json(record));
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

#[test]
fn snapshot_stream_record_json_session_meta() {
    let record = StreamRecord::SessionMeta {
        format_version: CURRENT_FORMAT_VERSION,
        session_id: fixed_session_id(),
        timestamp: fixed_timestamp(),
        working_directory: PathBuf::from("/workspace/cake"),
        model: Some("gpt-5.4".to_string()),
        model_config: Some("test".to_string()),
        tools: vec!["bash".to_string(), "read".to_string(), "edit".to_string()],
        cake_version: Some("1.2.3-test".to_string()),
        system_prompt: Some("You are cake.".to_string()),
        git: GitState {
            repository_url: Some("https://example.com/cake.git".to_string()),
            branch: Some("main".to_string()),
            commit_hash: Some("abcdef1234567890".to_string()),
        },
    };

    insta::assert_json_snapshot!(
        "stream_record_json_session_meta",
        serde_json::to_value(record).unwrap()
    );
}

#[test]
fn snapshot_stream_record_json_prompt_context() {
    let record = StreamRecord::PromptContext {
        session_id: fixed_session_id(),
        task_id: fixed_task_id(),
        role: Role::Developer,
        content: "Use the project instructions.".to_string(),
        timestamp: fixed_timestamp(),
    };

    insta::assert_json_snapshot!(
        "stream_record_json_prompt_context",
        serde_json::to_value(record).unwrap()
    );
}

#[test]
fn snapshot_stream_record_json_skill_activated() {
    let record = StreamRecord::SkillActivated {
        session_id: fixed_session_id(),
        task_id: fixed_task_id(),
        timestamp: fixed_timestamp(),
        name: "debugging-cake".to_string(),
        path: PathBuf::from("/workspace/cake/.agents/skills/debugging-cake/SKILL.md"),
    };

    insta::assert_json_snapshot!(
        "stream_record_json_skill_activated",
        serde_json::to_value(record).unwrap()
    );
}

#[test]
fn snapshot_stream_record_json_replay_error() {
    let record = StreamRecord::ReplayError {
        session_id: Some(fixed_session_id()),
        kind: ReplayErrorKind::SessionNotFound,
        error: "session not found: 550e8400-e29b-41d4-a716-446655440000".to_string(),
        exit_code: 3,
    };

    insta::assert_json_snapshot!(
        "stream_record_json_replay_error",
        serde_json::to_value(record).unwrap()
    );
}

#[test]
fn stream_record_replay_error_omits_unknown_session_id() {
    let record = StreamRecord::ReplayError {
        session_id: None,
        kind: ReplayErrorKind::InvalidUuid,
        error: "invalid session UUID 'nope'".to_string(),
        exit_code: 3,
    };

    let json = serde_json::to_value(record).unwrap();
    assert_eq!(json["type"], "replay_error");
    assert_eq!(json["kind"], "invalid_uuid");
    assert!(json.get("session_id").is_none());
}

#[test]
fn session_record_to_stream_record_preserves_json() {
    let session_records = [
        SessionRecord::SessionMeta {
            format_version: CURRENT_FORMAT_VERSION,
            session_id: fixed_session_id(),
            timestamp: fixed_timestamp(),
            working_directory: PathBuf::from("/workspace/cake"),
            model: Some("gpt-5.4".to_string()),
            model_config: Some("test".to_string()),
            tools: vec!["bash".to_string()],
            cake_version: Some("1.2.3-test".to_string()),
            system_prompt: Some("You are cake.".to_string()),
            git: GitState::default(),
        },
        SessionRecord::TaskStart(TaskStartData {
            session_id: fixed_session_id(),
            task_id: fixed_task_id(),
            timestamp: fixed_timestamp(),
        }),
        SessionRecord::PromptContext {
            session_id: fixed_session_id(),
            task_id: fixed_task_id(),
            role: Role::Developer,
            content: "mutable context".to_string(),
            timestamp: fixed_timestamp(),
        },
        SessionRecord::SkillActivated {
            session_id: fixed_session_id(),
            task_id: fixed_task_id(),
            timestamp: fixed_timestamp(),
            name: "debugging-cake".to_string(),
            path: PathBuf::from("/workspace/cake/.agents/skills/debugging-cake/SKILL.md"),
        },
    ];

    for record in session_records {
        let session_json = serde_json::to_value(&record).unwrap();
        let stream_json = serde_json::to_value(StreamRecord::from(record)).unwrap();
        assert_eq!(session_json, stream_json);
        let restored = serde_json::from_value::<StreamRecord>(stream_json).unwrap();
        assert_eq!(
            serde_json::to_value(SessionRecord::from(restored)).unwrap(),
            session_json
        );
    }
}
