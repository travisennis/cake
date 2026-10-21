//! Integration tests for exit codes.
//!
//! These tests verify that cake returns the correct exit code for each
//! failure mode:
//!
//! - 0 — success
//! - 1 — agent/tool error
//! - 2 — API error (rate limit, auth failure, network error, request timeout)
//! - 3 — input error (no prompt, invalid flags, bad configuration, missing API key)
//!
//! The `--fail-on-error` runs use an unroutable `127.0.0.1:1` endpoint so they
//! need no provider: the connection is refused after retry backoff, which
//! costs several seconds per run. Keep the number of those runs small.

#![expect(clippy::expect_used, reason = "test code uses expect for assertions")]

mod support;

use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdout, Stdio};

use support::TestEnv;

fn cake_env() -> TestEnv {
    TestEnv::new("cake-exit-test")
}

/// The env var the fixture model reads its API key from.
const FAIL_ON_ERROR_KEY: &str = "FAIL_ON_ERROR_TEST_KEY";

/// A port nothing listens on, so every connect is refused after retry backoff.
const UNREACHABLE_BASE_URL: &str = "http://127.0.0.1:1";

/// Provider settings that point the fixture model at `base_url`.
fn provider_settings(base_url: &str) -> String {
    format!(
        r#"
default_model = "test"

[[models]]
name = "test"
model = "test-model"
base_url = "{base_url}"
api_key_env = "{FAIL_ON_ERROR_KEY}"
api_type = "responses"
"#
    )
}

/// Spawn a `stream-json` run against `base_url`, optionally with the strict
/// exit-code flag.
fn spawn_stream_json(env: &TestEnv, base_url: &str, fail_on_error: bool) -> Child {
    env.write_project_settings(&provider_settings(base_url));
    let mut command = env.command();
    command
        .args(["--output-format", "stream-json"])
        .args(["--no-session"])
        .env(FAIL_ON_ERROR_KEY, "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if fail_on_error {
        command.arg("--fail-on-error");
    }
    command.arg("hello");
    command.spawn().expect("failed to spawn cake")
}

/// Spawn a `json` run against `base_url`, optionally with the strict
/// exit-code flag (which must be inert in this mode).
fn spawn_json(env: &TestEnv, base_url: &str, fail_on_error: bool) -> Child {
    env.write_project_settings(&provider_settings(base_url));
    let mut command = env.command();
    command
        .args(["--output-format", "json"])
        .args(["--no-session"])
        .env(FAIL_ON_ERROR_KEY, "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if fail_on_error {
        command.arg("--fail-on-error");
    }
    command.arg("hello");
    command.spawn().expect("failed to spawn cake")
}

