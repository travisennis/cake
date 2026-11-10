//! Integration tests for `cake debug session`.

#![expect(clippy::expect_used, reason = "test code uses expect for assertions")]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use serde_json::{Value, json};
use support::TestEnv;

const SESSION_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
const TASK_ID: &str = "550e8400-e29b-41d4-a716-446655440001";
const SECOND_TASK_ID: &str = "550e8400-e29b-41d4-a716-446655440002";
const INVOCATION_ID: &str = "550e8400-e29b-41d4-a716-446655440003";
const SECOND_INVOCATION_ID: &str = "550e8400-e29b-41d4-a716-446655440004";

fn session_env() -> TestEnv {
    TestEnv::new("cake-debug-session-test")
}

fn run(env: &TestEnv, args: &[&str]) -> Output {
    env.command()
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake")
}

fn run_with_key(env: &TestEnv, args: &[&str], key: &str) -> Output {
    env.command()
        .args(args)
        .env("CONFIG_TEST_KEY", key)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn parse_document(output: &Output) -> Value {
    serde_json::from_str(&stdout(output)).expect("stdout must be one JSON document")
}

fn session_path(env: &TestEnv) -> PathBuf {
    env.data_dir
        .join("sessions")
        .join(format!("{SESSION_ID}.jsonl"))
}

fn telemetry_path(env: &TestEnv) -> PathBuf {
    env.data_dir
        .join("session-telemetry")
        .join(format!("{SESSION_ID}.ndjson"))
}

#[cfg(unix)]
fn write_executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt as _;

    fs::create_dir_all(path.parent().expect("tool parent")).expect("create tool directory");
    fs::write(path, body).expect("write tool");
    let mut permissions = fs::metadata(path).expect("tool metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make tool executable");
}

