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
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

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

fn success_response_without_usage() -> serde_json::Value {
    let mut response = success_response();
    response
        .as_object_mut()
        .expect("success response should be an object")
        .remove("usage");
    response
}

fn summary_response(summary: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "resp-summary",
        "output": [
            {
                "type": "message",
                "id": "msg-summary",
                "status": "completed",
                "content": [{ "type": "output_text", "text": summary }]
            }
        ],
        "usage": { "input_tokens": 100, "output_tokens": 20, "total_tokens": 120 }
    })
}

fn write_responses_settings(env: &TestEnv, base_url: &str) {
    write_responses_settings_with_image_support(env, base_url, false);
}

const COMPACTION_TEST_CONTEXT_WINDOW: u32 = 200_000;
const COMPACTION_TEST_TRIGGER_TOKENS: u64 = 20_000;
const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

fn write_compaction_settings(env: &TestEnv, base_url: &str, enabled: bool, trigger_tokens: u64) {
    env.write_global_settings(&format!(
        r#"
[[models]]
name = "test"
model = "glm-5.1"
base_url = "{base_url}"
api_key_env = "{TEST_KEY}"
api_type = "responses"
context_window = {COMPACTION_TEST_CONTEXT_WINDOW}

[compaction]
enabled = {enabled}
trigger_tokens = {trigger_tokens}
keep_recent_tokens = 12000
summary_tokens = 2048
"#
    ));
    env.write_project_settings(
        r#"
default_model = "test"
"#,
    );
}

fn write_codex_compaction_settings(env: &TestEnv, enabled: bool) {
    env.write_global_settings(&format!(
        r#"
[[models]]
name = "test"
model = "gpt-5.1-codex"
base_url = "{CODEX_BASE_URL}"
api_key_env = "UNUSED_CODEX_KEY"
api_type = "responses"
context_window = {COMPACTION_TEST_CONTEXT_WINDOW}

[compaction]
enabled = {enabled}
trigger_tokens = {COMPACTION_TEST_TRIGGER_TOKENS}
"#
    ));
    env.write_project_settings(
        r#"
default_model = "test"
"#,
    );
}

/// Seed the file-backed Codex credential and return its `CODEX_HOME`.
fn write_codex_auth(env: &TestEnv) -> PathBuf {
    let codex_home = env.home_dir.join(".codex");
    fs::create_dir_all(&codex_home).expect("failed to create Codex home");
    fs::write(
        codex_home.join("auth.json"),
        r#"{"tokens":{"access_token":"test-token","account_id":"test-account"}}"#,
    )
    .expect("failed to write Codex auth fixture");
    codex_home
}

fn write_image_responses_settings(env: &TestEnv, base_url: &str) {
    write_responses_settings_with_image_support(env, base_url, true);
}

fn write_responses_settings_with_image_support(
    env: &TestEnv,
    base_url: &str,
    supports_images: bool,
) {
    let image_support = if supports_images {
        "supports_images = true"
    } else {
        ""
    };
    env.write_global_settings(&format!(
        r#"
[[models]]
name = "test"
model = "glm-5.1"
base_url = "{base_url}"
api_key_env = "{TEST_KEY}"
api_type = "responses"
{image_support}
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

fn forked_session_file(env: &TestEnv) -> PathBuf {
    fs::read_dir(env.data_dir.join("sessions"))
        .expect("sessions directory should exist")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.extension().is_some_and(|ext| ext == "jsonl")
                && path.file_stem().and_then(|stem| stem.to_str()) != Some(SESSION_ID)
        })
        .expect("fork should create a new session file")
}

fn session_records(path: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(path)
        .expect("session file should be readable")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("session line should be JSON"))
        .collect()
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
    run_mode_with_response(env, mock_server, args, success_response()).await
}

