use super::*;
use crate::types::compaction::test_support::{
    as_skill_read, as_user_input, checkpoint, exchange, message, reasoning,
};
use crate::types::{ConversationItem, ReplaySafety, SessionRecord};

fn deserialize_record(json: &str) -> Result<SessionRecord, String> {
    serde_json::from_str::<SessionRecord>(json).map_err(|error| error.to_string())
}

fn checkpoint_json(data: &CompactionData) -> serde_json::Value {
    serde_json::to_value(SessionRecord::Compaction(data.clone())).expect("record should serialize")
}

fn user_message(content: &str) -> ConversationItem {
    ConversationItem::Message {
        role: Role::User,
        content: content.to_string(),
        id: None,
        status: None,
        timestamp: None,
        images: Vec::new(),
    }
}

fn developer_message(content: &str) -> ConversationItem {
    ConversationItem::Message {
        role: Role::Developer,
        content: content.to_string(),
        id: None,
        status: None,
        timestamp: None,
        images: Vec::new(),
    }
}

fn contents(items: &[ConversationItem]) -> Vec<String> {
    items
        .iter()
        .map(|item| match item {
            ConversationItem::Message { content, .. } => content.clone(),
            ConversationItem::Reasoning { .. } => "<reasoning>".to_string(),
            ConversationItem::FunctionCall { call_id, .. } => format!("call:{call_id}"),
            ConversationItem::FunctionCallOutput { call_id, .. } => format!("output:{call_id}"),
        })
        .collect()
}

fn summary_item(summary: &str) -> String {
    format!("{HISTORICAL_SUMMARY_MARKER}\n\n{summary}")
}

fn provenance_checkpoint() -> CompactionData {
    let mut first_exchange = exchange("call-1", "skill instructions");
    let call = first_exchange.remove(0);
    let mut output = first_exchange.remove(0);
    let RetainedConversationRecord::FunctionCallOutput(data) = &mut output.record else {
        unreachable!()
    };
    data.replay = Some(ReplaySafety::Safe);
    let retained = vec![
        as_user_input(message(Role::User, "do the thing")),
        reasoning("r-1"),
        call,
        as_skill_read(output, "debugging-cake"),
    ];
    checkpoint("checkpoint-1", "earlier work", retained)
}

fn assert_provenance_json(json: &serde_json::Value) {
    assert_eq!(json["type"], "compaction");
    assert_eq!(json["checkpoint_version"], CHECKPOINT_VERSION);
    assert_eq!(json["retained"][0]["type"], "message");
    assert_eq!(json["retained"][0]["role"], "user");
    assert_eq!(json["retained"][0]["origin"], "user");
    assert_eq!(json["retained"][0]["pin"]["kind"], "user_input");
    assert_eq!(json["retained"][1]["type"], "reasoning");
    assert_eq!(json["retained"][2]["type"], "function_call");
    assert_eq!(json["retained"][3]["type"], "function_call_output");
    assert_eq!(json["retained"][3]["skill_read"]["name"], "debugging-cake");
    assert_eq!(json["retained"][3]["pin"]["kind"], "skill_instructions");
    assert_eq!(json["retained"][3]["pin"]["name"], "debugging-cake");
    assert_eq!(
        json["retained"][3]["replay"], "safe",
        "a retained output keeps its execution-time replay declaration"
    );
    assert!(json["retained"][3]["source_task"].is_string());
}

