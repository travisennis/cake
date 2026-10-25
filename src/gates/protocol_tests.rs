use serde_json::json;

use super::*;

fn describe_value() -> serde_json::Value {
    json!({
        "protocol": "cake-gate/1",
        "name": "safety",
        "version": "0.1.0",
        "phases": ["pre_tool"],
        "actions": ["bash.exec"],
        "transports": ["stdio"],
        "capabilities": ["shadow", "diagnostic", "resolved-models"]
    })
}

#[test]
fn describe_round_trips_and_accepts_unknown_fields() {
    let mut value = describe_value();
    value["future_field"] = json!({"anything": true});
    value["capabilities"] = json!(["shadow", "future-capability"]);
    let describe: Describe = serde_json::from_value(value).expect("describe parses");
    describe.validate().expect("describe validates");
    assert_eq!(describe.name, "safety");
    assert_eq!(describe.version, "0.1.0");
    assert!(describe.covers(GatePhase::PreTool, GateAction::BashExec));
    assert!(!describe.covers(GatePhase::PostTool, GateAction::BashExec));
    assert!(
        describe
            .capabilities
            .iter()
            .any(|value| value == "future-capability")
    );
}

#[test]
fn describe_validation_rejects_protocol_name_version_and_shape() {
    let mut value = describe_value();
    value["protocol"] = json!("cake-gate/2");
    let describe: Describe = serde_json::from_value(value).expect("describe parses");
    assert!(describe.validate().unwrap_err().contains("cake-gate/2"));

    for (field, replacement) in [
        ("name", json!("Safety")),
        ("version", json!("0.1")),
        ("transports", json!(["oneshot"])),
        ("phases", json!([])),
        ("phases", json!(["pre_tool", "pre_tool"])),
        ("actions", json!([])),
    ] {
        let mut value = describe_value();
        value[field] = replacement;
        let describe: Describe = serde_json::from_value(value).expect("describe parses");
        assert!(describe.validate().is_err(), "{field} should be rejected");
    }
}

#[test]
fn open_error_classes_normalize_unknown_values_to_protocol() {
    let class: GateErrorClass = serde_json::from_value(json!("future-class")).expect("open class");
    assert_eq!(class.as_str(), "future-class");
    assert_eq!(class.normalized(), GateErrorClass::Protocol);
    let class: GateErrorClass = serde_json::from_value(json!("rate_limit")).expect("known class");
    assert_eq!(class, GateErrorClass::RateLimit);
    assert_eq!(
        serde_json::to_value(GateErrorClass::Crash).expect("serializes"),
        json!("crash")
    );
}

#[test]
fn check_frame_uses_protocol_field_names_and_ignores_unknown_fields() {
    let mut value = json!({
        "protocol": "cake-gate/1",
        "type": "check",
        "id": "request-1",
        "phase": "pre_tool",
        "action": "bash.exec",
        "input": {
            "command": "git status --short",
            "cwd": "/repo",
            "reason": null,
            "repo_digest": "git repo, branch main",
            "script_evidence": []
        },
        "session": {"call_id": "call-1", "turn": 3},
        "options": {"diagnostic": false},
        "future_field": true
    });
    value["input"]["script_evidence"] = json!([
        {"path": "/repo/run.sh", "contents": "echo hi"}
    ]);
    let frame: CheckFrame = serde_json::from_value(value).expect("check parses");
    assert_eq!(frame.frame_type, FrameType::Check);
    assert_eq!(frame.action, GateAction::BashExec);
    assert_eq!(frame.phase, GatePhase::PreTool);
    assert_eq!(frame.input.script_evidence.len(), 1);
    assert_eq!(frame.session.as_ref().and_then(|s| s.turn), Some(3));
    let serialized = serde_json::to_value(&frame).expect("serializes");
    assert_eq!(serialized["type"], json!("check"));
    assert_eq!(serialized["action"], json!("bash.exec"));
}

#[test]
fn init_and_ready_frames_round_trip() {
    let init = InitFrame::new(InitSession {
        session_id: Some("session".to_string()),
        task_id: None,
        invocation_id: Some("invocation".to_string()),
        cwd: "/repo".to_string(),
    })
    .with_config(json!({"model": "judge"}))
    .with_env(vec!["JUDGE_API_KEY".to_string()]);
    let value = serde_json::to_value(&init).expect("init serializes");
    assert_eq!(value["type"], json!("init"));
    assert_eq!(value["id"], json!("init"));
    let round_trip: InitFrame = serde_json::from_value(value).expect("init parses");
    assert_eq!(round_trip, init);

    let ready = ReadyFrame::new("fake-safety".to_string(), "0.1.0".to_string());
    let value = serde_json::to_value(&ready).expect("ready serializes");
    assert_eq!(value["type"], json!("ready"));
    assert_eq!(value["id"], json!("init"));
    let round_trip: ReadyFrame = serde_json::from_value(value).expect("ready parses");
    assert_eq!(round_trip, ready);
}

