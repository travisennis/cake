//! End-to-end coverage for content-addressed tool outputs.
//!
//! These tests drive the real binary against a mocked Responses provider. They
//! prove that an above-threshold Bash result is stored once as a blob, that
//! resume and replay restore its exact text, that a below-threshold result stays
//! inline, and that a legacy inline result keeps loading unchanged.

#![expect(clippy::expect_used, reason = "test code uses expect for assertions")]

mod support;

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Output, Stdio},
};

use sha2::{Digest, Sha256};
use support::TestEnv;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TEST_KEY: &str = "SESSION_TOOL_OUTPUTS_TEST_KEY";
const INITIAL_PROMPT: &str = "run the command";
const ABOVE_THRESHOLD_XS: usize = 20 * 1024;
const BELOW_THRESHOLD_XS: usize = 15 * 1024;
const LEGACY_SESSION_ID: &str = "550e8400-e29b-41d4-a716-446655440000";

fn write_responses_settings(env: &TestEnv, base_url: &str) {
    env.write_global_settings(&format!(
        r#"
[[models]]
name = "test"
model = "glm-5.1"
base_url = "{base_url}"
api_key_env = "{TEST_KEY}"
api_type = "responses"
"#
    ));
    env.write_project_settings(
        r#"
default_model = "test"
"#,
    );
}

fn output_command(count: usize) -> String {
    format!("printf 'x%.0s' {{1..{count}}}")
}

fn tool_call_response(command: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "resp-output-tool",
        "output": [
            {
                "type": "function_call",
                "id": "fc-bash",
                "call_id": "call-output",
                "name": "Bash",
                "arguments": serde_json::json!({ "command": command }).to_string()
            }
        ],
        "usage": { "input_tokens": 10, "output_tokens": 5, "total_tokens": 15 }
    })
}

fn final_response(text: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "resp-final",
        "output": [
            {
                "type": "message",
                "id": "msg-final",
                "status": "completed",
                "content": [{ "type": "output_text", "text": text }]
            }
        ],
        "usage": { "input_tokens": 20, "output_tokens": 5, "total_tokens": 25 }
    })
}

async fn mount_tool_turn(mock_server: &MockServer, command: &str) {
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tool_call_response(command)))
        .up_to_n_times(1)
        .expect(1)
        .mount(mock_server)
        .await;
    mount_final_turn(mock_server, "The command finished.").await;
}

async fn mount_final_turn(mock_server: &MockServer, text: &str) {
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_response(text)))
        .expect(1)
        .mount(mock_server)
        .await;
}

fn run_cake(env: &TestEnv, args: &[&str]) -> Output {
    env.command()
        .args(args)
        .env(TEST_KEY, "test-token")
        // The default command-policy gate is not installed in this test
        // environment; this suite exercises persistence, not gate policy.
        .env("CAKE_GATE", "off")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "cake should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn session_file(env: &TestEnv) -> PathBuf {
    let sessions_dir = env.data_dir.join("sessions");
    let mut files: Vec<_> = fs::read_dir(&sessions_dir)
        .expect("sessions directory should exist")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    assert_eq!(files.len(), 1, "expected exactly one session file");
    files.pop().expect("one session file")
}

fn session_id_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .expect("session file should have a UUID name")
        .to_string()
}

fn blob_dir(env: &TestEnv, session_id: &str) -> PathBuf {
    env.data_dir
        .join("sessions")
        .join(format!("{session_id}.blobs"))
}

fn blob_files(env: &TestEnv, session_id: &str) -> Vec<PathBuf> {
    let mut files: Vec<_> = fs::read_dir(blob_dir(env, session_id))
        .expect("session blob directory should exist")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    files.sort();
    files
}

fn session_records(path: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(path)
        .expect("session file should be readable")
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).expect("session line should parse"))
        .collect()
}

fn stored_tool_output(records: &[serde_json::Value]) -> &serde_json::Value {
    records
        .iter()
        .find(|record| {
            record["type"] == "function_call_output" && record["call_id"] == "call-output"
        })
        .expect("session should contain the Bash output record")
}

/// The text of every `function_call_output` item in a Responses request.
fn request_tool_outputs(body: &[u8]) -> Vec<String> {
    let request: serde_json::Value =
        serde_json::from_slice(body).expect("provider request should be JSON");
    request["input"]
        .as_array()
        .expect("provider request should have an input array")
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .filter_map(|item| item["output"].as_str().map(ToOwned::to_owned))
        .collect()
}

fn parse_stream(stdout: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("stdout lines must be JSON"))
        .collect()
}

fn replay_tool_output(env: &TestEnv, session_id: &str) -> serde_json::Value {
    let replay = run_cake(
        env,
        &["--output-format", "stream-json", "replay", session_id],
    );
    assert_success(&replay);
    parse_stream(&replay.stdout)
        .into_iter()
        .find(|record| {
            record["type"] == "function_call_output" && record["call_id"] == "call-output"
        })
        .expect("replay should emit the tool output")
}