async fn run_mode_with_response(
    env: &TestEnv,
    mock_server: &MockServer,
    args: &[&str],
    response: serde_json::Value,
) -> std::process::Output {
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
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

const LARGE_POST_CHECKPOINT_SENTINEL: &str = "post-checkpoint-large-sentinel";

fn large_post_checkpoint_text() -> String {
    format!(
        "{LARGE_POST_CHECKPOINT_SENTINEL} {}",
        "post checkpoint detail ".repeat(4_000)
    )
}

/// The checkpointed session with a large unpinned assistant exchange after the
/// newest checkpoint, so an enabled policy has retired content to summarize.
fn checkpointed_records_with_large_tail(env: &TestEnv) -> Vec<serde_json::Value> {
    let mut records = checkpointed_records(env);
    records.push(serde_json::json!({
        "type": "message",
        "role": "assistant",
        "content": large_post_checkpoint_text(),
        "timestamp": "2026-07-27T12:00:06Z",
    }));
    records.push(serde_json::json!({
        "type": "function_call",
        "id": "fc-post-large",
        "call_id": "call-post-large",
        "name": "Read",
        "arguments": "{\"file_path\":\"AGENTS.md\"}",
        "timestamp": "2026-07-27T12:00:07Z",
    }));
    records.push(serde_json::json!({
        "type": "function_call_output",
        "call_id": "call-post-large",
        "output": "post-large output",
        "timestamp": "2026-07-27T12:00:08Z",
    }));
    records
}

/// The checkpointed session with a complete usage record before the newest
/// checkpoint. Restore must not seed the agent's context basis from it.
fn checkpointed_records_with_pre_checkpoint_usage(
    env: &TestEnv,
    input_tokens: u64,
) -> Vec<serde_json::Value> {
    let mut records = checkpointed_records(env);
    let checkpoint = records
        .iter()
        .rposition(|record| record["type"] == "compaction")
        .expect("fixture should contain a checkpoint");
    let output_tokens = 1_000;
    records.insert(
        checkpoint,
        serde_json::json!({
            "type": "turn_usage",
            "session_id": SESSION_ID,
            "task_id": "550e8400-e29b-41d4-a716-446655440001",
            "turn": 1,
            "usage": {
                "input_tokens": input_tokens,
                "input_tokens_details": { "cached_tokens": 0 },
                "output_tokens": output_tokens,
                "output_tokens_details": { "reasoning_tokens": 0 },
                "total_tokens": input_tokens + output_tokens,
            },
            "timestamp": "2026-07-27T12:00:02Z",
            "usage_presence": "complete",
            "context_window": COMPACTION_TEST_CONTEXT_WINDOW,
            "remaining_context_tokens": u64::from(COMPACTION_TEST_CONTEXT_WINDOW) - input_tokens,
        }),
    );
    records
}

fn codex_checkpointed_records(env: &TestEnv) -> Vec<serde_json::Value> {
    let mut records = checkpointed_records(env);
    records[0]["model"] = serde_json::json!("gpt-5.1-codex");
    records
}

/// Two distinct valid 1x1 PNGs, retained in a checkpoint's tool output.
const CHECKPOINT_IMAGE_ONE_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR42mP4z8AAAAMBAQD3A0FDAAAAAElFTkSuQmCC";
const CHECKPOINT_IMAGE_TWO_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9ZQmcAAAAASUVORK5CYII=";

/// The same two-checkpoint session, with both images retained in the latest
/// checkpoint's `Read` output.
fn checkpointed_records_with_images(env: &TestEnv) -> Vec<serde_json::Value> {
    let mut records = checkpointed_records(env);
    let checkpoint = records
        .iter_mut()
        .rev()
        .find(|record| record["type"] == "compaction")
        .expect("fixture should contain a checkpoint");
    let output = checkpoint["retained"]
        .as_array_mut()
        .expect("retained should be an array")
        .iter_mut()
        .find(|record| record["type"] == "function_call_output")
        .expect("checkpoint should retain a tool output");
    output["images"] = serde_json::json!([
        { "media_type": "image/png", "data_base64": CHECKPOINT_IMAGE_ONE_BASE64 },
        { "media_type": "image/png", "data_base64": CHECKPOINT_IMAGE_TWO_BASE64 },
    ]);
    records
}

fn expected_checkpoint_image_urls() -> Vec<String> {
    vec![
        format!("data:image/png;base64,{CHECKPOINT_IMAGE_ONE_BASE64}"),
        format!("data:image/png;base64,{CHECKPOINT_IMAGE_TWO_BASE64}"),
    ]
}

/// Every provider `input_image` URL in wire order.
fn request_image_urls(value: &serde_json::Value) -> Vec<String> {
    fn collect(value: &serde_json::Value, urls: &mut Vec<String>) {
        match value {
            serde_json::Value::Array(items) => {
                for item in items {
                    collect(item, urls);
                }
            },
            serde_json::Value::Object(map) => {
                if map.get("type").and_then(serde_json::Value::as_str) == Some("input_image") {
                    urls.push(
                        map.get("image_url")
                            .and_then(serde_json::Value::as_str)
                            .expect("input_image should carry an image_url")
                            .to_string(),
                    );
                }
                for child in map.values() {
                    collect(child, urls);
                }
            },
            _ => {},
        }
    }

    let mut urls = Vec::new();
    collect(value, &mut urls);
    urls
}

/// Whether a request is the production summary adapter's bounded call.
fn is_summary_request(body: &serde_json::Value) -> bool {
    let instruction_present = body
        .get("instructions")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|text| text.contains("producing a checkpoint summary"))
        || body
            .get("messages")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|messages| {
                messages.iter().any(|message| {
                    message
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|text| text.contains("producing a checkpoint summary"))
                })
            });
    let tools_empty = body
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .is_none_or(Vec::is_empty);
    instruction_present && tools_empty
}