#[test]
fn compaction_checkpoint_round_trips_retained_provenance() {
    let json = checkpoint_json(&provenance_checkpoint());
    assert_provenance_json(&json);

    let parsed = deserialize_record(&json.to_string()).expect("checkpoint should load");
    let SessionRecord::Compaction(parsed) = parsed else {
        panic!("expected a compaction record");
    };
    assert_eq!(
        serde_json::to_value(SessionRecord::Compaction(parsed.clone())).unwrap(),
        json,
        "round trip must preserve every retained field"
    );
    assert_eq!(parsed.retained[0].origin, Some(MessageOrigin::User));
    assert_eq!(parsed.retained[0].pin, Some(PinReason::UserInput));
    assert_eq!(
        parsed.retained[3].skill_read,
        Some(SkillRead {
            name: "debugging-cake".to_string(),
            path: std::path::PathBuf::from("/skills/debugging-cake/SKILL.md"),
        })
    );
    assert!(parsed.retained[3].pin.is_some());
    assert!(parsed.retained[3].source_task.is_some());
    let RetainedConversationRecord::FunctionCallOutput(output) = &parsed.retained[3].record else {
        panic!("expected a retained output");
    };
    assert_eq!(
        output.replay,
        Some(ReplaySafety::Safe),
        "replay metadata survives the checkpoint round trip"
    );
}

#[test]
fn compaction_checkpoint_rejects_unknown_version() {
    let data = checkpoint("checkpoint-1", "summary", vec![]);
    let mut json = checkpoint_json(&data);
    json["checkpoint_version"] = serde_json::json!(CHECKPOINT_VERSION + 1);

    let error = deserialize_record(&json.to_string()).unwrap_err();
    assert!(
        error.contains(&format!(
            "Unsupported compaction checkpoint_version: {} (expected {CHECKPOINT_VERSION})",
            CHECKPOINT_VERSION + 1
        )),
        "unexpected error: {error}"
    );
}

#[test]
fn compaction_checkpoint_validation_rejects_unknown_version() {
    let mut data = checkpoint("checkpoint-1", "summary", vec![]);
    data.checkpoint_version = CHECKPOINT_VERSION + 1;

    let error = data.validate().unwrap_err().to_string();
    assert!(error.contains("Unsupported compaction checkpoint_version: 2"));
}

#[test]
fn compaction_checkpoint_rejects_blank_summary() {
    for summary in ["", "   \n\t"] {
        let data = checkpoint("checkpoint-1", summary, vec![]);
        let error = data.validate().unwrap_err().to_string();
        assert!(
            error.contains("Compaction checkpoint summary is empty"),
            "unexpected error for {summary:?}: {error}"
        );
    }
}

#[test]
fn compaction_checkpoint_rejects_incomplete_call_pairing() {
    let data = checkpoint(
        "checkpoint-1",
        "summary",
        vec![exchange("call-1", "out").remove(0)],
    );
    let error = data.validate().unwrap_err().to_string();
    assert!(
        error.contains("retained a function call 'call-1' without its output"),
        "unexpected error: {error}"
    );

    let data = checkpoint(
        "checkpoint-1",
        "summary",
        vec![exchange("call-1", "out").remove(1)],
    );
    let error = data.validate().unwrap_err().to_string();
    assert!(
        error.contains("function call output for call_id 'call-1' with no preceding unmatched"),
        "unexpected error: {error}"
    );

    let mut retained = exchange("call-1", "out");
    retained.push(exchange("call-2", "out").remove(0));
    let data = checkpoint("checkpoint-1", "summary", retained);
    let error = data.validate().unwrap_err().to_string();
    assert!(
        error.contains("retained a function call 'call-2' without its output"),
        "unexpected error: {error}"
    );

    let mut duplicate = exchange("call-1", "out").remove(0);
    let RetainedConversationRecord::FunctionCall(call) = &mut duplicate.record else {
        unreachable!()
    };
    call.call_id = "call-1".to_string();
    let data = checkpoint(
        "checkpoint-1",
        "summary",
        vec![exchange("call-1", "out").remove(0), duplicate],
    );
    let error = data.validate().unwrap_err().to_string();
    assert!(
        error.contains("two unfinished function calls sharing call_id 'call-1'"),
        "unexpected error: {error}"
    );
}

#[test]
fn compaction_checkpoint_accepts_a_reused_call_id_after_its_output() {
    let mut data = checkpoint("checkpoint-1", "summary", exchange("call-1", "first"));
    data.retained.extend(exchange("call-1", "second"));

    data.validate()
        .expect("a call id reused after its completed pair is valid");
}