fn write_jsonl(path: &Path, records: &[Value]) {
    fs::create_dir_all(path.parent().expect("record parent")).expect("create record parent");
    let content = records
        .iter()
        .map(|record| serde_json::to_string(record).expect("serialize record"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(path, format!("{content}\n")).expect("write JSONL");
}

fn meta(env: &TestEnv, timestamp: &str, system_prompt: Option<&str>) -> Value {
    let mut meta = json!({
        "type": "session_meta",
        "format_version": 4,
        "session_id": SESSION_ID,
        "timestamp": timestamp,
        "working_directory": env.workspace_dir.display().to_string(),
        "model": "test-model",
        "model_config": "test",
        "tools": ["Bash", "Read"],
        "cake_version": "dev",
        "git": {},
    });
    if let Some(system_prompt) = system_prompt {
        meta["system_prompt"] = json!(system_prompt);
    }
    meta
}

fn task_start(task_id: &str, timestamp: &str) -> Value {
    json!({
        "type": "task_start",
        "session_id": SESSION_ID,
        "task_id": task_id,
        "timestamp": timestamp,
        "model": "test-model",
        "model_config": "test",
    })
}

fn task_complete(task_id: &str, subtype: &str, is_error: bool, duration_ms: u64) -> Value {
    json!({
        "type": "task_complete",
        "subtype": subtype,
        "is_error": is_error,
        "duration_ms": duration_ms,
        "turn_count": 2,
        "tool_call_count": 1,
        "session_id": SESSION_ID,
        "task_id": task_id,
        "usage": usage(),
    })
}

fn usage() -> Value {
    json!({
        "input_tokens": 100,
        "input_tokens_details": {"cached_tokens": 20, "cache_write_tokens": 10},
        "output_tokens": 50,
        "output_tokens_details": {"reasoning_tokens": 5},
        "total_tokens": 150,
    })
}

fn telemetry_init(invocation_id: &str, timestamp: &str, base_url: &str) -> Value {
    json!({
        "type": "telemetry_init",
        "session_id": SESSION_ID,
        "invocation_id": invocation_id,
        "timestamp": timestamp,
        "mode": "new",
        "model": "test-model",
        "api_type": "chat_completions",
        "output_format": "text",
        "tools": ["Bash", "Read"],
        "settings": {"base_url": base_url},
    })
}

fn in_flight(invocation_id: &str, timestamp: &str, task_id: &str) -> Value {
    json!({
        "type": "api_attempt_in_flight",
        "session_id": SESSION_ID,
        "invocation_id": invocation_id,
        "timestamp": timestamp,
        "task_id": task_id,
        "turn_index": 1,
        "attempt": 1,
        "model": "test-model",
        "started_at": timestamp,
        "phase": "awaiting_headers",
    })
}

fn telemetry_records(
    invocation_id: &str,
    task_id: &str,
    start: &str,
    base_url: &str,
) -> Vec<Value> {
    vec![
        telemetry_init(invocation_id, start, base_url),
        in_flight(invocation_id, start, task_id),
        json!({
            "type": "api_attempt",
            "session_id": SESSION_ID,
            "invocation_id": invocation_id,
            "timestamp": "2026-01-02T03:00:02Z",
            "turn_index": 1,
            "attempt": 1,
            "request_ms": 4000,
            "parse_ms": 1000,
            "total_ms": 5000,
            "history_items": 3,
            "status_code": 200,
            "error": "ERROR_CANARY",
            "usage": usage(),
            "usage_presence": "complete",
            "model": "test-model",
            "terminal_class": "completed",
            "request_overrides": {},
        }),
        json!({
            "type": "retry_scheduled",
            "session_id": SESSION_ID,
            "invocation_id": invocation_id,
            "timestamp": "2026-01-02T03:00:03Z",
            "turn_index": 1,
            "attempt": 1,
            "max_retries": 3,
            "reason": "server_error",
            "delay_ms": 1500,
            "detail": "DETAIL_CANARY",
            "changed_request_overrides": false,
            "request_overrides": {},
        }),
        json!({
            "type": "retry_wait",
            "session_id": SESSION_ID,
            "invocation_id": invocation_id,
            "timestamp": "2026-01-02T03:00:04Z",
            "turn_index": 1,
            "attempt": 1,
            "started_at": "2026-01-02T03:00:03Z",
            "completed_at": "2026-01-02T03:00:04Z",
            "duration_ms": 1500,
        }),
        json!({
            "type": "tool_call",
            "session_id": SESSION_ID,
            "invocation_id": invocation_id,
            "timestamp": "2026-01-02T03:00:05Z",
            "turn_index": 1,
            "name": "Bash",
            "duration_ms": 2500,
            "output_bytes": 10,
            "was_error": false,
        }),
        json!({
            "type": "gate_evidence",
            "session_id": SESSION_ID,
            "invocation_id": invocation_id,
            "timestamp": "2026-01-02T03:00:06Z",
            "gate": "judge",
            "kind": "command",
            "data": {"command": "GATE_CANARY"},
        }),
        json!({
            "type": "session_summary",
            "session_id": SESSION_ID,
            "invocation_id": invocation_id,
            "timestamp": "2026-01-02T03:00:07Z",
            "success": true,
            "duration_ms": 5000,
            "turn_count": 2,
            "usage": usage(),
            "error": "SUMMARY_CANARY",
        }),
    ]
}

fn completed_session(env: &TestEnv, canaries: bool) -> Vec<Value> {
    let system_prompt = canaries.then_some("HEADER_CANARY");
    let hook_command = if canaries {
        format!(
            "touch {} HOOK_CANARY",
            env.workspace_dir.join("hook-ran").display()
        )
    } else {
        "HOOK_CANARY".to_string()
    };
    vec![
        meta(env, "2026-01-02T03:00:00Z", system_prompt),
        task_start(TASK_ID, "2026-01-02T03:00:01Z"),
        json!({
            "type": "prompt_context",
            "session_id": SESSION_ID,
            "task_id": TASK_ID,
            "role": "user",
            "content": "PROMPT_CANARY",
            "timestamp": "2026-01-02T03:00:01Z",
        }),
        json!({
            "type": "message",
            "role": "user",
            "content": "MESSAGE_CANARY",
            "timestamp": "2026-01-02T03:00:02Z",
        }),
        json!({
            "type": "function_call",
            "id": "call-1",
            "call_id": "call-1",
            "name": "Bash",
            "arguments": "ARGUMENT_CANARY",
            "timestamp": "2026-01-02T03:00:03Z",
        }),
        json!({
            "type": "function_call_output",
            "call_id": "call-1",
            "output": "OUTPUT_CANARY",
            "timestamp": "2026-01-02T03:00:04Z",
        }),
        json!({
            "type": "hook_event",
            "timestamp": "2026-01-02T03:00:05Z",
            "task_id": TASK_ID,
            "event": "PreToolUse",
            "source_file": "/hooks.json",
            "command": hook_command,
            "duration_ms": 2,
            "decision": "allow",
            "fail_closed": false,
            "stdout": "HOOK_STDOUT_CANARY",
            "stderr": "HOOK_STDERR_CANARY",
        }),
        task_complete(TASK_ID, "success", false, 5000),
    ]
}

fn write_completed_session(env: &TestEnv, canaries: bool) {
    write_jsonl(&session_path(env), &completed_session(env, canaries));
    let base_url = if canaries {
        "https://user:URL_CANARY@example.invalid/v1?key=URL_CANARY"
    } else {
        "https://example.invalid/v1"
    };
    write_jsonl(
        &telemetry_path(env),
        &telemetry_records(INVOCATION_ID, TASK_ID, "2026-01-02T03:00:01Z", base_url),
    );
}

fn write_resumed_session(env: &TestEnv) {
    let mut session = completed_session(env, false);
    session.push(task_start(SECOND_TASK_ID, "2026-01-02T03:10:01Z"));
    session.push(task_complete(
        SECOND_TASK_ID,
        "error_during_execution",
        true,
        9000,
    ));
    write_jsonl(&session_path(env), &session);

    let mut telemetry = telemetry_records(
        INVOCATION_ID,
        TASK_ID,
        "2026-01-02T03:00:01Z",
        "https://example.invalid/v1",
    );
    telemetry.extend(telemetry_records(
        SECOND_INVOCATION_ID,
        SECOND_TASK_ID,
        "2026-01-02T03:10:01Z",
        "https://example.invalid/v1",
    ));
    write_jsonl(&telemetry_path(env), &telemetry);
}

#[test]
fn debug_session_text_reports_state_artifacts_and_operations() {
    let env = session_env();
    fs::create_dir_all(env.data_dir.join("session-telemetry")).expect("create telemetry directory");
    write_completed_session(&env, false);

    let output = run(&env, &["debug", "session", SESSION_ID]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let report = stdout(&output);
    assert!(report.contains("Session diagnostics"));
    assert!(report.contains("State: completed"));
    assert!(report.contains("Liveness: not held"));
    assert!(report.contains("Transcript\n"));
    assert!(report.contains("Telemetry\n"));
    assert!(report.contains("Artifacts\n"));
    assert!(report.contains("Tasks\n"));
    assert!(report.contains(TASK_ID));
    assert!(report.contains("Retries\n"));
    assert!(report.contains("server_error"));
    assert!(report.contains("Slow operations"));
    assert!(report.contains("Current configuration\n"));
    assert!(report.contains("Model: (none)"));
}

#[test]
#[expect(
    clippy::cognitive_complexity,
    reason = "the test asserts the full machine-readable report shape"
)]
fn debug_session_json_is_deterministic_and_correlates_the_invocation() {
    let env = session_env();
    fs::create_dir_all(env.data_dir.join("session-telemetry")).expect("create telemetry directory");
    write_completed_session(&env, false);

    let first = run(&env, &["debug", "session", SESSION_ID, "--json"]);
    let second = run(&env, &["debug", "session", SESSION_ID, "--json"]);

    assert_eq!(first.status.code(), Some(0), "stderr: {}", stderr(&first));
    assert_eq!(
        stdout(&first),
        stdout(&second),
        "output must be byte-stable"
    );
    assert_eq!(stderr(&first), "");
    let document = parse_document(&first);
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["command"], "debug session");
    assert_eq!(document["status"], "ok");
    assert_eq!(document["summary"]["state"], "completed");
    assert_eq!(document["summary"]["liveness"], "not_live");
    assert_eq!(document["summary"]["task_count"], 1);
    assert_eq!(document["summary"]["completed_task_count"], 1);
    assert_eq!(document["summary"]["invocation_count"], 1);
    assert_eq!(document["summary"]["matched_invocation_count"], 1);
    assert_eq!(document["data"]["tasks"][0]["termination"], "success");
    assert_eq!(document["data"]["tasks"][0]["invocation_id"], INVOCATION_ID);
    assert_eq!(document["data"]["tasks"][0]["invocation_match"], "task_id");
    assert_eq!(document["data"]["invocations"][0]["task_id"], TASK_ID);
    assert_eq!(document["data"]["retries"][0]["reason"], "server_error");
    assert!(
        document["data"]["slow_operations"]
            .as_array()
            .expect("slow operations")
            .iter()
            .any(|operation| operation["kind"] == "api_attempt")
    );
    assert!(document["data"]["current_config"].is_object());
    assert_eq!(document["data"]["transcript"]["state"], "ok");
    assert_eq!(document["data"]["telemetry"]["state"], "present");
    assert_eq!(document["data"]["diagnosis_complete"], true);
    assert_eq!(document["data"]["last_activity"]["source"], "telemetry");
    assert_eq!(
        document["data"]["last_activity"]["at"],
        "2026-01-02T03:00:07Z"
    );
}

