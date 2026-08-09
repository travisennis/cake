//! End-to-end coverage for the session modes that restore or fork a prior
//! session (`--continue`, `--fork <UUID>`): the run resolves the restored
//! session's model, attaches the command-safety judge context, and reaches
//! the provider, exercising the `ContinueLatest` and `Fork` arms of
//! `build_client_and_session`.

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
        "model": "test",
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
async fn continue_restores_latest_session_and_reaches_provider() {
    let env = TestEnv::new("cake-continue-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    write_session_fixture(&env, &minimal_records(&env));

    let output = run_mode(&env, &mock_server, &["--continue", "carry on"]).await;
    assert!(
        output.status.success(),
        "continue should succeed. stderr: {}",
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
