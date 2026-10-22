//! End-to-end coverage for the session modes that restore or fork a prior
//! session (`--resume <UUID>`, `--fork <UUID>`): the run resolves the
//! restored session's model and reaches the provider, exercising the `Resume`
//! and `Fork` arms of `build_client_and_session`. The command-safety judge
//! context those arms attach is unit-tested in `session_factory` (these e2e
//! tests never invoke Bash, so they cannot observe the wiring themselves).

#![expect(clippy::expect_used, reason = "test code uses expect for assertions")]

mod support;

use std::{fs, process::Stdio};

use support::TestEnv;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SESSION_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
const TEST_KEY: &str = "SESSION_MODES_TEST_KEY";

fn success_response() -> serde_json::Value {
    serde_json::json!({
        "id": "resp-123",
        "output": [
            {
                "type": "message",
                "id": "msg-1",
                "status": "completed",
                "content": [{ "type": "output_text", "text": "Done." }]
            }
        ],
        "usage": { "input_tokens": 10, "output_tokens": 5, "total_tokens": 15 }
    })
}

fn write_responses_settings(env: &TestEnv, base_url: &str) {
    env.write_project_settings(&format!(
        r#"
default_model = "test"

[[models]]
name = "test"
model = "glm-5.1"
base_url = "{base_url}"
api_key_env = "{TEST_KEY}"
api_type = "responses"
"#
    ));
}

fn session_meta(env: &TestEnv) -> serde_json::Value {
    // `current_dir` resolves symlinks (e.g. `/var` -> `/private/var` on macOS),
    // and `load_latest_session` compares the working directory by exact path,
    // so canonicalize the fixture's working directory.
    let working_directory = fs::canonicalize(&env.workspace_dir).unwrap_or_else(|_| {
        panic!(
            "workspace dir should canonicalize: {}",
            env.workspace_dir.display()
        )
    });
    serde_json::json!({
        "type": "session_meta",
        "format_version": 4,
        "session_id": SESSION_ID,
        "timestamp": "2026-07-27T12:00:00Z",
        "working_directory": working_directory,
        "model": "glm-5.1",
        "model_config": "test",
        "tools": ["Bash", "Edit", "Read", "Write"],
    })
}

fn minimal_records(env: &TestEnv) -> Vec<serde_json::Value> {
    vec![
        session_meta(env),
        serde_json::json!({
            "type": "task_start",
            "session_id": SESSION_ID,
            "task_id": "550e8400-e29b-41d4-a716-446655440001",
            "timestamp": "2026-07-27T12:00:01Z",
        }),
        serde_json::json!({
            "type": "message",
            "role": "user",
            "content": "list the files",
            "timestamp": "2026-07-27T12:00:02Z",
        }),
    ]
}

