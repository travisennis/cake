//! End-to-end coverage for the session modes that restore or fork a prior
//! session (`--resume <UUID>`, `--fork <UUID>`): the run resolves the
//! restored session's model and reaches the provider, exercising the `Resume`
//! and `Fork` arms of `build_client_and_session`. The command-safety judge
//! context those arms attach is unit-tested in `session_factory` (these e2e
//! tests never invoke Bash, so they cannot observe the wiring themselves).

#![expect(clippy::expect_used, reason = "test code uses expect for assertions")]

mod support;

use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
};

use sha2::{Digest, Sha256};
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

/// A session whose `session_meta` pins the given provider endpoint.
fn records_with_identity(env: &TestEnv, base_url: &str) -> Vec<serde_json::Value> {
    let mut records = minimal_records(env);
    let base_url = reqwest::Url::parse(base_url)
        .expect("test endpoint should parse")
        .to_string();
    records[0]["provider_identity"] = serde_json::json!({
        "model_config": "test",
        "model": "glm-5.1",
        "base_url": base_url,
        "api_type": "responses",
        "credential": { "kind": "env", "name": TEST_KEY },
    });
    records
}

/// A session whose `session_meta` pins the given endpoint and query digest.
fn records_with_identity_and_query(
    env: &TestEnv,
    base_url: &str,
    query: &str,
) -> Vec<serde_json::Value> {
    let mut records = minimal_records(env);
    let base_url = reqwest::Url::parse(base_url)
        .expect("test endpoint should parse")
        .to_string();
    let mut hasher = Sha256::new();
    hasher.update(b"cake-provider-identity-query\0");
    hasher.update(query.as_bytes());
    records[0]["provider_identity"] = serde_json::json!({
        "model_config": "test",
        "model": "glm-5.1",
        "base_url": base_url,
        "base_url_query_digest": hex::encode(hasher.finalize()),
        "api_type": "responses",
        "credential": { "kind": "env", "name": TEST_KEY },
    });
    records
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

fn session_file(env: &TestEnv) -> PathBuf {
    env.data_dir
        .join("sessions")
        .join(format!("{SESSION_ID}.jsonl"))
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;

    fs::metadata(path)
        .expect("path should have metadata")
        .permissions()
        .mode()
        & 0o777
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("failed to set test mode");
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

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no provider identity snapshot"),
        "a legacy session should warn about its unknown provider baseline, got: {stderr}"
    );
}

#[tokio::test]
async fn resume_with_matching_provider_identity_succeeds() {
    let env = TestEnv::new("cake-resume-matching-identity-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    write_session_fixture(&env, &records_with_identity(&env, &mock_server.uri()));

    let output = run_mode(&env, &mock_server, &["--resume", SESSION_ID, "carry on"]).await;
    assert!(
        output.status.success(),
        "matching identity should resume. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("provider identity changed"),
        "an unchanged identity should not warn, got: {stderr}"
    );
}

#[tokio::test]
async fn resume_with_changed_provider_identity_fails_before_a_provider_request() {
    let env = TestEnv::new("cake-resume-changed-identity-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    write_session_fixture(
        &env,
        &records_with_identity(&env, "https://old.example.invalid/v1"),
    );

    let output = env
        .command()
        .args(["--resume", SESSION_ID, "carry on"])
        .env(TEST_KEY, "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert_eq!(
        output.status.code(),
        Some(3),
        "a session-model mismatch is an input error. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Session model mismatch"), "{stderr}");
    assert!(stderr.contains("base_url:"), "{stderr}");
    assert!(
        stderr.contains("https://old.example.invalid/v1"),
        "{stderr}"
    );
    assert!(stderr.contains(&mock_server.uri()), "{stderr}");
    assert!(
        mock_server
            .received_requests()
            .await
            .expect("recorded requests")
            .is_empty(),
        "the mismatch must fail before any provider request"
    );
}

#[tokio::test]
async fn resume_with_changed_base_url_query_fails_before_a_provider_request() {
    let env = TestEnv::new("cake-resume-query-drift-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &format!("{}/?upstream=attacker", mock_server.uri()));
    write_session_fixture(
        &env,
        &records_with_identity_and_query(&env, &mock_server.uri(), "upstream=prod"),
    );

    let output = env
        .command()
        .args(["--resume", SESSION_ID, "carry on"])
        .env(TEST_KEY, "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert_eq!(
        output.status.code(),
        Some(3),
        "a query-only mismatch is an input error. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Session model mismatch"), "{stderr}");
    assert!(stderr.contains("base_url_query_digest"), "{stderr}");
    assert!(!stderr.contains("upstream=prod"), "{stderr}");
    assert!(!stderr.contains("upstream=attacker"), "{stderr}");
    assert!(
        mock_server
            .received_requests()
            .await
            .expect("recorded requests")
            .is_empty(),
        "the mismatch must fail before any provider request"
    );
}