#[test]
fn compaction_checkpoint_rejects_nonconversation_records() {
    let data = checkpoint("checkpoint-1", "summary", vec![]);
    let mut json = checkpoint_json(&data);
    for kind in [
        "task_start",
        "task_complete",
        "turn_usage",
        "prompt_context",
        "hook_event",
        "compaction",
        "skill_activated",
    ] {
        json["retained"] = serde_json::json!([{ "type": kind }]);
        let error = deserialize_record(&json.to_string()).unwrap_err();
        assert!(
            error.contains("unknown variant") || error.contains("missing field"),
            "retained {kind} should be rejected, got: {error}"
        );
    }
}

#[test]
fn compaction_checkpoint_rejects_prompt_roles_and_misplaced_provenance() {
    for role in [Role::System, Role::Tool] {
        let data = checkpoint("checkpoint-1", "summary", vec![message(role, "prompt")]);
        let error = data.validate().unwrap_err().to_string();
        assert!(
            error.contains("retained messages must be user, developer, or assistant"),
            "unexpected error for {role:?}: {error}"
        );
    }

    let mut origin_on_output = exchange("call-1", "out").remove(1);
    origin_on_output.origin = Some(MessageOrigin::Synthetic);
    let data = checkpoint("checkpoint-1", "summary", vec![origin_on_output]);
    let error = data.validate().unwrap_err().to_string();
    assert!(
        error.contains("origin is only valid on a retained message"),
        "unexpected error: {error}"
    );

    let mut skill_on_message = message(Role::User, "read a skill");
    skill_on_message.skill_read = Some(SkillRead {
        name: "debugging-cake".to_string(),
        path: std::path::PathBuf::from("/skills/debugging-cake/SKILL.md"),
    });
    let data = checkpoint("checkpoint-1", "summary", vec![skill_on_message]);
    let error = data.validate().unwrap_err().to_string();
    assert!(
        error.contains("skill_read is only valid on a retained function call output"),
        "unexpected error: {error}"
    );
}

#[test]
fn compaction_checkpoint_rejects_misplaced_pins() {
    let mut assistant_input = message(Role::Assistant, "answer");
    assistant_input.pin = Some(PinReason::UserInput);
    let data = checkpoint("checkpoint-1", "summary", vec![assistant_input]);
    let error = data.validate().unwrap_err().to_string();
    assert!(
        error.contains("user_input pin is only valid on a retained user message"),
        "unexpected error: {error}"
    );

    let mut skill_pin_on_message = message(Role::User, "read a skill");
    skill_pin_on_message.pin = Some(PinReason::SkillInstructions {
        name: "debugging-cake".to_string(),
        path: std::path::PathBuf::from("/skills/debugging-cake/SKILL.md"),
    });
    let data = checkpoint("checkpoint-1", "summary", vec![skill_pin_on_message]);
    let error = data.validate().unwrap_err().to_string();
    assert!(
        error.contains("skill_instructions pin is only valid on a retained function call output"),
        "unexpected error: {error}"
    );

    let mut skill_pin_without_read = exchange("call-1", "out").remove(1);
    skill_pin_without_read.pin = Some(PinReason::SkillInstructions {
        name: "debugging-cake".to_string(),
        path: std::path::PathBuf::from("/skills/debugging-cake/SKILL.md"),
    });
    let data = checkpoint(
        "checkpoint-1",
        "summary",
        vec![exchange("call-1", "out").remove(0), skill_pin_without_read],
    );
    let error = data.validate().unwrap_err().to_string();
    assert!(
        error.contains("skill_instructions pin must match the retained output's skill_read"),
        "unexpected error: {error}"
    );

    let mut mismatched_skill = as_skill_read(exchange("call-1", "out").remove(1), "debugging-cake");
    mismatched_skill.pin = Some(PinReason::SkillInstructions {
        name: "other-skill".to_string(),
        path: std::path::PathBuf::from("/skills/other-skill/SKILL.md"),
    });
    let data = checkpoint(
        "checkpoint-1",
        "summary",
        vec![exchange("call-1", "out").remove(0), mismatched_skill],
    );
    let error = data.validate().unwrap_err().to_string();
    assert!(
        error.contains("skill_instructions pin must match the retained output's skill_read"),
        "unexpected error: {error}"
    );

    let mut image_pin_on_reasoning = reasoning("r-1");
    image_pin_on_reasoning.pin = Some(PinReason::UnconsumedImage);
    let data = checkpoint("checkpoint-1", "summary", vec![image_pin_on_reasoning]);
    let error = data.validate().unwrap_err().to_string();
    assert!(
        error.contains("unconsumed_image pin is only valid on a retained message"),
        "unexpected error: {error}"
    );

    let mut image_pin_without_images = message(Role::User, "text only");
    image_pin_without_images.pin = Some(PinReason::UnconsumedImage);
    let data = checkpoint("checkpoint-1", "summary", vec![image_pin_without_images]);
    let error = data.validate().unwrap_err().to_string();
    assert!(
        error.contains("unconsumed_image pin is only valid on a retained message"),
        "unexpected error: {error}"
    );
}