async fn mount_compaction_provider(server: &MockServer, summary: &str) {
    let summary = summary.to_string();
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(move |request: &Request| {
            let body = serde_json::from_slice::<serde_json::Value>(&request.body)
                .expect("provider request body must be JSON");
            let response = if is_summary_request(&body) {
                summary_response(&summary)
            } else {
                success_response()
            };
            ResponseTemplate::new(200).set_body_json(response)
        })
        .mount(server)
        .await;
}

fn assert_checkpoint_image_blocks(body: &serde_json::Value) {
    let expected = expected_checkpoint_image_urls();
    assert_eq!(
        request_image_urls(body),
        expected,
        "the restored request should carry both seeded images once each, in order"
    );

    let output = body["input"]
        .as_array()
        .expect("request input should be an array")
        .iter()
        .find(|item| item["type"] == "function_call_output" && item["call_id"] == "call-latest")
        .and_then(|item| item["output"].as_array())
        .expect("the retained tool output should be present");
    assert_eq!(output.len(), 3, "one text block plus both image blocks");
    assert_eq!(output[0]["type"], "input_text");
    assert_eq!(output[0]["text"], "contents of call-latest");
    let output_urls: Vec<String> = output[1..]
        .iter()
        .map(|block| {
            assert_eq!(block["type"], "input_image");
            block["image_url"]
                .as_str()
                .expect("input_image should carry an image_url")
                .to_string()
        })
        .collect();
    assert_eq!(
        output_urls, expected,
        "the retained output should keep the seeded image order and bytes"
    );
}