/// Answer one provider request with an immediate `400` (never retried), the
/// agent-class provider failure the strict exit contract maps to `1`.
///
/// The request is read in full before the answer is written: a response that
/// lands while the client is still sending is a transport error to hyper
/// ("received unexpected message from connection"), and cake then retries --
/// a connection this one-shot fixture never answers, hanging the test.
fn answer_bad_request(stream: &mut std::net::TcpStream) {
    use std::io::{Read as _, Write as _};

    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .expect("fixture must bound its read");
    let mut request = Vec::new();
    let mut buffer = [0_u8; 8192];
    let (body_start, content_length) = loop {
        let read = stream
            .read(&mut buffer)
            .expect("fixture should read the request");
        assert!(read > 0, "client closed before sending a full request");
        request.extend_from_slice(&buffer[..read]);
        if let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            let content_length = String::from_utf8_lossy(&request[..position])
                .lines()
                .find_map(|line| {
                    let line = line.to_ascii_lowercase();
                    line.strip_prefix("content-length: ")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            break (position + 4, content_length);
        }
    };
    while request.len() < body_start + content_length {
        let read = stream
            .read(&mut buffer)
            .expect("fixture should read the request body");
        assert!(read > 0, "client closed before sending the request body");
        request.extend_from_slice(&buffer[..read]);
    }

    let body = r#"{"error":{"message":"bad request","type":"invalid_request_error"}}"#;
    let response = format!(
        "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .expect("fixture should answer the provider request");
}

/// Read every remaining `stream-json` record from `reader`.
fn read_records(reader: &mut impl BufRead) -> Vec<serde_json::Value> {
    let mut records = Vec::new();
    for line in reader.lines() {
        let line = line.expect("stream line should be readable");
        if line.trim().is_empty() {
            continue;
        }
        records.push(serde_json::from_str(&line).expect("stream-json records must be JSON"));
    }
    records
}

/// The final `task_complete` record's `subtype` and `is_error` fields. The
/// record is the run's outcome channel, so it must be the last record written.
///
/// `status` and `stderr` feed the panic messages: a missing `task_complete`
/// usually means the process died, and the exit status names the signal.
fn task_complete_outcome(
    records: &[serde_json::Value],
    status: std::process::ExitStatus,
    stderr: &[u8],
) -> (String, bool) {
    let stderr = String::from_utf8_lossy(stderr);
    let Some(record) = records.last() else {
        panic!("stream-json run must emit records; exit status: {status:?}; stderr: {stderr}");
    };
    assert_eq!(
        record["type"], "task_complete",
        "the final record must be task_complete; records: {records:?}; exit status: {status:?}; stderr: {stderr}"
    );
    (
        record["subtype"].as_str().unwrap_or_default().to_string(),
        record["is_error"].as_bool().unwrap_or(false),
    )
}

// --- Exit code 0: success ---

#[test]
fn test_help_exits_zero() {
    let env = cake_env();
    let output = env
        .command()
        .arg("--help")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to execute command");

    assert!(output.status.success(), "--help should exit 0");
}

#[test]
fn test_version_exits_zero() {
    let env = cake_env();
    let output = env
        .command()
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to execute command");

    assert!(output.status.success(), "--version should exit 0");
}

// --- Exit code 3: input error ---

#[test]
fn test_prompt_with_subcommand_exits_three() {
    let env = cake_env();
    let output = env
        .command()
        .args(["ignored prompt", "sessions", "list"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to execute command");

    let code = output.status.code().unwrap_or(-1);
    assert_eq!(
        code, 3,
        "Prompt with a subcommand should exit 3, got {code}"
    );
    assert!(output.stdout.is_empty(), "rejected input must not dispatch");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid usage"), "Stderr: {stderr}");
    assert!(stderr.contains("prompt"), "Stderr: {stderr}");
    assert!(stderr.contains("sessions"), "Stderr: {stderr}");
}

#[test]
fn test_run_only_option_with_subcommand_exits_three() {
    let env = cake_env();
    let output = env
        .command()
        .args([
            "--output-format",
            "stream-json",
            "--output-schema",
            "result.schema.json",
            "replay",
            "550e8400-e29b-41d4-a716-446655440000",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to execute command");

    let code = output.status.code().unwrap_or(-1);
    assert_eq!(
        code, 3,
        "Run-only option with a subcommand should exit 3, got {code}"
    );
    assert!(output.stdout.is_empty(), "rejected input must not dispatch");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--output-schema"), "Stderr: {stderr}");
    assert!(stderr.contains("replay"), "Stderr: {stderr}");
}

#[test]
fn test_no_prompt_exits_three() {
    let env = cake_env();
    let output = env
        .command()
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to execute command");

    let code = output.status.code().unwrap_or(-1);
    assert_eq!(code, 3, "No prompt should exit 3, got {code}");
}

#[test]
fn test_invalid_flag_exits_three() {
    let env = cake_env();
    let output = env
        .command()
        .arg("--bogus-flag")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to execute command");

    let code = output.status.code().unwrap_or(-1);
    assert_eq!(code, 3, "Invalid flag should exit 3, got {code}");
}

#[test]
fn test_no_model_configured_exits_three() {
    let env = cake_env();
    let output = env
        .command()
        .arg("test prompt")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to execute command");

    let code = output.status.code().unwrap_or(-1);
    assert_eq!(code, 3, "Missing model config should exit 3, got {code}");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No model specified"),
        "Error message should mention missing model config. Stderr: {stderr}"
    );
}

#[test]
fn test_unparsable_project_settings_exits_three() {
    let env = cake_env();
    env.write_project_settings("not valid toml = [");

    let output = env
        .command()
        .arg("test prompt")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to execute command");

    let code = output.status.code().unwrap_or(-1);
    assert_eq!(code, 3, "Unparsable settings should exit 3, got {code}");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Failed to parse settings file"),
        "Error message should name the settings parse failure. Stderr: {stderr}"
    );
}

#[test]
fn test_missing_api_key_exits_three() {
    let env = cake_env();
    env.write_project_settings(
        r#"
default_model = "test"

[[models]]
name = "test"
model = "glm-5.1"
base_url = "https://example.com"
api_key_env = "TEST_API_KEY"
"#,
    );

    let output = env
        .command()
        .arg("test prompt")
        .env_remove("TEST_API_KEY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to execute command");

    let code = output.status.code().unwrap_or(-1);
    assert_eq!(code, 3, "Missing API key should exit 3, got {code}");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("TEST_API_KEY"),
        "Error message should mention the configured env var. Stderr: {stderr}"
    );
}

#[test]
fn test_unknown_model_exits_three() {
    let env = cake_env();
    let output = env
        .command()
        .arg("--model")
        .arg("nonexistent-model")
        .arg("test prompt")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to execute command");

    let code = output.status.code().unwrap_or(-1);
    assert_eq!(code, 3, "Unknown model should exit 3, got {code}");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Unknown model 'nonexistent-model'"),
        "Error message should mention the unknown model. Stderr: {stderr}"
    );
}

#[test]
fn test_invalid_session_uuid_exits_three() {
    let env = cake_env();
    let output = env
        .command()
        .arg("--resume")
        .arg("not-a-uuid")
        .arg("test prompt")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to execute command");

    let code = output.status.code().unwrap_or(-1);
    assert_eq!(code, 3, "Invalid session UUID should exit 3, got {code}");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Invalid session UUID"),
        "Error should reject non-UUID resume values. Stderr: {stderr}"
    );
}

#[test]
fn test_removed_continue_flag_exits_three_without_starting_a_session() {
    let env = cake_env();
    let output = env
        .command()
        .args(["--continue", "test prompt"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to execute command");

    let code = output.status.code().unwrap_or(-1);
    assert_eq!(code, 3, "Removed flag should exit 3, got {code}");
    assert!(output.stdout.is_empty(), "rejected input must not dispatch");
    assert!(
        !env.data_dir.join("sessions").exists(),
        "rejected input must not create session state"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unexpected argument '--continue'"),
        "Error should identify the removed flag. Stderr: {stderr}"
    );
}

#[test]
fn test_resume_and_fork_conflict_exits_three() {
    let env = cake_env();
    let output = env
        .command()
        .arg("--resume")
        .arg("550e8400-e29b-41d4-a716-446655440000")
        .arg("--fork")
        .arg("test prompt")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to execute command");

    let code = output.status.code().unwrap_or(-1);
    assert_eq!(
        code, 3,
        "Conflicting session mode flags should exit 3, got {code}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot be used with"),
        "Error should mention the flag conflict. Stderr: {stderr}"
    );
}

#[test]
fn test_no_session_and_restore_mode_conflict_exits_three() {
    let env = cake_env();
    let output = env
        .command()
        .arg("--no-session")
        .arg("--resume")
        .arg("550e8400-e29b-41d4-a716-446655440000")
        .arg("test prompt")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to execute command");

    let code = output.status.code().unwrap_or(-1);
    assert_eq!(
        code, 3,
        "Conflicting session mode flags should exit 3, got {code}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot be used with"),
        "Error should mention the flag conflict. Stderr: {stderr}"
    );
}

// --- Exit codes for --fail-on-error with stream-json ---

#[test]
fn test_fail_on_error_with_subcommand_exits_three() {
    let env = cake_env();
    let output = env
        .command()
        .args([
            "--output-format",
            "stream-json",
            "--fail-on-error",
            "ignored prompt",
            "sessions",
            "list",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to execute command");

    let code = output.status.code().unwrap_or(-1);
    assert_eq!(
        code, 3,
        "--fail-on-error is a run-only option and should exit 3, got {code}"
    );
    assert!(output.stdout.is_empty(), "rejected input must not dispatch");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid usage")
            && stderr.contains("cannot be used with subcommand 'sessions'"),
        "the rejection must come from run-option validation, not clap's unknown-flag path. Stderr: {stderr}"
    );
}

#[test]
fn test_stream_json_failure_exits_zero_without_fail_on_error() {
    let env = cake_env();
    let child = spawn_stream_json(&env, UNREACHABLE_BASE_URL, false);
    let output = child.wait_with_output().expect("failed to wait for cake");

    let records = read_records(&mut output.stdout.as_slice());
    let (subtype, is_error) = task_complete_outcome(&records, output.status, &output.stderr);
    assert_eq!(subtype, "error_during_execution");
    assert!(is_error, "provider failure must be an error outcome");
    assert_eq!(
        output.status.code(),
        Some(0),
        "the default stream-json contract reports the failure in the stream and exits 0"
    );
}

#[test]
fn test_fail_on_error_stream_json_network_failure_exits_two() {
    let env = cake_env();
    let child = spawn_stream_json(&env, UNREACHABLE_BASE_URL, true);
    let output = child.wait_with_output().expect("failed to wait for cake");

    // The stream is unchanged: the failure is still the final task_complete.
    let records = read_records(&mut output.stdout.as_slice());
    let (subtype, is_error) = task_complete_outcome(&records, output.status, &output.stderr);
    assert_eq!(subtype, "error_during_execution");
    assert!(is_error, "provider failure must be an error outcome");
    assert_eq!(
        output.status.code(),
        Some(2),
        "a refused connection is the API/network class: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A provider that answers `400` immediately (never retried) is an
/// agent-class failure, so `--fail-on-error` exits `1` --- the class the other
/// CLI cases did not cover.
#[test]
fn test_fail_on_error_stream_json_agent_class_failure_exits_one() {
    use std::net::TcpListener;

    let env = cake_env();
    let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener should bind");
    let base_url = format!(
        "http://{}",
        listener
            .local_addr()
            .expect("listener should have an address")
    );
    let child = spawn_stream_json(&env, &base_url, true);

    let (mut stream, _peer) = listener.accept().expect("cake should reach the provider");
    answer_bad_request(&mut stream);

    let output = child.wait_with_output().expect("failed to wait for cake");
    let records = read_records(&mut output.stdout.as_slice());
    let (subtype, is_error) = task_complete_outcome(&records, output.status, &output.stderr);
    assert_eq!(subtype, "error_during_execution");
    assert!(is_error, "provider failure must be an error outcome");
    assert_eq!(
        output.status.code(),
        Some(1),
        "a provider 400 is the agent class: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `--fail-on-error` is accepted and inert with `json`, whose runs already
/// exit with the failure's class and still print their document.
#[test]
fn test_fail_on_error_is_inert_with_json_output_format() {
    use std::net::TcpListener;

    let env = cake_env();
    let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener should bind");
    let base_url = format!(
        "http://{}",
        listener
            .local_addr()
            .expect("listener should have an address")
    );
    let child = spawn_json(&env, &base_url, true);

    let (mut stream, _peer) = listener.accept().expect("cake should reach the provider");
    answer_bad_request(&mut stream);

    let output = child.wait_with_output().expect("failed to wait for cake");
    assert_eq!(
        output.status.code(),
        Some(1),
        "json already exits with the failure's class: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("json mode must print one document");
    assert!(
        document["error"].is_string() && document["result"].is_null(),
        "the failure stays in the document's error position: {document}"
    );
}

#[test]
fn test_fail_on_error_stream_json_broken_pipe_exits_zero() {
    let env = cake_env();
    let mut child = spawn_stream_json(&env, UNREACHABLE_BASE_URL, true);

    // Model a `head -1` consumer: closing stdout is its choice, not a failure.
    drop(child.stdout.take());
    let output = child.wait_with_output().expect("failed to wait for cake");

    assert_eq!(
        output.status.code(),
        Some(0),
        "closed output should still exit 0: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Broken pipe") && !stderr.contains("panicked"),
        "closed output must not panic or report a broken pipe: {stderr}"
    );
}

/// A provider that accepts the request and never answers keeps the agent turn
/// in flight. The signal handlers are installed before the turn starts, so
/// accepting the connection is a safe point to interrupt.
#[cfg(unix)]
#[test]
fn test_fail_on_error_stream_json_interrupt_exits_130() {
    use std::io::Read as _;
    use std::net::TcpListener;

    let env = cake_env();
    let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener should bind");
    let base_url = format!(
        "http://{}",
        listener
            .local_addr()
            .expect("listener should have an address")
    );
    let mut child = spawn_stream_json(&env, &base_url, true);
    let stdout: ChildStdout = child.stdout.take().expect("stdout should be piped");
    let mut reader = BufReader::new(stdout);

    let (_request, _peer) = listener.accept().expect("cake should reach the provider");

    // SAFETY: `child.id()` is the cake process this test just spawned.
    unsafe {
        libc::kill(
            i32::try_from(child.id()).expect("pid must fit in an i32"),
            libc::SIGINT,
        );
    }

    let status = child.wait().expect("failed to wait for cake");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr should be piped")
        .read_to_string(&mut stderr)
        .expect("cake stderr should be readable");
    let records = read_records(&mut reader);
    let (subtype, is_error) = task_complete_outcome(&records, status, stderr.as_bytes());
    assert_eq!(subtype, "interrupted");
    assert!(is_error, "an interrupted task is an error outcome");
    assert_eq!(
        status.code(),
        Some(130),
        "interruption keeps 130 with --fail-on-error"
    );
}