fn assert_content_id(content_id: &str) {
    assert_eq!(content_id.len(), 64, "sha-256 is 64 hex characters");
    assert!(
        content_id
            .chars()
            .all(|character| matches!(character, '0'..='9' | 'a'..='f')),
        "content id should be lowercase hex: {content_id}"
    );
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end test proves the write, resume, replay, and both fail-closed paths"
)]
#[tokio::test]
async fn bash_output_above_threshold_is_stored_once_and_resumes_and_replays_inline() {
    let env = TestEnv::new("cake-session-tool-outputs-above");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    mount_tool_turn(&mock_server, &output_command(ABOVE_THRESHOLD_XS)).await;

    let initial = run_cake(&env, &[INITIAL_PROMPT]);
    assert_success(&initial);

    let session_path = session_file(&env);
    let session_id = session_id_from_path(&session_path);
    let records = session_records(&session_path);
    let stored = stored_tool_output(&records);
    assert_eq!(
        stored["output"], "",
        "an externalized output keeps the field with empty text"
    );
    let content_id = stored["content_id"]
        .as_str()
        .expect("externalized output should have a content id");
    assert_content_id(content_id);

    let blobs = blob_files(&env, &session_id);
    assert_eq!(blobs.len(), 1, "one output should store one blob");
    let blob_path = &blobs[0];
    assert_eq!(
        blob_path.file_name().and_then(|name| name.to_str()),
        Some(content_id),
        "the blob is named by its digest"
    );
    let blob_text = fs::read_to_string(blob_path).expect("the blob should contain UTF-8 output");
    assert!(
        blob_text.starts_with(&"x".repeat(ABOVE_THRESHOLD_XS)),
        "the blob should contain the command output"
    );
    assert!(
        blob_text.contains("[exit:0"),
        "the blob should contain the complete tool result: {blob_text}"
    );
    assert_eq!(
        digest(blob_text.as_bytes()),
        content_id,
        "the content id is the output digest"
    );

    let raw_session = fs::read_to_string(&session_path).expect("session file should be readable");
    assert!(
        !raw_session.contains(&"x".repeat(ABOVE_THRESHOLD_XS)),
        "the session JSONL must not retain the externalized output text"
    );

    mock_server.reset().await;
    mount_final_turn(&mock_server, "The command finished.").await;
    let resumed = run_cake(&env, &["--resume", &session_id, "what did it print?"]);
    assert_success(&resumed);
    let requests = mock_server
        .received_requests()
        .await
        .expect("recorded resume requests");
    assert_eq!(requests.len(), 1, "one resumed provider call expected");
    assert_eq!(
        request_tool_outputs(&requests[0].body),
        vec![blob_text.clone()],
        "resume should hydrate the exact output for the provider"
    );
    assert_eq!(
        blob_files(&env, &session_id).len(),
        1,
        "resume must not duplicate the blob"
    );

    let replay_before = fs::read(&session_path).expect("session file should be readable");
    let replayed = replay_tool_output(&env, &session_id);
    assert_eq!(
        replayed["output"], blob_text,
        "replay should emit the hydrated output"
    );
    assert!(
        replayed.get("content_id").is_none(),
        "replay keeps the pre-blob stream shape: {replayed}"
    );
    assert_eq!(
        fs::read(&session_path).expect("session file should be readable"),
        replay_before,
        "replay must not modify the session file"
    );

    fs::remove_file(blob_path).expect("failed to remove the output blob");
    mock_server.reset().await;
    let missing = run_cake(&env, &["--resume", &session_id, "try again"]);
    assert!(
        !missing.status.success(),
        "resume should fail when the output blob is missing"
    );
    let missing_stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(missing_stderr.contains(&session_id), "{missing_stderr}");
    assert!(missing_stderr.contains(content_id), "{missing_stderr}");
    assert!(
        missing_stderr.contains("missing"),
        "the error should name the missing blob: {missing_stderr}"
    );
    assert!(
        mock_server
            .received_requests()
            .await
            .expect("recorded failed resume requests")
            .is_empty(),
        "a missing output blob must fail before the provider request"
    );

    fs::write(blob_path, b"corrupt output bytes").expect("failed to corrupt output blob");
    let corrupt = run_cake(&env, &["--resume", &session_id, "try once more"]);
    assert!(
        !corrupt.status.success(),
        "resume should fail when the output blob is corrupt"
    );
    let corrupt_stderr = String::from_utf8_lossy(&corrupt.stderr);
    assert!(corrupt_stderr.contains(&session_id), "{corrupt_stderr}");
    assert!(corrupt_stderr.contains(content_id), "{corrupt_stderr}");
    assert!(
        corrupt_stderr.contains("digest mismatch"),
        "the error should name the digest mismatch: {corrupt_stderr}"
    );
    assert!(
        mock_server
            .received_requests()
            .await
            .expect("recorded corrupt resume requests")
            .is_empty(),
        "a corrupt output blob must fail before the provider request"
    );
}