#[test]
fn debug_session_last_activity_includes_a_missing_sidecars_task_end() {
    let env = session_env();
    write_jsonl(&session_path(&env), &completed_session(&env, false));

    let output = run(&env, &["debug", "session", SESSION_ID, "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["data"]["last_activity"]["source"], "transcript");
    assert_eq!(
        document["data"]["last_activity"]["at"],
        "2026-01-02T03:00:06Z"
    );
}

#[test]
fn debug_session_reports_interrupted_state_and_missing_telemetry() {
    let env = session_env();
    write_jsonl(
        &session_path(&env),
        &[
            meta(&env, "2026-01-02T03:00:00Z", None),
            task_start(TASK_ID, "2026-01-02T03:00:01Z"),
        ],
    );

    let output = run(&env, &["debug", "session", SESSION_ID, "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["status"], "warning");
    assert_eq!(document["summary"]["state"], "interrupted");
    assert_eq!(document["data"]["tasks"][0]["state"], "interrupted");
    assert_eq!(document["data"]["telemetry"]["state"], "missing");
    let checks = document["checks"].as_array().expect("checks array");
    assert!(
        checks
            .iter()
            .any(|check| check["id"] == "session.telemetry_missing")
    );
    assert!(
        checks
            .iter()
            .any(|check| check["id"] == "session.incomplete_task")
    );
}

#[test]
fn debug_session_reports_a_live_writer() {
    let env = session_env();
    write_jsonl(
        &session_path(&env),
        &[
            meta(&env, "2026-01-02T03:00:00Z", None),
            task_start(TASK_ID, "2026-01-02T03:00:01Z"),
        ],
    );
    let file = fs::OpenOptions::new()
        .read(true)
        .append(true)
        .open(session_path(&env))
        .expect("open session for lock");
    file.try_lock().expect("hold session lock");

    let output = run(&env, &["debug", "session", SESSION_ID, "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["summary"]["state"], "live");
    assert_eq!(document["summary"]["liveness"], "live");
    assert_eq!(document["data"]["tasks"][0]["state"], "live");

    fs4::FileExt::unlock(&file).expect("release session lock");
}

#[test]
fn debug_session_tolerates_a_partial_tail_without_claiming_complete_evidence() {
    let env = session_env();
    write_completed_session(&env, false);
    let mut content = fs::read_to_string(session_path(&env)).expect("read session");
    content.push_str(r#"{"type":"message","role":"user","content":"tail"#);
    fs::write(session_path(&env), content).expect("append partial tail");

    let output = run(&env, &["debug", "session", SESSION_ID, "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["summary"]["state"], "completed");
    assert_eq!(document["data"]["transcript"]["partial_tail"], true);
    assert_eq!(document["data"]["transcript"]["ignored_partial_tail"], true);
    assert_eq!(document["data"]["diagnosis_complete"], false);
    assert!(
        document["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .any(|check| check["id"] == "session.partial_tail")
    );
}

#[test]
fn debug_session_marks_a_valid_unterminated_tail_incomplete() {
    let env = session_env();
    write_completed_session(&env, false);
    let content = fs::read_to_string(session_path(&env)).expect("read session");
    fs::write(session_path(&env), content.trim_end_matches('\n')).expect("remove final newline");

    let output = run(&env, &["debug", "session", SESSION_ID, "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["summary"]["state"], "completed");
    assert_eq!(document["data"]["transcript"]["partial_tail"], true);
    assert_eq!(
        document["data"]["transcript"]["ignored_partial_tail"],
        false
    );
    assert_eq!(document["data"]["diagnosis_complete"], false);
}

#[test]
fn debug_session_rejects_interior_corruption_with_an_error_document() {
    let env = session_env();
    write_completed_session(&env, false);
    let mut content = fs::read_to_string(session_path(&env)).expect("read session");
    content.push_str("not a record SECRET_CANARY\n");
    fs::write(session_path(&env), content).expect("append corrupt record");

    let output = run(&env, &["debug", "session", SESSION_ID, "--json"]);

    assert_eq!(output.status.code(), Some(1), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["status"], "error");
    assert_eq!(document["data"]["transcript"]["state"], "corrupt");
    assert_eq!(document["data"]["diagnosis_complete"], false);
    assert!(
        document["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .any(|check| check["id"] == "session.transcript_corrupt")
    );
    assert!(!stdout(&output).contains("not a record"));
    assert!(!stdout(&output).contains("SECRET_CANARY"));
    assert!(!stderr(&output).contains("SECRET_CANARY"));
}

#[test]
fn debug_session_rejects_corrupt_telemetry_with_an_error_document() {
    let env = session_env();
    write_completed_session(&env, false);
    let mut content = fs::read_to_string(telemetry_path(&env)).expect("read telemetry");
    content.push_str("{\"type\":\"api_attempt\",\"error\":\"SECRET_CANARY\"\n");
    fs::write(telemetry_path(&env), content).expect("append corrupt telemetry");

    let output = run(&env, &["debug", "session", SESSION_ID, "--json"]);

    assert_eq!(output.status.code(), Some(1), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["status"], "error");
    assert_eq!(document["data"]["telemetry"]["state"], "corrupt");
    assert_eq!(document["data"]["diagnosis_complete"], false);
    assert!(
        document["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .any(|check| check["id"] == "session.telemetry_corrupt")
    );
    assert!(!stdout(&output).contains("SECRET_CANARY"));
    assert!(!stderr(&output).contains("SECRET_CANARY"));
}

#[test]
fn debug_session_separates_resumed_tasks_and_invocations() {
    let env = session_env();
    fs::create_dir_all(env.data_dir.join("session-telemetry")).expect("create telemetry directory");
    write_resumed_session(&env);

    let output = run(&env, &["debug", "session", SESSION_ID, "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["summary"]["task_count"], 2);
    assert_eq!(document["summary"]["invocation_count"], 2);
    assert_eq!(document["summary"]["matched_invocation_count"], 2);
    assert_eq!(document["data"]["tasks"][0]["invocation_id"], INVOCATION_ID);
    assert_eq!(
        document["data"]["tasks"][1]["invocation_id"],
        SECOND_INVOCATION_ID
    );
    assert_eq!(
        document["data"]["tasks"][1]["termination"],
        "error_during_execution"
    );
}

#[test]
fn debug_session_export_is_redacted_owner_only_and_does_not_modify_sources() {
    let env = session_env();
    write_completed_session(&env, true);
    let log = env.data_dir.join("cake.2026-01-02.log");
    fs::write(&log, "LOG_CANARY\n").expect("write log");
    env.write_global_settings(
        r#"default_model = "test"

[[models]]
name = "test"
model = "test-model"
base_url = "https://example.invalid/v1"
api_key_env = "CONFIG_TEST_KEY"
"#,
    );
    let export = env.home_dir.join("diagnostic.json");
    let tool_marker = env.workspace_dir.join("toolbox-ran");
    #[cfg(unix)]
    write_executable(
        &env.home_dir.join(".config/cake/tools/probe"),
        &format!(
            "#!/bin/sh\ntouch {}\necho '{{\"name\":\"probe\"}}'\n",
            tool_marker.display()
        ),
    );
    let session_before = fs::read(session_path(&env)).expect("read session");
    let telemetry_before = fs::read(telemetry_path(&env)).expect("read telemetry");

    let output = run_with_key(
        &env,
        &[
            "debug",
            "session",
            SESSION_ID,
            "--json",
            "--export",
            export.to_str().expect("export path"),
        ],
        "KEY_CANARY",
    );

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let rendered = stdout(&output);
    let exported = fs::read_to_string(&export).expect("read export");
    assert_eq!(rendered, exported);
    for canary in [
        "HEADER_CANARY",
        "PROMPT_CANARY",
        "MESSAGE_CANARY",
        "ARGUMENT_CANARY",
        "OUTPUT_CANARY",
        "HOOK_CANARY",
        "HOOK_STDOUT_CANARY",
        "HOOK_STDERR_CANARY",
        "ERROR_CANARY",
        "DETAIL_CANARY",
        "GATE_CANARY",
        "SUMMARY_CANARY",
        "LOG_CANARY",
        "KEY_CANARY",
        "URL_CANARY",
    ] {
        assert!(!rendered.contains(canary), "stdout leaked {canary}");
        assert!(!exported.contains(canary), "export leaked {canary}");
    }
    assert!(!env.workspace_dir.join("hook-ran").exists());
    assert!(!tool_marker.exists());
    assert_eq!(
        fs::read(session_path(&env)).expect("read session after"),
        session_before
    );
    assert_eq!(
        fs::read(telemetry_path(&env)).expect("read telemetry after"),
        telemetry_before
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(&export)
                .expect("export metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn debug_session_export_refuses_existing_targets_and_source_artifacts() {
    let env = session_env();
    write_completed_session(&env, false);
    let export = env.home_dir.join("existing.json");
    fs::write(&export, "keep me").expect("write existing export");
    let session_before = fs::read(session_path(&env)).expect("read session");

    let existing = run(
        &env,
        &[
            "debug",
            "session",
            SESSION_ID,
            "--json",
            "--export",
            export.to_str().expect("export path"),
        ],
    );
    assert_eq!(
        existing.status.code(),
        Some(3),
        "stderr: {}",
        stderr(&existing)
    );
    assert_eq!(
        fs::read_to_string(&export).expect("read existing export"),
        "keep me"
    );
    let document = parse_document(&existing);
    assert_eq!(document["status"], "error");
    assert_eq!(document["checks"][0]["id"], "session.invalid_input");

    let source = run(
        &env,
        &[
            "debug",
            "session",
            SESSION_ID,
            "--json",
            "--export",
            session_path(&env).to_str().expect("session path"),
        ],
    );
    assert_eq!(source.status.code(), Some(3), "stderr: {}", stderr(&source));
    assert_eq!(
        fs::read(session_path(&env)).expect("read session after"),
        session_before
    );
}

#[test]
fn debug_session_missing_session_is_an_input_error() {
    let env = session_env();

    let output = run(
        &env,
        &[
            "debug",
            "session",
            "550e8400-e29b-41d4-a716-446655440099",
            "--json",
        ],
    );

    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["status"], "error");
    assert_eq!(document["checks"][0]["id"], "session.invalid_input");
}

#[test]
fn debug_session_invalid_uuid_is_an_input_error() {
    let env = session_env();

    let output = run(&env, &["debug", "session", "not-a-uuid", "--json"]);

    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["status"], "error");
    assert_eq!(document["checks"][0]["id"], "session.invalid_input");
}

#[test]
fn debug_session_works_with_an_alternate_data_root() {
    let env = session_env();
    write_completed_session(&env, false);
    assert!(session_path(&env).starts_with(&env.data_dir));
    assert!(telemetry_path(&env).starts_with(&env.data_dir));

    let output = run(&env, &["debug", "session", SESSION_ID, "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(
        document["data"]["transcript"]["path"],
        session_path(&env).display().to_string()
    );
    assert_eq!(
        document["data"]["telemetry"]["path"],
        telemetry_path(&env).display().to_string()
    );
}

#[test]
fn debug_session_degrades_when_current_settings_cannot_be_loaded() {
    let env = session_env();
    write_completed_session(&env, false);
    env.write_project_settings("not valid toml = [");

    let output = run(&env, &["debug", "session", SESSION_ID, "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["status"], "warning");
    assert_eq!(document["data"]["current_config"], Value::Null);
    assert!(
        document["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .any(|check| check["id"] == "session.current_config_unavailable")
    );
}

#[test]
fn write_jsonl_keeps_a_trailing_newline() {
    let env = session_env();
    write_jsonl(&session_path(&env), &[json!({"type": "session_meta"})]);
    let content = fs::read_to_string(session_path(&env)).expect("read session");
    assert!(content.ends_with('\n'));
}

#[test]
fn export_path_must_have_an_existing_parent() {
    let env = session_env();
    write_completed_session(&env, false);
    let export = env.home_dir.join("missing/diagnostic.json");

    let output = run(
        &env,
        &[
            "debug",
            "session",
            SESSION_ID,
            "--json",
            "--export",
            export.to_str().expect("export path"),
        ],
    );

    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    assert!(!export.exists());
}

#[test]
fn debug_session_does_not_follow_a_symlinked_transcript() {
    #[cfg(unix)]
    {
        let env = session_env();
        write_completed_session(&env, false);
        let target = env.home_dir.join("target.jsonl");
        fs::rename(session_path(&env), &target).expect("move transcript");
        std::os::unix::fs::symlink(&target, session_path(&env)).expect("symlink transcript");

        let output = run(&env, &["debug", "session", SESSION_ID, "--json"]);

        assert_eq!(output.status.code(), Some(1), "stderr: {}", stderr(&output));
        let document = parse_document(&output);
        assert_eq!(document["data"]["transcript"]["state"], "unreadable");
        assert_eq!(document["data"]["liveness"]["state"], "unknown");
        assert!(
            document["checks"]
                .as_array()
                .expect("checks")
                .iter()
                .any(|check| check["id"] == "session.transcript_unreadable")
        );
        assert!(
            document["checks"]
                .as_array()
                .expect("checks")
                .iter()
                .any(|check| check["id"] == "session.liveness_unknown")
        );
    }
}

#[test]
fn debug_session_truncates_the_slow_operation_list_but_keeps_the_count() {
    let env = session_env();
    write_completed_session(&env, false);
    let mut records = telemetry_records(
        INVOCATION_ID,
        TASK_ID,
        "2026-01-02T03:00:01Z",
        "https://example.invalid/v1",
    );
    for index in 0..25 {
        records.push(json!({
            "type": "tool_call",
            "session_id": SESSION_ID,
            "invocation_id": INVOCATION_ID,
            "timestamp": format!("2026-01-02T03:01:{index:02}Z"),
            "turn_index": 1,
            "name": "Bash",
            "duration_ms": 3000 + index,
            "output_bytes": 1,
            "was_error": false,
        }));
    }
    write_jsonl(&telemetry_path(&env), &records);

    let output = run(&env, &["debug", "session", SESSION_ID, "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["summary"]["slow_operation_count"], 27);
    assert_eq!(
        document["data"]["slow_operations"]
            .as_array()
            .expect("slow operations")
            .len(),
        20
    );
}

#[test]
fn debug_session_has_an_empty_report_for_a_header_only_session() {
    let env = session_env();
    write_jsonl(
        &session_path(&env),
        &[meta(&env, "2026-01-02T03:00:00Z", None)],
    );

    let output = run(&env, &["debug", "session", SESSION_ID, "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["summary"]["state"], "interrupted");
    assert_eq!(document["data"]["tasks"], json!([]));
}

#[test]
fn debug_session_text_uses_the_export_path() {
    let env = session_env();
    write_completed_session(&env, false);
    let export = env.home_dir.join("text-export.json");

    let output = run(
        &env,
        &[
            "debug",
            "session",
            SESSION_ID,
            "--export",
            export.to_str().expect("export path"),
        ],
    );

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("Exported redacted diagnostic document to"));
    assert!(export.exists());
}

#[test]
fn debug_session_slow_threshold_is_configurable() {
    let env = session_env();
    write_completed_session(&env, false);

    let output = run(
        &env,
        &[
            "debug",
            "session",
            SESSION_ID,
            "--slow-ms",
            "6000",
            "--json",
        ],
    );

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["summary"]["slow_operation_count"], 0);
    assert_eq!(document["data"]["slow_operations"], json!([]));
}

#[test]
fn debug_session_export_accepts_a_relative_path_in_the_working_directory() {
    let env = session_env();
    write_completed_session(&env, false);

    let output = run(
        &env,
        &[
            "debug",
            "session",
            SESSION_ID,
            "--json",
            "--export",
            "relative-diagnostic.json",
        ],
    );

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(env.workspace_dir.join("relative-diagnostic.json").exists());
}

#[test]
fn debug_session_help_lists_the_flags() {
    let env = session_env();
    let output = run(&env, &["debug", "session", "--help"]);
    let help = stdout(&output);

    assert!(output.status.success());
    assert!(help.contains("--export"));
    assert!(help.contains("--slow-ms"));
    assert!(help.contains("--json"));
}
