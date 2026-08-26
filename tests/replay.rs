//! End-to-end coverage for `cake replay`: the command re-emits an existing
//! session transcript as stream-json events, reports structured failures, and
//! never mutates the session file.

#![expect(clippy::expect_used, reason = "test code uses expect for assertions")]
#![expect(
    dead_code,
    reason = "shared support helpers are used by some test binaries only"
)]

mod support;

use std::{
    fs,
    io::{BufRead, BufReader},
    path::PathBuf,
    process::Stdio,
};

use support::TestEnv;

const SESSION_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
const TASK_ID: &str = "550e8400-e29b-41d4-a716-446655440001";

fn cake_env() -> TestEnv {
    let env = TestEnv::new("cake-replay-test");
    fs::create_dir_all(env.data_dir.join("sessions")).expect("failed to create sessions dir");
    env
}

fn session_path(env: &TestEnv, id: &str) -> PathBuf {
    env.data_dir.join("sessions").join(format!("{id}.jsonl"))
}

fn session_fixture() -> Vec<serde_json::Value> {
    let mut records = session_meta_records();
    records.extend(conversation_records());
    records
}

fn session_meta_records() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "type": "session_meta",
            "format_version": 4,
            "session_id": SESSION_ID,
            "timestamp": "2026-08-14T12:00:00Z",
            "working_directory": "/work",
            "model": "test-model",
            "tools": ["bash", "read"],
            "cake_version": "0.1.0-test",
            "system_prompt": "You are cake.",
            "git": {
                "repository_url": "https://example.com/repo.git",
                "branch": "main",
                "commit_hash": "abc123",
            },
        }),
        serde_json::json!({
            "type": "task_start",
            "session_id": SESSION_ID,
            "task_id": TASK_ID,
            "timestamp": "2026-08-14T12:00:01Z",
        }),
        serde_json::json!({
            "type": "prompt_context",
            "session_id": SESSION_ID,
            "task_id": TASK_ID,
            "role": "developer",
            "content": "mutable context",
            "timestamp": "2026-08-14T12:00:01Z",
        }),
    ]
}

fn conversation_records() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "type": "message",
            "role": "user",
            "content": "Hello",
            "id": "msg-1",
            "status": "completed",
            "timestamp": "2026-08-14T12:00:02Z",
        }),
        serde_json::json!({
            "type": "function_call",
            "id": "fc-1",
            "call_id": "call-1",
            "name": "bash",
            "arguments": r#"{"cmd":"ls"}"#,
            "timestamp": "2026-08-14T12:00:03Z",
        }),
        serde_json::json!({
            "type": "function_call_output",
            "call_id": "call-1",
            "output": "result",
            "timestamp": "2026-08-14T12:00:04Z",
        }),
        serde_json::json!({
            "type": "skill_activated",
            "session_id": SESSION_ID,
            "task_id": TASK_ID,
            "timestamp": "2026-08-14T12:00:05Z",
            "name": "debugging-cake",
            "path": "/skills/debugging-cake/SKILL.md",
        }),
        serde_json::json!({
            "type": "hook_event",
            "timestamp": "2026-08-14T12:00:06Z",
            "task_id": TASK_ID,
            "event": "PreToolUse",
            "call_id": "call-1",
            "tool_name": "Bash",
            "source_file": "/hooks/hook.sh",
            "command": "printf ok",
            "exit_code": 0,
            "duration_ms": 5,
            "decision": "allow",
            "fail_closed": false,
            "stdout": "",
            "stderr": "",
        }),
        serde_json::json!({
            "type": "reasoning",
            "id": "r-1",
            "summary": ["step 1", "step 2"],
            "timestamp": "2026-08-14T12:00:07Z",
        }),
        serde_json::json!({
            "type": "task_complete",
            "subtype": "success",
            "is_error": false,
            "result": "Done",
            "duration_ms": 100,
            "turn_count": 1,
            "tool_call_count": 1,
            "session_id": SESSION_ID,
            "task_id": TASK_ID,
            "usage": {
                "input_tokens": 10,
                "input_tokens_details": { "cached_tokens": 2 },
                "output_tokens": 5,
                "output_tokens_details": { "reasoning_tokens": 3 },
                "total_tokens": 15,
            },
        }),
    ]
}

fn write_fixture(env: &TestEnv) {
    let lines: Vec<String> = session_fixture().iter().map(ToString::to_string).collect();
    fs::write(session_path(env, SESSION_ID), lines.join("\n") + "\n")
        .expect("failed to write session fixture");
}

