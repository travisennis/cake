//! Resume of a session whose prior process died between persisting a tool call
//! and its output.

#![expect(clippy::expect_used, reason = "test code uses expect for assertions")]

mod support;

use std::{fs, process::Stdio};

use support::TestEnv;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SESSION_ID: &str = "550e8400-e29b-41d4-a716-446655440000";

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
api_key_env = "RESUME_REPAIR_TEST_KEY"
api_type = "responses"
"#
    ));
}

/// Write a session file whose last record is a `function_call` with no output.
fn write_session_fixture(env: &TestEnv, records: &[serde_json::Value]) -> String {
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
    contents
}

fn session_meta(env: &TestEnv) -> serde_json::Value {
    serde_json::json!({
        "type": "session_meta",
        "format_version": 4,
        "session_id": SESSION_ID,
        "timestamp": "2026-07-27T12:00:00Z",
        "working_directory": env.workspace_dir,
        "model": "test",
        "tools": ["Bash", "Edit", "Read", "Write"],
    })
}

fn interrupted_records(env: &TestEnv) -> Vec<serde_json::Value> {
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
        serde_json::json!({
            "type": "function_call",
            "id": "fc-1",
            "call_id": "call-1",
            "name": "Bash",
            "arguments": "{\"command\":\"ls\"}",
            "timestamp": "2026-07-27T12:00:03Z",
        }),
    ]
}

fn session_lines(env: &TestEnv) -> Vec<serde_json::Value> {
    let contents = read_session_file(env);
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("session line should be valid JSON"))
        .collect()
}

fn read_session_file(env: &TestEnv) -> String {
    fs::read_to_string(
        env.data_dir
            .join("sessions")
            .join(format!("{SESSION_ID}.jsonl")),
    )
    .expect("session file should be readable")
}

fn run_resume(env: &TestEnv) -> std::process::Output {
    env.command()
        .arg("--resume")
        .arg(SESSION_ID)
        .arg("carry on")
        .env("RESUME_REPAIR_TEST_KEY", "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake")
}

#[tokio::test]
async fn resume_repairs_incomplete_tool_call_and_preserves_prior_bytes() {
    let env = TestEnv::new("cake-resume-repair-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    let prefix = write_session_fixture(&env, &interrupted_records(&env));

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    let output = run_resume(&env);
    assert!(
        output.status.success(),
        "cake should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The prior process's records are untouched; repair only appends.
    let contents = read_session_file(&env);
    assert!(
        contents.starts_with(&prefix),
        "resume must not rewrite prior session bytes"
    );

    let appended = &session_lines(&env)[interrupted_records(&env).len()..];
    let repair_index = appended
        .iter()
        .position(|record| record["type"] == "function_call_output")
        .expect("resume should append a repair output");
    let repair = &appended[repair_index];
    assert_eq!(repair["call_id"], "call-1");
    assert_eq!(
        repair["output"],
        "not executed: the previous cake process ended before Bash(call-1) recorded a \
         result. Assume the tool did not run, and call it again if its result is still \
         needed."
    );

    // The repair precedes the new user message in the appended records.
    let user_index = appended
        .iter()
        .position(|record| record["type"] == "message" && record["role"] == "user")
        .expect("new user message should be present");
    assert!(repair_index < user_index);

    // The provider request carries the repaired pairing.
    let requests = mock_server
        .received_requests()
        .await
        .expect("recorded requests");
    let body: serde_json::Value = requests[0]
        .body_json()
        .expect("request body should be JSON");
    let input = body["input"].as_array().expect("input array");
    let types = input
        .iter()
        .map(|item| item["type"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    let call_index = types
        .iter()
        .position(|kind| *kind == "function_call")
        .expect("request should include the restored function call");
    let output_index = types
        .iter()
        .position(|kind| *kind == "function_call_output")
        .expect("request should include the repair output");
    assert_eq!(input[call_index]["call_id"], "call-1");
    assert_eq!(input[output_index]["call_id"], "call-1");
    assert_eq!(output_index, call_index + 1);
}

#[tokio::test]
async fn resume_of_matched_history_appends_no_repair_records() {
    let env = TestEnv::new("cake-resume-matched-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());

    let mut records = interrupted_records(&env);
    records.push(serde_json::json!({
        "type": "function_call_output",
        "call_id": "call-1",
        "output": "AGENTS.md\nCargo.toml",
        "timestamp": "2026-07-27T12:00:04Z",
    }));
    records.push(serde_json::json!({
        "type": "message",
        "role": "assistant",
        "content": "Two files.",
        "timestamp": "2026-07-27T12:00:05Z",
    }));
    let record_count = records.len();
    write_session_fixture(&env, &records);

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    let output = run_resume(&env);
    assert!(
        output.status.success(),
        "cake should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let appended = session_lines(&env)[record_count..].to_vec();
    let outputs = appended
        .iter()
        .filter(|record| record["type"] == "function_call_output")
        .count();
    assert_eq!(
        outputs, 0,
        "a fully matched history must not gain synthetic outputs: {appended:?}"
    );
}

#[tokio::test]
async fn resume_of_ambiguous_history_fails_without_calling_the_provider() {
    let env = TestEnv::new("cake-resume-ambiguous-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());

    let mut records = interrupted_records(&env);
    records.push(serde_json::json!({
        "type": "function_call",
        "id": "fc-2",
        "call_id": "call-1",
        "name": "Read",
        "arguments": "{\"file_path\":\"AGENTS.md\"}",
        "timestamp": "2026-07-27T12:00:04Z",
    }));
    let prefix = write_session_fixture(&env, &records);

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .expect(0)
        .mount(&mock_server)
        .await;

    let output = run_resume(&env);
    assert!(!output.status.success(), "ambiguous history must not run");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("two unfinished function calls share call_id 'call-1'"),
        "stderr should name the ambiguity: {stderr}"
    );
    assert_eq!(
        read_session_file(&env),
        prefix,
        "a failed restore must not modify the session file"
    );
}