#[tokio::test]
async fn allow_session_model_change_accepts_a_changed_identity_for_one_run() {
    let env = TestEnv::new("cake-resume-identity-override-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    write_session_fixture(
        &env,
        &records_with_identity(&env, "https://old.example.invalid/v1"),
    );

    let output = run_mode(
        &env,
        &mock_server,
        &[
            "--allow-session-model-change",
            "--resume",
            SESSION_ID,
            "carry on",
        ],
    )
    .await;
    assert!(
        output.status.success(),
        "the override should accept the changed identity. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--allow-session-model-change was given"),
        "the override should warn, got: {stderr}"
    );

    let telemetry_file = env
        .data_dir
        .join("session-telemetry")
        .join(format!("{SESSION_ID}.ndjson"));
    let contents = fs::read_to_string(telemetry_file).expect("telemetry sidecar should exist");
    let init: serde_json::Value = serde_json::from_str(
        contents
            .lines()
            .next()
            .expect("telemetry should start with an init record"),
    )
    .expect("init record should be valid JSON");
    assert_eq!(init["type"], "telemetry_init");
    assert_eq!(init["session_model_change_override"], true);
    assert_eq!(init["provider_identity"]["credential_kind"], "env");
    assert!(
        !contents.contains(TEST_KEY),
        "telemetry must not carry the credential variable name"
    );
    assert_eq!(
        init["provider_identity"]["base_url"],
        reqwest::Url::parse(&mock_server.uri())
            .expect("mock endpoint should parse")
            .to_string(),
        "telemetry records the identity the invocation actually used"
    );
}