#[test]
fn compaction_checkpoint_accepts_correctly_placed_pins() {
    let pinned_user = as_user_input(message(Role::User, "do the thing"));
    let mut skill_exchange = exchange("call-1", "out");
    let skill_output = as_skill_read(skill_exchange.pop().unwrap(), "debugging-cake");
    let mut image_message = message(Role::User, "what is this?");
    let RetainedConversationRecord::Message(data) = &mut image_message.record else {
        unreachable!()
    };
    data.images.push(test_image());
    image_message.pin = Some(PinReason::UnconsumedImage);

    let mut retained = exchange("call-2", "image out");
    let RetainedConversationRecord::FunctionCallOutput(data) = &mut retained[1].record else {
        unreachable!()
    };
    data.images.push(test_image());
    retained[1].pin = Some(PinReason::UnconsumedImage);

    let mut records = vec![pinned_user, image_message];
    records.extend(retained);
    records.extend(skill_exchange);
    records.push(skill_output);
    let data = checkpoint("checkpoint-1", "summary", records);

    data.validate()
        .expect("every pin sits on the payload kind it describes");
}

fn test_image() -> crate::types::ImagePart {
    crate::types::ImagePart {
        media_type: "image/png".to_string(),
        data_base64: "iVBORw0KGgo=".to_string(),
    }
}

#[test]
fn compaction_checkpoint_rejects_a_record_between_a_call_and_its_output() {
    let mut call_and_output = exchange("call-1", "out");
    let output = call_and_output.pop().unwrap();
    let call = call_and_output.pop().unwrap();

    for interleaved in [
        message(Role::User, "a new request mid-call"),
        reasoning("r-between"),
    ] {
        let data = checkpoint(
            "checkpoint-1",
            "summary",
            vec![call.clone(), interleaved, output.clone()],
        );
        let error = data.validate().unwrap_err().to_string();
        assert!(
            error.contains("while function call 'call-1' still awaits its output"),
            "unexpected error: {error}"
        );
    }

    // Two parallel calls may open together and their outputs close in any order.
    let first_output = exchange("call-1", "first").remove(1);
    let first_call = exchange("call-1", "first").remove(0);
    let second_exchange = exchange("call-2", "second");
    let mut retained = vec![first_call, second_exchange[0].clone()];
    retained.push(second_exchange[1].clone());
    retained.push(first_output);
    let data = checkpoint("checkpoint-1", "summary", retained);
    data.validate().expect("parallel calls close in any order");
}