async fn assert_mode_restores_checkpoint_images(mode: &str) {
    let env = TestEnv::new(&format!("cake-{mode}-checkpoint-images-test"));
    let mock_server = MockServer::start().await;
    write_image_responses_settings(&env, &mock_server.uri());
    write_session_fixture(&env, &checkpointed_records_with_images(&env));

    let mode_flag = format!("--{mode}");
    let output = run_mode(&env, &mock_server, &[&mode_flag, SESSION_ID, "carry on"]).await;
    assert!(
        output.status.success(),
        "{mode} should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let requests = mock_server
        .received_requests()
        .await
        .expect("recorded requests");
    assert_eq!(requests.len(), 1, "{mode}: one provider call expected");
    let body: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("request body should be JSON");
    assert_checkpoint_image_blocks(&body);

    if mode == "fork" {
        assert_eq!(
            session_file_count(&env),
            2,
            "fork should persist a new session beside the seeded one"
        );
    }
}

#[tokio::test]
async fn resume_restores_checkpoint_images_byte_identical_once_in_order() {
    assert_mode_restores_checkpoint_images("resume").await;
}

#[tokio::test]
async fn fork_restores_checkpoint_images_byte_identical_once_in_order() {
    assert_mode_restores_checkpoint_images("fork").await;
}

fn assert_compaction_requests(mode: &str, requests: &[Request], stderr: &str) {
    assert_eq!(
        requests.len(),
        2,
        "{mode}: one summary and one main provider call expected; stderr: {stderr}"
    );
    let bodies: Vec<serde_json::Value> = requests
        .iter()
        .map(|request| {
            serde_json::from_slice(&request.body).expect("provider request body should be JSON")
        })
        .collect();
    assert!(
        is_summary_request(&bodies[0]),
        "{mode}: the summary must run before the compacted request"
    );
    assert!(
        !is_summary_request(&bodies[1]),
        "{mode}: the compacted request must be the only main call"
    );
    let main_body = String::from_utf8_lossy(&requests[1].body);
    assert!(
        main_body.contains("fresh check summary"),
        "{mode}: the committed summary must reach the next request: {main_body}"
    );
    assert!(
        !main_body.contains(LARGE_POST_CHECKPOINT_SENTINEL),
        "{mode}: the retired large exchange must not reach the compacted request: {main_body}"
    );
    assert!(
        !main_body.contains("older summary"),
        "{mode}: a superseded checkpoint must stay retired: {main_body}"
    );
    assert!(
        main_body.contains("carry on"),
        "{mode}: the current input must survive the checkpoint: {main_body}"
    );
}

fn assert_compacted_session(mode: &str, env: &TestEnv) {
    let path = if mode == "fork" {
        forked_session_file(env)
    } else {
        session_file(env)
    };
    let records = session_records(&path);
    let checkpoints: Vec<_> = records
        .iter()
        .enumerate()
        .filter(|(_, record)| record["type"] == "compaction")
        .collect();
    let expected_checkpoints = if mode == "fork" { 2 } else { 3 };
    assert_eq!(
        checkpoints.len(),
        expected_checkpoints,
        "{mode}: one checkpoint should be added to the restored history"
    );
    let (checkpoint_index, checkpoint) = *checkpoints
        .last()
        .expect("compaction should append a checkpoint");
    assert_eq!(checkpoint["summary"], "fresh check summary");
    assert_ne!(checkpoint["checkpoint_id"], "checkpoint-newer summary");

    let start = records
        .iter()
        .rposition(|record| record["type"] == "compaction_start")
        .expect("compaction should persist a start record");
    let end = records
        .iter()
        .rposition(|record| record["type"] == "compaction_end")
        .expect("compaction should persist an end record");
    assert!(
        start < checkpoint_index && checkpoint_index < end,
        "{mode}: lifecycle order must be start, checkpoint, end"
    );
}

async fn assert_enabled_policy_compacts_restored_history(mode: &str) {
    let env = TestEnv::new(&format!("cake-{mode}-enabled-compaction-test"));
    let mock_server = MockServer::start().await;
    write_compaction_settings(
        &env,
        &mock_server.uri(),
        true,
        COMPACTION_TEST_TRIGGER_TOKENS,
    );
    write_session_fixture(&env, &checkpointed_records_with_large_tail(&env));
    mount_compaction_provider(&mock_server, "fresh check summary").await;

    let mode_flag = format!("--{mode}");
    let output = env
        .command()
        .args([&mode_flag, SESSION_ID, "carry on"])
        .env(TEST_KEY, "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");
    assert!(
        output.status.success(),
        "{mode} with enabled compaction should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let requests = mock_server
        .received_requests()
        .await
        .expect("recorded requests");
    assert_compaction_requests(mode, &requests, &String::from_utf8_lossy(&output.stderr));
    assert_compacted_session(mode, &env);
}

#[tokio::test]
async fn resume_with_enabled_policy_compacts_a_restored_exchange() {
    assert_enabled_policy_compacts_restored_history("resume").await;
}

#[tokio::test]
async fn fork_with_enabled_policy_compacts_a_restored_exchange() {
    assert_enabled_policy_compacts_restored_history("fork").await;
}

async fn assert_disabled_policy_ignores_pre_checkpoint_usage(mode: &str) {
    let env = TestEnv::new(&format!("cake-{mode}-disabled-compaction-test"));
    let mock_server = MockServer::start().await;
    write_compaction_settings(
        &env,
        &mock_server.uri(),
        false,
        COMPACTION_TEST_TRIGGER_TOKENS,
    );
    write_session_fixture(
        &env,
        &checkpointed_records_with_pre_checkpoint_usage(&env, 190_000),
    );

    let mode_flag = format!("--{mode}");
    let output = run_mode_with_response(
        &env,
        &mock_server,
        &[&mode_flag, SESSION_ID, "carry on"],
        success_response_without_usage(),
    )
    .await;
    assert!(
        output.status.success(),
        "{mode} with disabled compaction should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let requests = mock_server
        .received_requests()
        .await
        .expect("recorded requests");
    assert_eq!(requests.len(), 1, "{mode}: one main provider call expected");
    let body = String::from_utf8_lossy(&requests[0].body);
    assert!(
        body.contains("newer summary") && body.contains("retained latest"),
        "{mode}: the existing checkpoint must still restore: {body}"
    );
    assert!(
        !body.contains("older summary") && !body.contains("retired request"),
        "{mode}: disabled policy must not resurrect retired history: {body}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Context window budget remaining"),
        "{mode}: pre-checkpoint usage must not seed the context budget: {stderr}"
    );
    assert!(
        !stderr.contains("context_tokens=190000"),
        "{mode}: the retired usage record must not leak into the budget log: {stderr}"
    );

    let path = if mode == "fork" {
        forked_session_file(&env)
    } else {
        session_file(&env)
    };
    let records = session_records(&path);
    let expected_checkpoints = if mode == "fork" { 1 } else { 2 };
    assert_eq!(
        records
            .iter()
            .filter(|record| record["type"] == "compaction")
            .count(),
        expected_checkpoints,
        "{mode}: disabled policy must not create a checkpoint"
    );
    assert!(
        !records.iter().any(|record| {
            record["type"] == "compaction_start" || record["type"] == "compaction_end"
        }),
        "{mode}: disabled policy must emit no compaction lifecycle"
    );
}

#[tokio::test]
async fn resume_with_disabled_policy_ignores_pre_checkpoint_usage() {
    assert_disabled_policy_ignores_pre_checkpoint_usage("resume").await;
}

#[tokio::test]
async fn fork_with_disabled_policy_ignores_pre_checkpoint_usage() {
    assert_disabled_policy_ignores_pre_checkpoint_usage("fork").await;
}

fn set_proxy_env(command: &mut std::process::Command, proxy: &str) {
    for name in [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        command.env(name, proxy);
    }
    command.env("NO_PROXY", "").env("no_proxy", "");
}

async fn assert_codex_enabled_policy_makes_no_network_call(mode: &str) {
    let env = TestEnv::new(&format!("cake-{mode}-codex-enabled-test"));
    let proxy = MockServer::start().await;
    let codex_home = write_codex_auth(&env);
    write_codex_compaction_settings(&env, true);
    write_session_fixture(&env, &codex_checkpointed_records(&env));

    let mode_flag = format!("--{mode}");
    let mut command = env.command();
    command
        .args([&mode_flag, SESSION_ID, "carry on"])
        .env("CODEX_HOME", &codex_home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    set_proxy_env(&mut command, &proxy.uri());
    let output = command.output().expect("failed to execute cake");

    assert!(!output.status.success(), "{mode}: Codex policy must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("does not support a bounded summary generation cap"),
        "{mode}: the error must name the Codex summary restriction: {stderr}"
    );
    assert!(
        proxy
            .received_requests()
            .await
            .expect("recorded proxy requests")
            .is_empty(),
        "{mode}: the Codex policy check must make zero network calls"
    );
}

#[tokio::test]
async fn resume_with_enabled_policy_fails_on_codex_before_network() {
    assert_codex_enabled_policy_makes_no_network_call("resume").await;
}

#[tokio::test]
async fn fork_with_enabled_policy_fails_on_codex_before_network() {
    assert_codex_enabled_policy_makes_no_network_call("fork").await;
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