#[tokio::test]
async fn resume_omits_history_images_for_a_text_only_model() {
    let env = TestEnv::new("cake-resume-images-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    let mut records = minimal_records(&env);
    records[2]["images"] = serde_json::json!([{
        "media_type": "image/png",
        "data_base64": "QUJD"
    }]);
    write_session_fixture(&env, &records);

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
    let body_text = String::from_utf8_lossy(&requests[0].body);
    assert!(
        body_text.contains("list the files"),
        "restored text should still reach the provider, got: {body_text}"
    );
    assert!(
        !body_text.contains("input_image"),
        "a text-only model must not receive restored images, got: {body_text}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.matches("cannot accept images").count(),
        1,
        "exactly one warning per run, got: {stderr}"
    );
    assert!(
        stderr.contains("omitting 1 image"),
        "the warning should name the omitted count, got: {stderr}"
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

#[tokio::test]
async fn fork_copies_the_parent_provider_identity_snapshot() {
    let env = TestEnv::new("cake-fork-identity-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    write_session_fixture(&env, &records_with_identity(&env, &mock_server.uri()));

    let output = run_mode(&env, &mock_server, &["--fork", SESSION_ID, "branch off"]).await;
    assert!(
        output.status.success(),
        "fork should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let new_session = fs::read_dir(env.data_dir.join("sessions"))
        .expect("sessions directory should exist")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_stem().is_some_and(|stem| stem != SESSION_ID)
                && path.extension().is_some_and(|ext| ext == "jsonl")
        })
        .expect("the fork should create a new session file");
    let first_line = fs::read_to_string(&new_session)
        .expect("forked session should be readable")
        .lines()
        .next()
        .expect("forked session should have a meta record")
        .to_string();
    let meta: serde_json::Value = serde_json::from_str(&first_line).expect("valid session meta");
    assert_eq!(meta["type"], "session_meta");
    assert_eq!(meta["provider_identity"]["model_config"], "test");
    assert_eq!(
        meta["provider_identity"]["base_url"],
        reqwest::Url::parse(&mock_server.uri())
            .expect("mock endpoint should parse")
            .to_string()
    );
    assert_eq!(meta["provider_identity"]["credential"]["name"], TEST_KEY);
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

// -------------------------------------------------------------------------
// Session storage permissions
// -------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn new_session_store_is_owner_only_under_a_permissive_umask() {
    use std::os::unix::process::CommandExt as _;

    let env = TestEnv::new("cake-session-permissions-umask");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    let mut command = env.command();
    command
        .arg("hello")
        .env(TEST_KEY, "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: the closure runs in the forked child before exec and calls only
    // the async-signal-safe `umask` syscall, matching the parent process state.
    unsafe {
        command.pre_exec(|| {
            libc::umask(0o000);
            Ok(())
        });
    }
    let output = command.output().expect("failed to execute cake");

    assert!(
        output.status.success(),
        "umask 000 run should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let sessions_dir = env.data_dir.join("sessions");
    let file = fs::read_dir(&sessions_dir)
        .expect("sessions directory should exist")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .expect("a session file should exist");

    assert_eq!(mode(&env.data_dir), 0o700, "data directory mode");
    assert_eq!(mode(&sessions_dir), 0o700, "sessions directory mode");
    assert_eq!(mode(&file), 0o600, "session file mode");
}

#[cfg(unix)]
#[tokio::test]
async fn resume_tightens_a_broad_session_file_before_append() {
    let env = TestEnv::new("cake-resume-permissions");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    write_session_fixture(&env, &minimal_records(&env));

    let sessions_dir = env.data_dir.join("sessions");
    let path = session_file(&env);
    set_mode(&sessions_dir, 0o755);
    set_mode(&path, 0o644);

    let output = run_mode(&env, &mock_server, &["--resume", SESSION_ID, "carry on"]).await;

    assert!(
        output.status.success(),
        "resume should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        mode(&sessions_dir),
        0o700,
        "store initialization should tighten the sessions directory"
    );
    assert_eq!(
        mode(&path),
        0o600,
        "append should tighten the existing session file"
    );
}

#[cfg(unix)]
#[test]
fn listing_and_replay_leave_a_broad_session_file_untouched() {
    let env = TestEnv::new("cake-session-read-only-modes");
    write_session_fixture(&env, &minimal_records(&env));

    let sessions_dir = env.data_dir.join("sessions");
    let path = session_file(&env);
    set_mode(&sessions_dir, 0o755);
    set_mode(&path, 0o644);

    let list = env
        .command()
        .args(["sessions", "list"])
        .output()
        .expect("failed to execute cake sessions list");
    assert!(
        list.status.success(),
        "sessions list should succeed. stderr: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    assert_eq!(mode(&sessions_dir), 0o700, "store initialization mode");
    assert_eq!(mode(&path), 0o644, "listing must not change the file mode");

    let replay = env
        .command()
        .args(["--output-format", "stream-json", "replay", SESSION_ID])
        .output()
        .expect("failed to execute cake replay");
    assert!(
        replay.status.success(),
        "replay should succeed. stderr: {}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert_eq!(mode(&path), 0o644, "replay must not change the file mode");
}

#[cfg(unix)]
#[test]
fn resume_rejects_a_symlinked_session_file_without_writing_through_it() {
    use std::os::unix::fs::symlink;

    let env = TestEnv::new("cake-session-file-symlink");
    write_responses_settings(&env, "http://127.0.0.1:1");
    write_session_fixture(&env, &minimal_records(&env));

    let link = session_file(&env);
    let target = env.data_dir.join("target.jsonl");
    fs::rename(&link, &target).expect("failed to move the session fixture");
    symlink(&target, &link).expect("failed to create the session symlink");
    let before = fs::read(&target).expect("target should be readable");

    let output = env
        .command()
        .args(["--resume", SESSION_ID, "carry on"])
        .env(TEST_KEY, "test-token")
        .output()
        .expect("failed to execute cake");

    assert!(!output.status.success(), "a symlinked session must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("symlink"),
        "the error must name the symlink: {stderr}"
    );
    assert!(
        stderr.contains(&format!("{SESSION_ID}.jsonl")),
        "the error must name the session path: {stderr}"
    );
    assert_eq!(
        fs::read(&target).expect("target should still be readable"),
        before,
        "a rejected symlink must not write through to its target"
    );
}

#[cfg(unix)]
#[test]
fn symlinked_sessions_directory_is_rejected_at_startup() {
    use std::os::unix::fs::symlink;

    let env = TestEnv::new("cake-session-dir-symlink");
    fs::create_dir_all(&env.data_dir).expect("failed to create the data directory");
    let target = env.data_dir.join("real-sessions");
    fs::create_dir_all(&target).expect("failed to create the target directory");
    symlink(&target, env.data_dir.join("sessions")).expect("failed to create the symlink");

    let output = env
        .command()
        .args(["sessions", "list"])
        .output()
        .expect("failed to execute cake");

    assert!(!output.status.success(), "a symlinked store must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("sessions directory") && stderr.contains("symlink"),
        "the error must name the symlinked sessions directory: {stderr}"
    );
}

#[test]
fn sessions_path_that_is_not_a_directory_is_rejected_at_startup() {
    let env = TestEnv::new("cake-session-dir-file");
    fs::create_dir_all(&env.data_dir).expect("failed to create the data directory");
    fs::write(env.data_dir.join("sessions"), "not a directory")
        .expect("failed to create the sessions file fixture");

    let output = env
        .command()
        .args(["sessions", "list"])
        .output()
        .expect("failed to execute cake");

    assert!(!output.status.success(), "a non-directory store must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("sessions directory") && stderr.contains("not a directory"),
        "the error must name the non-directory sessions path: {stderr}"
    );
}