#[test]
fn compaction_checkpoint_rejects_empty_identity_fields() {
    for field in ["checkpoint_id", "session_id", "task_id"] {
        let mut data = checkpoint("checkpoint-1", "summary", vec![]);
        match field {
            "checkpoint_id" => data.checkpoint_id = "  ".to_string(),
            "session_id" => data.session_id = String::new(),
            "task_id" => data.task_id = "\t".to_string(),
            _ => unreachable!(),
        }
        let error = data.validate().unwrap_err().to_string();
        assert!(
            error.contains(&format!("Compaction checkpoint {field} is empty")),
            "unexpected error for {field}: {error}"
        );
    }
}

#[test]
fn compaction_checkpoint_rejects_a_record_over_the_size_ceiling() {
    let oversized = "x".repeat(MAX_CHECKPOINT_RECORD_BYTES + 1024);
    let data = checkpoint("checkpoint-1", &oversized, vec![]);
    let error = data.validate().unwrap_err().to_string();
    assert!(
        error.contains("the limit is 8388608 bytes"),
        "unexpected error: {error}"
    );

    let within_budget = checkpoint("checkpoint-1", &"x".repeat(4096), vec![]);
    within_budget
        .validate()
        .expect("a small checkpoint should validate");
}

#[test]
fn compaction_checkpoint_size_limit_measures_the_framed_record() {
    let mut data = checkpoint("checkpoint-1", "x", vec![]);
    let framed = data.to_record_bytes().unwrap();
    assert!(framed.ends_with(b"\n"), "the record includes its newline");
    assert_eq!(
        framed,
        {
            let mut expected =
                serde_json::to_vec(&SessionRecord::Compaction(data.clone())).unwrap();
            expected.push(b'\n');
            expected
        },
        "the measured bytes are exactly the framed session record"
    );

    // The ceiling applies to the complete framed record, not the payload.
    data.summary = "x".repeat(MAX_CHECKPOINT_RECORD_BYTES - framed.len() + 1);
    let exact = data
        .to_record_bytes()
        .expect("the exact ceiling is allowed");
    assert_eq!(exact.len(), MAX_CHECKPOINT_RECORD_BYTES);

    data.summary.push('x');
    let error = data.to_record_bytes().unwrap_err().to_string();
    assert!(
        error.contains(&format!(
            "record is {} bytes; the limit is {MAX_CHECKPOINT_RECORD_BYTES} bytes",
            MAX_CHECKPOINT_RECORD_BYTES + 1
        )),
        "unexpected error: {error}"
    );
}

#[test]
fn compaction_checkpoint_rejects_unknown_envelope_fields() {
    let data = checkpoint("checkpoint-1", "summary", vec![]);
    let mut json = checkpoint_json(&data);
    json["api_key"] = serde_json::json!("sk-secret");

    let error = deserialize_record(&json.to_string()).unwrap_err();
    assert!(error.contains("unknown field `api_key`"), "{error}");
}

#[test]
fn compaction_projection_replaces_retired_history_once() {
    let retained = vec![
        as_user_input(message(Role::User, "retired request")),
        reasoning("r-1"),
    ];
    let checkpoint = checkpoint("checkpoint-1", "retired work summary", retained);
    let history = vec![
        ConversationItem::Message {
            role: Role::System,
            content: "fresh system prompt".to_string(),
            id: None,
            status: None,
            timestamp: None,
            images: Vec::new(),
        },
        developer_message("fresh developer context"),
        user_message("retired request"),
        user_message("new request"),
    ];

    let projected = project_context(&history, Some(&checkpoint), 3);

    assert_eq!(
        contents(&projected),
        vec![
            "fresh system prompt".to_string(),
            "fresh developer context".to_string(),
            summary_item("retired work summary"),
            "retired request".to_string(),
            "<reasoning>".to_string(),
            "new request".to_string(),
        ],
        "fresh prompts survive, retired raw records appear only through the checkpoint"
    );
    assert!(matches!(
        &projected[2],
        ConversationItem::Message {
            role: Role::User,
            ..
        }
    ));
    assert_eq!(contents(&history).len(), 4, "raw history is unchanged");
}