#[test]
fn decision_validation_requires_code_and_message_for_block_and_warn() {
    let mut frame = DecisionFrame::new("request-1".to_string(), GateDecision::Block);
    assert!(frame.validate("request-1").is_err());
    frame = frame.with_message("stop".to_string());
    assert!(frame.validate("request-1").is_err());
    frame = frame.with_code("fake-block".to_string());
    frame.validate("request-1").expect("complete block");

    let mut warn = DecisionFrame::new("request-1".to_string(), GateDecision::Warn)
        .with_message("look".to_string())
        .with_code("fake-warn".to_string());
    warn.validate("request-1").expect("complete warn");
    warn.code = None;
    assert!(warn.validate("request-1").is_err());

    let mut allow = DecisionFrame::new("request-1".to_string(), GateDecision::Allow);
    allow.validate("request-1").expect("allow needs nothing");
    allow.confidence = Some(1.5);
    assert!(allow.validate("request-1").is_err());
    allow.confidence = Some(f32::NAN);
    assert!(allow.validate("request-1").is_err());
    allow.confidence = Some(0.5);
    assert!(allow.validate("other").is_err());
    allow.id = "request-1".to_string();
    allow.protocol = "cake-gate/2".to_string();
    assert!(allow.validate("request-1").is_err());
}

#[test]
fn decision_unknown_fields_and_open_annotations_are_tolerated() {
    let value = json!({
        "protocol": "cake-gate/1",
        "type": "decision",
        "id": "request-1",
        "decision": "warn",
        "code": "fake-warn",
        "message": "look",
        "annotations": [{"placement": "prepend", "text": "NOTICE: look", "future": 1}],
        "evidence": [{"kind": "attempt", "data": {"model": "judge"}, "future": 1}],
        "future": {"anything": true}
    });
    let frame: DecisionFrame = serde_json::from_value(value).expect("decision parses");
    frame.validate("request-1").expect("decision validates");
    assert_eq!(frame.annotations.len(), 1);
    assert_eq!(frame.evidence.len(), 1);
}

#[test]
fn error_and_shutdown_frames_round_trip() {
    let error = ErrorFrame::new(
        "request-1".to_string(),
        GateErrorClass::Timeout,
        "deadline".to_string(),
    );
    let value = serde_json::to_value(&error).expect("error serializes");
    assert_eq!(value["type"], json!("error"));
    assert_eq!(value["class"], json!("timeout"));
    let round_trip: ErrorFrame = serde_json::from_value(value).expect("error parses");
    assert_eq!(round_trip, error);

    let shutdown = ShutdownFrame::new();
    let value = serde_json::to_value(&shutdown).expect("shutdown serializes");
    assert_eq!(value["type"], json!("shutdown"));
    assert_eq!(value["id"], json!("shutdown"));
    let round_trip: ShutdownFrame = serde_json::from_value(value).expect("shutdown parses");
    assert_eq!(round_trip, shutdown);
}

#[test]
fn bounded_text_helpers_match_the_model_text_rule() {
    let text = format!(
        "a\u{0007}b\nc\r\td{}",
        "x".repeat(MAX_MODEL_TEXT_CHARS + 20)
    );
    let sanitized = sanitize_model_text(&text);
    assert!(!sanitized.contains('\u{0007}'));
    assert!(sanitized.contains('\n'));
    assert!(sanitized.contains('\r'));
    assert!(sanitized.contains('\t'));
    assert_eq!(sanitized.chars().count(), MAX_MODEL_TEXT_CHARS);

    assert_eq!(bound_chars("héllo", 2), "hé");
    assert_eq!(bound_chars("short", 10), "short");

    let bounded = bound_error_message(&format!("\u{001B}{}", "y".repeat(2_000)));
    assert!(!bounded.contains('\u{001B}'));
    assert_eq!(bounded.chars().count(), MAX_ERROR_MESSAGE_CHARS);

    assert_eq!(json_byte_len(&json!({"a": 1})), 7);
}

#[test]
fn gate_names_and_versions_follow_the_schema_patterns() {
    assert!(is_valid_gate_name("safety"));
    assert!(is_valid_gate_name("builtin-safety"));
    assert!(!is_valid_gate_name("Safety"));
    assert!(!is_valid_gate_name("-safety"));
    assert!(!is_valid_gate_name(""));

    assert!(is_valid_gate_version("0.1.0"));
    assert!(is_valid_gate_version("1.2.3-rc.1"));
    assert!(!is_valid_gate_version("1.2"));
    assert!(!is_valid_gate_version("1.2.3-"));
    assert!(!is_valid_gate_version("v1.2.3"));
}

#[test]
fn codes_capabilities_and_identifiers_follow_the_schema_patterns() {
    assert!(is_valid_code("git-force-push"));
    assert!(!is_valid_code("git_force"));
    assert!(is_valid_evidence_kind("judge_attempt"));
    assert!(is_valid_capability("future-capability"));
    assert!(is_valid_identifier("request-1"));
    assert!(!is_valid_identifier(""));
    assert!(!is_valid_identifier(&"x".repeat(MAX_IDENTIFIER_CHARS + 1)));
}