#[tokio::test]
async fn bash_output_below_threshold_stays_inline_through_resume_and_replay() {
    let env = TestEnv::new("cake-session-tool-outputs-below");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    mount_tool_turn(&mock_server, &output_command(BELOW_THRESHOLD_XS)).await;

    let initial = run_cake(&env, &[INITIAL_PROMPT]);
    assert_success(&initial);

    let session_path = session_file(&env);
    let session_id = session_id_from_path(&session_path);
    let records = session_records(&session_path);
    let stored = stored_tool_output(&records);
    let inline = stored["output"]
        .as_str()
        .expect("an inline output should keep its text");
    assert!(
        inline.starts_with(&"x".repeat(BELOW_THRESHOLD_XS)),
        "the session should store the command output inline"
    );
    assert!(
        stored.get("content_id").is_none(),
        "a below-threshold output must not have a content id: {stored}"
    );
    assert!(
        !blob_dir(&env, &session_id).exists(),
        "a below-threshold output must not create a blob directory"
    );

    mock_server.reset().await;
    mount_final_turn(&mock_server, "The command finished.").await;
    let resumed = run_cake(&env, &["--resume", &session_id, "what did it print?"]);
    assert_success(&resumed);
    let requests = mock_server
        .received_requests()
        .await
        .expect("recorded resume requests");
    assert_eq!(requests.len(), 1, "one resumed provider call expected");
    assert_eq!(
        request_tool_outputs(&requests[0].body),
        vec![inline.to_string()],
        "resume should send the inline output unchanged"
    );

    let replayed = replay_tool_output(&env, &session_id);
    assert_eq!(
        replayed["output"], inline,
        "replay should emit the inline output"
    );
    assert!(
        replayed.get("content_id").is_none(),
        "replay keeps the pre-blob stream shape: {replayed}"
    );
}

#[tokio::test]
async fn legacy_inline_output_loads_resumes_and_replays_unchanged() {
    let env = TestEnv::new("cake-session-tool-outputs-legacy");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());

    let legacy_output = "l".repeat(ABOVE_THRESHOLD_XS);
    let working_directory = fs::canonicalize(&env.workspace_dir).expect("workspace canonical");
    let records = [
        serde_json::json!({
            "type": "session_meta",
            "format_version": 4,
            "session_id": LEGACY_SESSION_ID,
            "timestamp": "2026-07-27T12:00:00Z",
            "working_directory": working_directory,
            "model": "glm-5.1",
            "model_config": "test",
            "tools": ["Bash"],
        }),
        serde_json::json!({
            "type": "task_start",
            "session_id": LEGACY_SESSION_ID,
            "task_id": "550e8400-e29b-41d4-a716-446655440001",
            "timestamp": "2026-07-27T12:00:01Z",
        }),
        serde_json::json!({
            "type": "message",
            "role": "user",
            "content": "run the legacy command",
            "timestamp": "2026-07-27T12:00:02Z",
        }),
        serde_json::json!({
            "type": "function_call",
            "id": "fc-legacy",
            "call_id": "call-output",
            "name": "Bash",
            "arguments": "{\"command\":\"printf legacy\"}",
            "timestamp": "2026-07-27T12:00:03Z",
        }),
        serde_json::json!({
            "type": "function_call_output",
            "call_id": "call-output",
            "output": legacy_output,
            "timestamp": "2026-07-27T12:00:04Z",
        }),
        serde_json::json!({
            "type": "message",
            "role": "user",
            "content": "carry on",
            "timestamp": "2026-07-27T12:00:05Z",
        }),
    ];
    let sessions_dir = env.data_dir.join("sessions");
    fs::create_dir_all(&sessions_dir).expect("failed to create sessions directory");
    let session_path = sessions_dir.join(format!("{LEGACY_SESSION_ID}.jsonl"));
    let fixture = records
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(&session_path, &fixture).expect("failed to write legacy session fixture");

    mount_final_turn(&mock_server, "The legacy run finished.").await;
    let resumed = run_cake(&env, &["--resume", LEGACY_SESSION_ID, "what did it print?"]);
    assert_success(&resumed);
    let requests = mock_server
        .received_requests()
        .await
        .expect("recorded legacy resume requests");
    assert_eq!(requests.len(), 1, "one legacy provider call expected");
    assert_eq!(
        request_tool_outputs(&requests[0].body),
        vec![legacy_output.clone()],
        "the legacy inline output should reach the provider unchanged"
    );
    assert!(
        !blob_dir(&env, LEGACY_SESSION_ID).exists(),
        "loading a legacy session must not create a blob directory"
    );

    let replay_before = fs::read(&session_path).expect("session file should be readable");
    let replayed = replay_tool_output(&env, LEGACY_SESSION_ID);
    assert_eq!(
        replayed["output"], legacy_output,
        "replay should emit the legacy inline output"
    );
    assert!(
        replayed.get("content_id").is_none(),
        "replay keeps the legacy shape: {replayed}"
    );
    assert_eq!(
        fs::read(&session_path).expect("session file should be readable"),
        replay_before,
        "replay must not migrate or rewrite a legacy session"
    );
}