#[test]
fn compaction_projection_without_checkpoint_is_raw_history() {
    let history = vec![
        user_message("first"),
        user_message("second"),
        developer_message("context"),
    ];

    let projected = project_context(&history, None, 0);

    assert_eq!(contents(&projected), contents(&history));
}

#[test]
fn compaction_projection_keeps_repeated_call_ids_and_source_order() {
    let checkpoint = checkpoint(
        "checkpoint-1",
        "summary",
        exchange("call-1", "first result"),
    );
    let mut history = vec![
        ConversationItem::Message {
            role: Role::System,
            content: "system".to_string(),
            id: None,
            status: None,
            timestamp: None,
            images: Vec::new(),
        },
        ConversationItem::FunctionCall {
            id: "fc-old".to_string(),
            call_id: "call-1".to_string(),
            name: "Read".to_string(),
            arguments: "{}".to_string(),
            timestamp: None,
        },
        ConversationItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: "first result".to_string(),
            timestamp: None,
            images: Vec::new(),
        },
    ];
    // A later provider turn may legitimately reuse `call-1`.
    history.extend([
        ConversationItem::FunctionCall {
            id: "fc-new".to_string(),
            call_id: "call-1".to_string(),
            name: "Read".to_string(),
            arguments: "{}".to_string(),
            timestamp: None,
        },
        ConversationItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: "second result".to_string(),
            timestamp: None,
            images: Vec::new(),
        },
    ]);
    history.push(user_message("new user input"));

    let projected = project_context(&history, Some(&checkpoint), 3);

    assert_eq!(
        contents(&projected),
        vec![
            "system".to_string(),
            summary_item("summary"),
            "call:call-1".to_string(),
            "output:call-1".to_string(),
            "call:call-1".to_string(),
            "output:call-1".to_string(),
            "new user input".to_string(),
        ],
        "the retained exchange and the post-boundary exchange both keep call-1"
    );
    assert_eq!(history.len(), 6, "raw offsets do not change");
}

#[test]
fn compaction_projection_keeps_fresh_prompts_at_a_boundary_of_zero() {
    let checkpoint = checkpoint("checkpoint-1", "summary", vec![]);
    let history = vec![
        ConversationItem::Message {
            role: Role::System,
            content: "fresh system prompt".to_string(),
            id: None,
            status: None,
            timestamp: None,
            images: Vec::new(),
        },
        developer_message("fresh developer context"),
        user_message("after"),
    ];

    for boundary in [0, 1] {
        let projected = project_context(&history, Some(&checkpoint), boundary);
        assert_eq!(
            contents(&projected),
            vec![
                "fresh system prompt".to_string(),
                "fresh developer context".to_string(),
                summary_item("summary"),
                "after".to_string(),
            ],
            "boundary {boundary}: fresh prompts stay at the head exactly once"
        );
    }
}

#[test]
fn compaction_projection_keeps_a_boundary_of_zero_in_source_order() {
    let checkpoint = checkpoint("checkpoint-1", "summary", vec![]);
    let history = vec![user_message("after")];

    let projected = project_context(&history, Some(&checkpoint), 0);

    assert_eq!(
        contents(&projected),
        vec![summary_item("summary"), "after".to_string()]
    );
}

#[test]
fn compaction_context_seed_default_is_empty() {
    let seed = ContextSeed::default();

    assert!(seed.messages.is_empty());
    assert!(seed.checkpoint.is_none());
    assert!(seed.boundary.is_none());
}

#[test]
fn compaction_trigger_and_provenance_values_are_stable() {
    assert_eq!(
        serde_json::to_value(CompactionTrigger::Overflow).unwrap(),
        serde_json::json!("overflow")
    );
    assert_eq!(
        serde_json::to_value(CompactionTrigger::Manual).unwrap(),
        serde_json::json!("manual")
    );
    assert_eq!(
        serde_json::to_value(MessageOrigin::Synthetic).unwrap(),
        serde_json::json!("synthetic")
    );
}
