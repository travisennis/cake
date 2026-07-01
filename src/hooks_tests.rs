use super::*;
use crate::config::hooks::{HookGroup, HookMatcher};

fn runner(command: &str, fail_closed: bool) -> HookRunner {
    let cwd = std::env::temp_dir();
    let source_path = cwd.join("hooks.json");
    let command = HookCommand {
        command: command.to_string(),
        timeout: Duration::from_secs(2),
        fail_closed,
        status_message: None,
        source_path,
    };
    let loaded = LoadedHooks {
        groups: vec![HookGroup {
            event: HookEvent::PreToolUse,
            matcher: HookMatcher::All,
            hooks: vec![command],
        }],
    };
    HookRunner::new(
        loaded,
        HookContext {
            session_id: uuid::Uuid::new_v4(),
            task_id: uuid::Uuid::new_v4(),
            transcript_path: None,
            session_writer: None,
            hook_event_sink: None,
            cwd,
            model: "test-model".to_string(),
        },
    )
}

#[tokio::test]
#[cfg(unix)]
async fn command_hook_receives_stdin_json() {
    let runner = runner(
        "payload=$(cat); case \"$payload\" in *'\"tool_name\":\"Bash\"'*) printf '{\"permission\":\"allow\"}' ;; *) exit 1 ;; esac",
        false,
    );

    let plan = runner
        .pre_tool_use("Bash", "call-1", r#"{"command":"printf ok"}"#)
        .await
        .unwrap();

    assert!(matches!(plan, ToolHookPlan::Execute { .. }));
}

#[tokio::test]
#[cfg(unix)]
async fn exit_two_blocks_pre_tool_use() {
    let runner = runner("echo blocked >&2; exit 2", false);

    let plan = runner
        .pre_tool_use("Bash", "call-1", r#"{"command":"printf ok"}"#)
        .await
        .unwrap();

    match plan {
        ToolHookPlan::Block { reason, .. } => assert!(reason.contains("blocked")),
        ToolHookPlan::Execute { .. } => panic!("expected block"),
    }
}

#[tokio::test]
#[cfg(unix)]
async fn invalid_json_fails_open_by_default() {
    let runner = runner("printf not-json", false);

    let plan = runner
        .pre_tool_use("Bash", "call-1", r#"{"command":"printf ok"}"#)
        .await
        .unwrap();

    assert!(matches!(plan, ToolHookPlan::Execute { .. }));
}

#[tokio::test]
#[cfg(unix)]
async fn post_tool_use_persists_hook_record_while_session_locked() {
    use crate::config::session::CURRENT_FORMAT_VERSION;
    use crate::config::{Session, SessionWriter};
    use crate::types::{GitState, SessionRecord};

    let dir = tempfile::TempDir::new().unwrap();
    let session_path = dir.path().join("session.jsonl");

    let session_id = uuid::Uuid::new_v4();
    let meta = SessionRecord::SessionMeta {
        format_version: CURRENT_FORMAT_VERSION,
        session_id: session_id.to_string(),
        timestamp: chrono::Utc::now(),
        working_directory: dir.path().to_path_buf(),
        model: Some("test-model".to_string()),
        tools: Vec::new(),
        cake_version: Some("test".to_string()),
        system_prompt: None,
        git: GitState {
            repository_url: None,
            branch: None,
            commit_hash: None,
        },
    };
    let file = Session::create_on_disk(&session_path, &meta).unwrap();
    let writer = SessionWriter::new(file);

    let source_path = dir.path().join("hooks.json");
    let command = HookCommand {
        command: r#"printf '{"permission":"allow"}'"#.to_string(),
        timeout: Duration::from_secs(2),
        fail_closed: false,
        status_message: None,
        source_path,
    };
    let loaded = LoadedHooks {
        groups: vec![HookGroup {
            event: HookEvent::PostToolUse,
            matcher: HookMatcher::All,
            hooks: vec![command],
        }],
    };
    let runner = HookRunner::new(
        loaded,
        HookContext {
            session_id,
            task_id: uuid::Uuid::new_v4(),
            transcript_path: Some(session_path.clone()),
            session_writer: Some(writer.clone()),
            hook_event_sink: None,
            cwd: dir.path().to_path_buf(),
            model: "test-model".to_string(),
        },
    );

    runner
        .post_tool_use(
            "Bash",
            "call-1",
            r#"{"command":"printf ok"}"#,
            &Ok("ok".to_string()),
        )
        .await
        .unwrap();

    // SessionWriter still holds the lock; reads via fs are unaffected by
    // advisory locks on macOS/Linux.
    let content = std::fs::read_to_string(&session_path).unwrap();
    assert!(
        content.contains(r#""type":"hook_event""#),
        "expected hook_event record in {content}"
    );
    assert!(content.contains(r#""event":"PostToolUse""#));
    assert!(content.contains(r#""call_id":"call-1""#));
    assert!(content.contains(r#""tool_name":"Bash""#));
    assert!(content.contains(r#""tool_input_summary":"printf ok""#));
    assert!(content.contains(r#""resolved_decision":"allow""#));
    drop(writer);
}

#[tokio::test]
#[cfg(unix)]
async fn post_tool_use_emits_hook_record_to_sink_without_session_writer() {
    let dir = tempfile::TempDir::new().unwrap();
    let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured);

    let source_path = dir.path().join("hooks.json");
    let command = HookCommand {
        command: r#"printf '{"permission":"allow"}'"#.to_string(),
        timeout: Duration::from_secs(2),
        fail_closed: false,
        status_message: None,
        source_path,
    };
    let loaded = LoadedHooks {
        groups: vec![HookGroup {
            event: HookEvent::PostToolUse,
            matcher: HookMatcher::All,
            hooks: vec![command],
        }],
    };
    let runner = HookRunner::new(
        loaded,
        HookContext {
            session_id: uuid::Uuid::new_v4(),
            task_id: uuid::Uuid::new_v4(),
            transcript_path: None,
            session_writer: None,
            hook_event_sink: Some(Arc::new(move |record| {
                captured_clone.lock().unwrap().push(record);
            })),
            cwd: dir.path().to_path_buf(),
            model: "test-model".to_string(),
        },
    );

    runner
        .post_tool_use(
            "Bash",
            "call-1",
            r#"{"command":"printf ok"}"#,
            &Ok("ok".to_string()),
        )
        .await
        .unwrap();

    let captured = captured.lock().unwrap();
    assert_eq!(captured.len(), 1);
    match &captured[0] {
        StreamRecord::HookEvent(record) => {
            assert_eq!(record.event, "PostToolUse");
            assert_eq!(record.call_id.as_deref(), Some("call-1"));
            assert_eq!(record.tool_name.as_deref(), Some("Bash"));
            assert_eq!(record.resolved_decision.as_deref(), Some("allow"));
        },
        other => panic!("expected hook_event stream record, got {other:?}"),
    }
}

#[tokio::test]
#[cfg(unix)]
async fn fail_closed_invalid_json_blocks() {
    let runner = runner("printf not-json", true);

    let plan = runner
        .pre_tool_use("Bash", "call-1", r#"{"command":"printf ok"}"#)
        .await
        .unwrap();

    assert!(matches!(plan, ToolHookPlan::Block { .. }));
}

#[tokio::test]
#[cfg(unix)]
async fn post_tool_use_fail_closed_propagates_error() {
    let dir = tempfile::TempDir::new().unwrap();
    let source_path = dir.path().join("hooks.json");
    let command = HookCommand {
        command: "exit 1".to_string(),
        timeout: Duration::from_secs(2),
        fail_closed: true,
        status_message: None,
        source_path,
    };
    let loaded = LoadedHooks {
        groups: vec![HookGroup {
            event: HookEvent::PostToolUse,
            matcher: HookMatcher::All,
            hooks: vec![command],
        }],
    };
    let runner = HookRunner::new(
        loaded,
        HookContext {
            session_id: uuid::Uuid::new_v4(),
            task_id: uuid::Uuid::new_v4(),
            transcript_path: None,
            session_writer: None,
            hook_event_sink: None,
            cwd: dir.path().to_path_buf(),
            model: "test-model".to_string(),
        },
    );

    // A fail-closed PostToolUse hook that fails should return an error.
    let result = runner
        .post_tool_use(
            "Bash",
            "call-1",
            r#"{"command":"printf ok"}"#,
            &Ok("ok".to_string()),
        )
        .await;

    assert!(
        result.is_err(),
        "fail_closed post_tool_use hook should return error"
    );
}

// ── HookDecision::from_raw ──────────────────────────────────────

#[test]
fn decision_from_raw_continue_when_no_fields() {
    let d = HookDecision::from_raw(None, None, None, None, None);
    assert_eq!(d, HookDecision::Continue);
}

#[test]
fn decision_from_raw_continue_from_allow_permission() {
    let d = HookDecision::from_raw(None, None, Some("allow"), None, None);
    assert_eq!(d, HookDecision::Continue);
}

#[test]
fn decision_from_raw_continue_from_allow_decision_field() {
    let d = HookDecision::from_raw(None, None, None, Some("allow"), None);
    assert_eq!(d, HookDecision::Continue);
}

#[test]
fn decision_from_raw_continue_from_unknown_permission() {
    // Unknown values are treated as allow (continue).
    let d = HookDecision::from_raw(None, None, Some("xyz"), None, None);
    assert_eq!(d, HookDecision::Continue);
}

#[test]
fn decision_from_raw_continue_true_is_same_as_none() {
    let d = HookDecision::from_raw(Some(true), None, None, None, None);
    assert_eq!(d, HookDecision::Continue);
}

#[test]
fn decision_from_raw_stop_from_continue_false() {
    let d = HookDecision::from_raw(Some(false), None, None, None, None);
    assert_eq!(
        d,
        HookDecision::Stop {
            reason: "hook requested stop".to_string()
        }
    );
}

#[test]
fn decision_from_raw_stop_from_continue_false_with_stop_reason() {
    let d = HookDecision::from_raw(Some(false), Some("done"), None, None, None);
    assert_eq!(
        d,
        HookDecision::Stop {
            reason: "done".to_string()
        }
    );
}

#[test]
fn decision_from_raw_stop_from_continue_false_with_reason() {
    let d = HookDecision::from_raw(Some(false), None, None, None, Some("enough"));
    assert_eq!(
        d,
        HookDecision::Stop {
            reason: "enough".to_string()
        }
    );
}

#[test]
fn decision_from_raw_stop_from_continue_false_stop_reason_wins_over_reason() {
    let d = HookDecision::from_raw(Some(false), Some("first"), None, None, Some("second"));
    assert_eq!(
        d,
        HookDecision::Stop {
            reason: "first".to_string()
        }
    );
}

#[test]
fn decision_from_raw_stop_continue_false_takes_priority_over_permission() {
    // continue: false wins even if permission says allow.
    let d = HookDecision::from_raw(Some(false), None, Some("allow"), None, None);
    assert!(matches!(d, HookDecision::Stop { .. }));
}

#[test]
fn decision_from_raw_deny_from_permission_deny() {
    let d = HookDecision::from_raw(None, None, Some("deny"), None, None);
    assert_eq!(
        d,
        HookDecision::Deny {
            reason: "hook denied action".to_string()
        }
    );
}

#[test]
fn decision_from_raw_deny_from_decision_field_block() {
    let d = HookDecision::from_raw(None, None, None, Some("block"), None);
    assert_eq!(
        d,
        HookDecision::Deny {
            reason: "hook denied action".to_string()
        }
    );
}

#[test]
fn decision_from_raw_deny_from_ask_with_custom_default() {
    let d = HookDecision::from_raw(None, None, Some("ask"), None, None);
    assert_eq!(
        d,
        HookDecision::Deny {
            reason: "interactive ask is not supported yet".to_string()
        }
    );
}

#[test]
fn decision_from_raw_deny_from_permission_deny_with_reason() {
    let d = HookDecision::from_raw(None, None, Some("deny"), None, Some("not allowed"));
    assert_eq!(
        d,
        HookDecision::Deny {
            reason: "not allowed".to_string()
        }
    );
}

#[test]
fn decision_from_raw_permission_takes_priority_over_decision_field() {
    // permission is checked first, so "deny" in permission wins over
    // "allow" in decision.
    let d = HookDecision::from_raw(None, None, Some("deny"), Some("allow"), None);
    assert!(matches!(d, HookDecision::Deny { .. }));
}

// ── RawHookOutput deserialization ──────────────────────────────

#[test]
fn raw_deser_empty_object_is_continue() {
    let raw: RawHookOutput = serde_json::from_str("{}").unwrap();
    let parsed: ParsedHookOutput = raw.into();
    assert_eq!(parsed.decision, HookDecision::Continue);
    assert!(parsed.updated_input.is_none());
    assert!(parsed.additional_context.is_none());
}

#[test]
fn raw_deser_permission_allow() {
    let raw: RawHookOutput = serde_json::from_str(r#"{"permission": "allow"}"#).unwrap();
    let parsed: ParsedHookOutput = raw.into();
    assert_eq!(parsed.decision, HookDecision::Continue);
}

#[test]
fn raw_deser_continue_false_with_stop_reason() {
    let raw: RawHookOutput =
        serde_json::from_str(r#"{"continue": false, "stop_reason": "done"}"#).unwrap();
    let parsed: ParsedHookOutput = raw.into();
    assert_eq!(
        parsed.decision,
        HookDecision::Stop {
            reason: "done".to_string()
        }
    );
}

#[test]
fn raw_deser_decision_deny_with_reason() {
    let raw: RawHookOutput =
        serde_json::from_str(r#"{"decision": "deny", "reason": "nope"}"#).unwrap();
    let parsed: ParsedHookOutput = raw.into();
    assert_eq!(
        parsed.decision,
        HookDecision::Deny {
            reason: "nope".to_string()
        }
    );
}

#[test]
fn raw_deser_permission_block() {
    let raw: RawHookOutput = serde_json::from_str(r#"{"permission": "block"}"#).unwrap();
    let parsed: ParsedHookOutput = raw.into();
    assert!(matches!(parsed.decision, HookDecision::Deny { .. }));
}

#[test]
fn raw_deser_updated_input_and_additional_context_with_allow() {
    let raw: RawHookOutput = serde_json::from_str(
            r#"{"permission": "allow", "updated_input": {"cmd": "safe"}, "additional_context": "be careful"}"#,
        )
        .unwrap();
    let parsed: ParsedHookOutput = raw.into();
    assert_eq!(parsed.decision, HookDecision::Continue);
    assert_eq!(
        parsed.updated_input,
        Some(serde_json::json!({"cmd": "safe"}))
    );
    assert_eq!(parsed.additional_context, Some("be careful".to_string()));
}

// ── HookDecision::decision_label ───────────────────────────────

#[test]
fn decision_label_continue_is_none() {
    assert_eq!(HookDecision::Continue.decision_label(), "none");
}

#[test]
fn decision_label_deny_is_deny() {
    let d = HookDecision::Deny {
        reason: "x".to_string(),
    };
    assert_eq!(d.decision_label(), "deny");
}

#[test]
fn decision_label_stop_is_stop() {
    let d = HookDecision::Stop {
        reason: "x".to_string(),
    };
    assert_eq!(d.decision_label(), "stop");
}

// ── record_outcome decision label ──────────────────────────────

#[test]
fn decision_label_no_op_hook_is_none() {
    // Exit-0 hook with empty stdout: parsed is None, error is None
    // → should record decision "none", not "error"
    let outcome = InvocationOutcome {
        command: HookCommand {
            command: "true".to_string(),
            timeout: Duration::from_secs(2),
            fail_closed: false,
            status_message: None,
            source_path: PathBuf::from("/tmp/hooks.json"),
        },
        exit_code: Some(0),
        duration: Duration::from_millis(10),
        stdout: String::new(),
        stderr: String::new(),
        parsed: None,
        error: None,
    };

    let decision = outcome_decision_label(outcome.parsed.as_ref(), outcome.error.as_deref());
    assert_eq!(decision, "none");
}

#[test]
fn decision_label_hook_error_is_error() {
    // Hook that failed to start or had execution error:
    // parsed is None, error is Some → should record "error"
    let outcome = InvocationOutcome {
        command: HookCommand {
            command: "bad-command".to_string(),
            timeout: Duration::from_secs(2),
            fail_closed: false,
            status_message: None,
            source_path: PathBuf::from("/tmp/hooks.json"),
        },
        exit_code: Some(1),
        duration: Duration::from_millis(5),
        stdout: String::new(),
        stderr: "command not found".to_string(),
        parsed: None,
        error: Some("hook exited with code 1: command not found".to_string()),
    };

    let decision = outcome_decision_label(outcome.parsed.as_ref(), outcome.error.as_deref());
    assert_eq!(decision, "error");
}

#[test]
fn decision_label_parsed_continue_is_none() {
    // Parsed Continue decision should still label as "none"
    let outcome = InvocationOutcome {
        command: HookCommand {
            command: "hook.sh".to_string(),
            timeout: Duration::from_secs(2),
            fail_closed: false,
            status_message: None,
            source_path: PathBuf::from("/tmp/hooks.json"),
        },
        exit_code: Some(0),
        duration: Duration::from_millis(10),
        stdout: "{\"permission\":\"allow\"}".to_string(),
        stderr: String::new(),
        parsed: Some(ParsedHookOutput {
            decision: HookDecision::Continue,
            explicit_allow: true,
            updated_input: None,
            additional_context: None,
        }),
        error: None,
    };

    let decision = outcome_decision_label(outcome.parsed.as_ref(), outcome.error.as_deref());
    assert_eq!(decision, "none");
}

#[test]
fn resolved_decision_label_explicit_allow_is_allow() {
    let parsed: ParsedHookOutput =
        serde_json::from_str::<RawHookOutput>(r#"{"permission":"allow"}"#)
            .unwrap()
            .into();

    let decision = resolved_decision_label(Some(&parsed), None);

    assert_eq!(decision, "allow");
}

#[test]
fn tool_input_summary_prefers_command() {
    let summary = tool_input_summary(r#"{"command":"just ci","timeout":120}"#);

    assert_eq!(summary, "just ci");
}

#[test]
fn decision_label_parsed_deny_is_deny() {
    let outcome = InvocationOutcome {
        command: HookCommand {
            command: "hook.sh".to_string(),
            timeout: Duration::from_secs(2),
            fail_closed: false,
            status_message: None,
            source_path: PathBuf::from("/tmp/hooks.json"),
        },
        exit_code: Some(0),
        duration: Duration::from_millis(10),
        stdout: "{\"permission\":\"deny\"}".to_string(),
        stderr: String::new(),
        parsed: Some(ParsedHookOutput {
            decision: HookDecision::Deny {
                reason: "not allowed".to_string(),
            },
            explicit_allow: false,
            updated_input: None,
            additional_context: None,
        }),
        error: None,
    };

    let decision = outcome_decision_label(outcome.parsed.as_ref(), outcome.error.as_deref());
    assert_eq!(decision, "deny");
}