fn write_session_fixture(env: &TestEnv, records: &[serde_json::Value]) {
    let sessions_dir = env.data_dir.join("sessions");
    fs::create_dir_all(&sessions_dir).expect("failed to create sessions directory");
    let contents = records
        .iter()
        .map(|record| serde_json::to_string(record).expect("record should serialize"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(sessions_dir.join(format!("{SESSION_ID}.jsonl")), &contents)
        .expect("failed to write session fixture");
}

fn session_file_count(env: &TestEnv) -> usize {
    fs::read_dir(env.data_dir.join("sessions"))
        .expect("sessions directory should exist")
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
        .count()
}

/// Mount a mock Responses backend that answers one call, then run `cake` with
/// the given mode args against the seeded session.
async fn run_mode(env: &TestEnv, mock_server: &MockServer, args: &[&str]) -> std::process::Output {
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .expect(1)
        .mount(mock_server)
        .await;

    env.command()
        .args(args)
        .env(TEST_KEY, "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake")
}

#[tokio::test]
async fn resume_restores_named_session_and_reaches_provider() {
    let env = TestEnv::new("cake-resume-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    write_session_fixture(&env, &minimal_records(&env));

    let output = run_mode(&env, &mock_server, &["--resume", SESSION_ID, "carry on"]).await;
    assert!(
        output.status.success(),
        "resume should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let requests = mock_server
        .received_requests()
        .await
        .expect("recorded requests");
    assert_eq!(requests.len(), 1, "one provider call expected");
    let body = requests[0].body.clone();
    let body_text = String::from_utf8_lossy(&body);
    assert!(
        body_text.contains("list the files"),
        "restored history should reach the provider, got: {body_text}"
    );
    assert!(
        body_text.contains("carry on"),
        "new prompt should reach the provider, got: {body_text}"
    );
}

#[tokio::test]
async fn fork_creates_new_session_and_reaches_provider() {
    let env = TestEnv::new("cake-fork-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    write_session_fixture(&env, &minimal_records(&env));

    let output = run_mode(&env, &mock_server, &["--fork", SESSION_ID, "branch off"]).await;
    assert!(
        output.status.success(),
        "fork should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let requests = mock_server
        .received_requests()
        .await
        .expect("recorded requests");
    assert_eq!(requests.len(), 1, "one provider call expected");

    assert_eq!(
        session_file_count(&env),
        2,
        "fork should persist a new session beside the seeded one"
    );
}

/// The live stream's first record reports the model identity the run resolved,
/// so a streaming consumer never has to guess from `--model` or read settings.
#[tokio::test]
async fn resume_stream_task_start_reports_the_resolved_model_identity() {
    let env = TestEnv::new("cake-resume-identity-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    write_session_fixture(&env, &minimal_records(&env));

    let output = run_mode(
        &env,
        &mock_server,
        &[
            "--output-format",
            "stream-json",
            "--resume",
            SESSION_ID,
            "carry on",
        ],
    )
    .await;
    assert!(
        output.status.success(),
        "resume should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stream = String::from_utf8(output.stdout).expect("stream-json should be UTF-8");
    let first_line = stream.lines().next().expect("the stream should start");
    let first: serde_json::Value = serde_json::from_str(first_line).expect("valid first record");
    assert_eq!(first["type"], "task_start");
    assert_eq!(first["model"], "glm-5.1");
    assert_eq!(first["model_config"], "test");
}

// -------------------------------------------------------------------------
// Context checkpoints on resume and fork
// -------------------------------------------------------------------------

fn checkpoint_record(summary: &str, retained_content: &str, call_id: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "compaction",
        "checkpoint_version": 1,
        "checkpoint_id": format!("checkpoint-{summary}"),
        "session_id": SESSION_ID,
        "task_id": "550e8400-e29b-41d4-a716-446655440001",
        "timestamp": "2026-07-27T12:00:03Z",
        "trigger": "threshold",
        "summary": summary,
        "retained": [
            {
                "type": "message",
                "role": "user",
                "content": retained_content,
                "origin": "user",
                "pin": { "kind": "user_input" }
            },
            {
                "type": "function_call",
                "id": format!("fc-{call_id}"),
                "call_id": call_id,
                "name": "Read",
                "arguments": "{\"file_path\":\"AGENTS.md\"}"
            },
            {
                "type": "function_call_output",
                "call_id": call_id,
                "output": format!("contents of {call_id}")
            }
        ],
        "model": { "model": "glm-5.1", "model_config": "test" },
        "summary_instruction_version": 1,
        "estimated_input": {
            "before_tokens": 900,
            "after_tokens": 200,
            "basis": "content_class"
        }
    })
}

/// A format-4 session whose two checkpoints retire the first two records.
fn checkpointed_records(env: &TestEnv) -> Vec<serde_json::Value> {
    vec![
        session_meta(env),
        serde_json::json!({
            "type": "task_start",
            "session_id": SESSION_ID,
            "task_id": "550e8400-e29b-41d4-a716-446655440001",
            "timestamp": "2026-07-27T12:00:01Z",
        }),
        serde_json::json!({
            "type": "message",
            "role": "user",
            "content": "retired request",
            "timestamp": "2026-07-27T12:00:02Z",
        }),
        checkpoint_record("older summary", "retained older", "call-older"),
        serde_json::json!({
            "type": "message",
            "role": "assistant",
            "content": "intervening answer",
            "timestamp": "2026-07-27T12:00:04Z",
        }),
        checkpoint_record("newer summary", "retained latest", "call-latest"),
        serde_json::json!({
            "type": "message",
            "role": "user",
            "content": "new request",
            "timestamp": "2026-07-27T12:00:05Z",
        }),
    ]
}

#[tokio::test]
async fn resume_across_two_checkpoints_sends_only_the_latest() {
    let env = TestEnv::new("cake-resume-checkpoint-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    write_session_fixture(&env, &checkpointed_records(&env));

    let output = run_mode(&env, &mock_server, &["--resume", SESSION_ID, "carry on"]).await;
    assert!(
        output.status.success(),
        "resume should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let requests = mock_server
        .received_requests()
        .await
        .expect("recorded requests");
    assert_eq!(requests.len(), 1, "one provider call expected");
    let body = String::from_utf8_lossy(&requests[0].body).to_string();
    assert!(
        body.contains("newer summary"),
        "the latest summary reaches the provider: {body}"
    );
    assert!(
        body.contains("retained latest") && body.contains("call-latest"),
        "the latest retained tail reaches the provider: {body}"
    );
    assert!(
        body.contains("new request") && body.contains("carry on"),
        "records after the boundary and the new prompt stay in order: {body}"
    );
    for retired in [
        "older summary",
        "retained older",
        "call-older",
        "retired request",
        "intervening answer",
    ] {
        assert!(
            !body.contains(retired),
            "pre-boundary content {retired:?} must not be emitted: {body}"
        );
    }
}

#[tokio::test]
async fn fork_reconstructs_the_checkpoint_boundary_in_the_new_session() {
    let env = TestEnv::new("cake-fork-checkpoint-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    write_session_fixture(&env, &checkpointed_records(&env));

    let output = run_mode(&env, &mock_server, &["--fork", SESSION_ID, "branch off"]).await;
    assert!(
        output.status.success(),
        "fork should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let sessions_dir = env.data_dir.join("sessions");
    let new_file = fs::read_dir(&sessions_dir)
        .expect("sessions directory should exist")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.extension().is_some_and(|ext| ext == "jsonl")
                && path.file_stem().and_then(|stem| stem.to_str()) != Some(SESSION_ID)
        })
        .expect("fork should create a new session file");
    let contents = fs::read_to_string(&new_file).expect("new session should be readable");
    let records: Vec<serde_json::Value> = contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("session line should be JSON"))
        .collect();
    let new_session_id = records[0]["session_id"]
        .as_str()
        .expect("new session id")
        .to_string();
    let checkpoints: Vec<_> = records
        .iter()
        .filter(|record| record["type"] == "compaction")
        .collect();
    assert_eq!(checkpoints.len(), 1, "only the latest checkpoint is copied");
    assert_eq!(checkpoints[0]["checkpoint_id"], "checkpoint-newer summary");
    assert_eq!(
        checkpoints[0]["session_id"], new_session_id,
        "the copied checkpoint names the new session"
    );
    let checkpoint_index = records
        .iter()
        .position(|record| record["type"] == "compaction")
        .expect("copied checkpoint");
    let later_request = records
        .iter()
        .position(|record| record["type"] == "message" && record["content"] == "new request")
        .expect("later request copied");
    assert!(
        checkpoint_index < later_request,
        "the checkpoint keeps its original position among copied records"
    );
    assert!(
        !contents.contains("older summary"),
        "a fork must not copy a superseded checkpoint"
    );

    let requests = mock_server
        .received_requests()
        .await
        .expect("recorded requests");
    assert_eq!(requests.len(), 1, "one provider call expected");
    let body = String::from_utf8_lossy(&requests[0].body).to_string();
    assert!(
        body.contains("newer summary") && body.contains("retained latest"),
        "the forked request uses the copied checkpoint: {body}"
    );
    assert!(
        !body.contains("retired request") && !body.contains("intervening answer"),
        "the forked request omits pre-boundary raw records: {body}"
    );
}

#[tokio::test]
async fn resume_rejects_an_unknown_checkpoint_version_without_a_provider_call() {
    let env = TestEnv::new("cake-resume-unknown-checkpoint-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    let mut records = checkpointed_records(&env);
    records[5]["checkpoint_version"] = serde_json::json!(2);
    write_session_fixture(&env, &records);

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .expect(0)
        .mount(&mock_server)
        .await;

    let output = env
        .command()
        .args(["--resume", SESSION_ID, "carry on"])
        .env(TEST_KEY, "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert!(!output.status.success(), "an unknown checkpoint must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Unsupported compaction checkpoint_version: 2"),
        "the error must name the checkpoint version: {stderr}"
    );
}