fn write_large_fixture(env: &TestEnv) {
    let mut lines: Vec<String> = session_meta_records()
        .iter()
        .map(ToString::to_string)
        .collect();
    let content = "x".repeat(1024);
    lines.extend((0..4096).map(|index| {
        serde_json::json!({
            "type": "message",
            "role": "assistant",
            "content": content,
            "id": format!("msg-{index}"),
            "status": "completed",
            "timestamp": "2026-08-14T12:00:02Z",
        })
        .to_string()
    }));
    fs::write(session_path(env, SESSION_ID), lines.join("\n") + "\n")
        .expect("failed to write large session fixture");
}

fn parse_stream(stdout: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("stdout lines must be JSON"))
        .collect()
}

#[test]
fn replay_emits_complete_transcript_in_order() {
    let env = cake_env();
    write_fixture(&env);

    let output = env
        .command()
        .args(["--output-format", "stream-json", "replay", SESSION_ID])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute command");

    assert!(
        output.status.success(),
        "replay should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "successful replay should keep stderr empty, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let records = parse_stream(&output.stdout);
    let types: Vec<&str> = records
        .iter()
        .map(|record| record["type"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        types,
        vec![
            "session_meta",
            "task_start",
            "prompt_context",
            "message",
            "function_call",
            "function_call_output",
            "skill_activated",
            "hook_event",
            "reasoning",
            "task_complete",
        ],
        "transcript should preserve record order"
    );

    assert_eq!(records[0]["session_id"], SESSION_ID);
    assert_eq!(records[0]["model"], "test-model");
    assert_eq!(records[1]["task_id"], TASK_ID);
    assert_eq!(records[9]["subtype"], "success");
    assert_eq!(records[9]["usage"]["total_tokens"], 15);
}

#[test]
fn replay_exits_cleanly_when_consumer_closes_stdout() {
    let env = cake_env();
    write_large_fixture(&env);

    let mut child = env
        .command()
        .args(["--output-format", "stream-json", "replay", SESSION_ID])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn command");

    let mut stdout = BufReader::new(child.stdout.take().expect("stdout should be piped"));
    let mut first_line = String::new();
    stdout
        .read_line(&mut first_line)
        .expect("failed to read the first stream record");
    drop(stdout);

    let output = child
        .wait_with_output()
        .expect("failed to wait for command");
    assert_eq!(
        output.status.code(),
        Some(0),
        "a closed stream consumer should be a successful exit, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "a broken pipe should not produce a diagnostic, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !first_line.is_empty(),
        "the consumer should receive the first stream record"
    );
}

#[test]
fn replay_does_not_modify_the_session_file() {
    let env = cake_env();
    write_fixture(&env);
    let before = fs::read(session_path(&env, SESSION_ID)).expect("fixture should exist");

    let output = env
        .command()
        .args(["--output-format", "stream-json", "replay", SESSION_ID])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute command");

    assert!(output.status.success());
    let after = fs::read(session_path(&env, SESSION_ID)).expect("fixture should still exist");
    assert_eq!(
        before, after,
        "replay must not append to or rewrite the session file"
    );
}

#[test]
fn replay_missing_session_fails_with_structured_error() {
    let env = cake_env();

    let output = env
        .command()
        .args([
            "--output-format",
            "stream-json",
            "replay",
            "00000000-0000-0000-0000-000000000001",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute command");

    assert_eq!(
        output.status.code(),
        Some(3),
        "missing session is an input error"
    );
    let records = parse_stream(&output.stdout);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["type"], "replay_error");
    assert_eq!(records[0]["kind"], "session_not_found");
    assert_eq!(records[0]["exit_code"], 3);
}

#[test]
fn replay_invalid_uuid_fails_with_structured_error() {
    let env = cake_env();

    let output = env
        .command()
        .args(["--output-format", "stream-json", "replay", "not-a-uuid"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute command");

    assert_eq!(
        output.status.code(),
        Some(3),
        "invalid UUID is an input error"
    );
    let records = parse_stream(&output.stdout);
    assert_eq!(records[0]["type"], "replay_error");
    assert_eq!(records[0]["kind"], "invalid_uuid");
    assert!(records[0].get("session_id").is_none());
}

#[test]
fn replay_requires_stream_json_output_format() {
    let env = cake_env();
    write_fixture(&env);

    let output = env
        .command()
        .args(["replay", SESSION_ID])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute command");

    assert_eq!(
        output.status.code(),
        Some(3),
        "wrong output format is an input error"
    );
    let records = parse_stream(&output.stdout);
    assert_eq!(records[0]["type"], "replay_error");
    assert_eq!(records[0]["kind"], "output_format");
}

#[test]
fn replay_corrupt_session_fails_with_structured_error() {
    let env = cake_env();
    fs::write(
        session_path(&env, SESSION_ID),
        "{\"type\":\"session_meta\",\"format_version\":4}\n{ not json\n",
    )
    .expect("failed to write corrupt fixture");

    let output = env
        .command()
        .args(["--output-format", "stream-json", "replay", SESSION_ID])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute command");

    assert_eq!(
        output.status.code(),
        Some(1),
        "corrupt file is an agent error"
    );
    let records = parse_stream(&output.stdout);
    assert_eq!(records[0]["type"], "replay_error");
    assert_eq!(records[0]["kind"], "corrupt");
    assert_eq!(records[0]["exit_code"], 1);
}

#[test]
fn replay_unsupported_format_version_fails_with_structured_error() {
    let env = cake_env();
    fs::write(
        session_path(&env, SESSION_ID),
        serde_json::json!({
            "type": "session_meta",
            "format_version": 99,
            "session_id": SESSION_ID,
            "timestamp": "2026-08-14T12:00:00Z",
            "working_directory": "/work",
            "tools": [],
            "git": {},
        })
        .to_string()
            + "\n",
    )
    .expect("failed to write unsupported-format fixture");

    let output = env
        .command()
        .args(["--output-format", "stream-json", "replay", SESSION_ID])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute command");

    assert_eq!(
        output.status.code(),
        Some(1),
        "unsupported format version is an agent error"
    );
    let records = parse_stream(&output.stdout);
    assert_eq!(records[0]["type"], "replay_error");
    assert_eq!(records[0]["kind"], "unsupported_format");
}

#[test]
#[cfg(unix)]
fn replay_permission_denied_fails_with_structured_error() {
    use std::os::unix::fs::PermissionsExt;

    let env = cake_env();
    write_fixture(&env);
    let path = session_path(&env, SESSION_ID);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000))
        .expect("failed to make fixture unreadable");

    let output = env
        .command()
        .args(["--output-format", "stream-json", "replay", SESSION_ID])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute command");

    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .expect("failed to restore fixture permissions");

    assert_eq!(
        output.status.code(),
        Some(1),
        "permission error is an agent error"
    );
    let records = parse_stream(&output.stdout);
    assert_eq!(records[0]["type"], "replay_error");
    assert_eq!(records[0]["kind"], "permission");
}

#[test]
fn replay_tolerates_interrupted_final_record() {
    let env = cake_env();
    let mut lines: Vec<String> = session_fixture().iter().map(ToString::to_string).collect();
    // A writer interrupted mid-record leaves a partial final line with no
    // trailing newline; replay should emit the complete records and exit 0.
    lines.push("{\"type\":\"mess".to_string());
    fs::write(session_path(&env, SESSION_ID), lines.join("\n"))
        .expect("failed to write interrupted fixture");

    let output = env
        .command()
        .args(["--output-format", "stream-json", "replay", SESSION_ID])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute command");

    assert!(
        output.status.success(),
        "replay should exit 0 despite the interrupted tail, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records = parse_stream(&output.stdout);
    assert_eq!(records.len(), 10, "all complete records should be emitted");
    assert_eq!(records[9]["type"], "task_complete");
}

#[test]
#[cfg(unix)]
fn replay_session_dir_permission_denied_reports_permission() {
    use std::os::unix::fs::PermissionsExt;

    let env = cake_env();
    write_fixture(&env);
    // Without search permission on the sessions directory, `Path::exists`
    // would report `false`; replay must still classify this as a permission
    // failure, not a missing session.
    let dir = env.data_dir.join("sessions");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o000))
        .expect("failed to make sessions dir unsearchable");

    let output = env
        .command()
        .args(["--output-format", "stream-json", "replay", SESSION_ID])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute command");

    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .expect("failed to restore sessions dir permissions");

    assert_eq!(
        output.status.code(),
        Some(1),
        "directory permission error is an agent error"
    );
    let records = parse_stream(&output.stdout);
    assert_eq!(records[0]["type"], "replay_error");
    assert_eq!(records[0]["kind"], "permission");
}

#[test]
fn replay_mismatched_header_session_id_fails_as_corrupt() {
    let env = cake_env();
    let mut records = session_meta_records();
    records[0]["session_id"] = serde_json::json!("550e8400-e29b-41d4-a716-4466554400ff");
    let lines: Vec<String> = records.iter().map(ToString::to_string).collect();
    fs::write(session_path(&env, SESSION_ID), lines.join("\n") + "\n")
        .expect("failed to write mismatched fixture");

    let output = env
        .command()
        .args(["--output-format", "stream-json", "replay", SESSION_ID])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute command");

    assert_eq!(
        output.status.code(),
        Some(1),
        "identity mismatch is a corrupt-session error"
    );
    let records = parse_stream(&output.stdout);
    assert_eq!(records[0]["type"], "replay_error");
    assert_eq!(records[0]["kind"], "corrupt");
}
