//! End-to-end coverage for stdio MCP servers.
//!
//! These tests drive the real binary against a mocked Responses API and the
//! Python fixture server under `src/clients/mcp/fixtures/`. They cover global
//! `mcp.json` discovery, tool registration in the prompt, dispatch through
//! `tools/call`, the read-only exclusion, project trust, and the explicit
//! `--mcp-config` input-error path.

#![expect(clippy::expect_used, reason = "test code uses expect for assertions")]
#![cfg(unix)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use support::TestEnv;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TEST_KEY: &str = "MCP_TEST_KEY";
const FIXTURE: &str = include_str!("../src/clients/mcp/fixtures/fake_mcp_server.py");

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
    env.write_project_settings("default_model = \"test\"\n");
}

fn write_fixture_server(env: &TestEnv) -> PathBuf {
    let script = env.workspace_dir.join("fake_mcp_server.py");
    fs::write(&script, FIXTURE).expect("failed to write MCP fixture server");
    script
}

fn write_global_mcp_config(env: &TestEnv, script: &Path, era: &str, exposure: &str) {
    write_mcp_config(env, script, era, exposure, false);
}

fn write_dynamic_mcp_config(env: &TestEnv, script: &Path, era: &str, exposure: &str) {
    write_mcp_config(env, script, era, exposure, true);
}

fn write_mcp_config(env: &TestEnv, script: &Path, era: &str, exposure: &str, dynamic: bool) {
    let config_dir = env.home_dir.join(".config").join("cake");
    fs::create_dir_all(&config_dir).expect("failed to create global config directory");
    let mut environment = serde_json::json!({ "FAKE_MCP_ERA": era });
    if dynamic {
        environment["FAKE_MCP_DYNAMIC"] = serde_json::json!("1");
    }
    let config = serde_json::json!({
        "mcpServers": {
            "fixture": {
                "command": "python3",
                "args": [script.display().to_string()],
                "env": environment,
                "exposure": exposure
            }
        }
    });
    fs::write(
        config_dir.join("mcp.json"),
        serde_json::to_string_pretty(&config).expect("fixture config should serialize"),
    )
    .expect("failed to write global mcp.json");
}

fn tool_call_response() -> serde_json::Value {
    serde_json::json!({
        "id": "resp-1",
        "output": [
            {
                "type": "function_call",
                "id": "fc-1",
                "call_id": "call-1",
                "name": "mcp__fixture__echo",
                "arguments": "{\"message\": \"hello\"}"
            }
        ],
        "usage": { "input_tokens": 10, "output_tokens": 5, "total_tokens": 15 }
    })
}

fn mutate_call_response() -> serde_json::Value {
    serde_json::json!({
        "id": "resp-mutate",
        "output": [
            {
                "type": "function_call",
                "id": "fc-mutate",
                "call_id": "call-mutate",
                "name": "mcp__fixture__mutate",
                "arguments": "{}"
            }
        ],
        "usage": { "input_tokens": 10, "output_tokens": 5, "total_tokens": 15 }
    })
}

fn tool_search_response() -> serde_json::Value {
    serde_json::json!({
        "id": "resp-search",
        "output": [
            {
                "type": "function_call",
                "id": "fc-search",
                "call_id": "call-search",
                "name": "ToolSearch",
                "arguments": "{\"query\": \"echo\"}"
            }
        ],
        "usage": { "input_tokens": 10, "output_tokens": 5, "total_tokens": 15 }
    })
}

fn final_response() -> serde_json::Value {
    serde_json::json!({
        "id": "resp-2",
        "output": [
            {
                "type": "message",
                "id": "msg-1",
                "status": "completed",
                "content": [{ "type": "output_text", "text": "MCP said hello." }]
            }
        ],
        "usage": { "input_tokens": 20, "output_tokens": 5, "total_tokens": 25 }
    })
}

fn session_file_path(env: &TestEnv) -> PathBuf {
    let sessions_dir = env.data_dir.join("sessions");
    let entries: Vec<_> = fs::read_dir(&sessions_dir)
        .expect("sessions directory should exist")
        .collect::<Result<Vec<_>, _>>()
        .expect("sessions directory should be readable");
    assert_eq!(entries.len(), 1, "expected exactly one session file");
    entries[0].path()
}

fn session_file_contents(env: &TestEnv) -> String {
    fs::read_to_string(session_file_path(env)).expect("session file should be readable")
}

fn session_id(env: &TestEnv) -> String {
    session_file_path(env)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("session file should have a UUID name")
        .to_string()
}

/// Mount the two-turn provider conversation used by the dispatch test.
async fn mount_two_turn_provider(mock_server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tool_call_response()))
        .up_to_n_times(1)
        .expect(1)
        .mount(mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_response()))
        .expect(1)
        .mount(mock_server)
        .await;
}

/// Mount two searches, then a final answer, for the active-result display test.
async fn mount_repeated_search_provider(mock_server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tool_search_response()))
        .up_to_n_times(2)
        .expect(2)
        .mount(mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_response()))
        .expect(1)
        .mount(mock_server)
        .await;
}

/// Mount search, MCP dispatch, then final answer for deferred MCP coverage.
async fn mount_deferred_provider(mock_server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tool_search_response()))
        .up_to_n_times(1)
        .expect(1)
        .mount(mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tool_call_response()))
        .up_to_n_times(1)
        .expect(1)
        .mount(mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_response()))
        .expect(1)
        .mount(mock_server)
        .await;
}

#[tokio::test]
async fn global_mcp_server_is_registered_and_called() {
    let env = TestEnv::new("cake-mcp-e2e-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    let script = write_fixture_server(&env);
    write_global_mcp_config(&env, &script, "modern", "direct");
    mount_two_turn_provider(&mock_server).await;

    let output = env
        .command()
        .arg("say hello through the fixture")
        .env(TEST_KEY, "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert!(
        output.status.success(),
        "cake should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let session = session_file_contents(&env);
    assert!(
        session.contains("mcp__fixture__echo"),
        "session should register the MCP tool: {session}"
    );
    assert!(
        session.contains("- **mcp__fixture__echo**: MCP server 'fixture': Echo a message."),
        "system prompt should list the MCP tool: {session}"
    );
    assert!(
        !session.contains("- **ToolSearch**:"),
        "a direct server should not advertise ToolSearch: {session}"
    );
    assert!(
        session.contains("echo: hello"),
        "session should record the MCP tool result: {session}"
    );

    let requests = mock_server
        .received_requests()
        .await
        .expect("recorded requests");
    assert_eq!(requests.len(), 2, "one tool turn and one final turn");
    let second = String::from_utf8_lossy(&requests[1].body);
    assert!(
        second.contains("mcp__fixture__echo") && second.contains("echo: hello"),
        "the tool result should reach the provider: {second}"
    );
}

#[tokio::test]
async fn deferred_mcp_tool_is_discovered_and_called() {
    let env = TestEnv::new("cake-mcp-deferred-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    let script = write_fixture_server(&env);
    write_global_mcp_config(&env, &script, "modern", "deferred");
    mount_deferred_provider(&mock_server).await;

    let output = env
        .command()
        .arg("say hello through the deferred fixture")
        .env(TEST_KEY, "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert!(
        output.status.success(),
        "cake should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let session = session_file_contents(&env);
    assert!(
        session.contains("## MCP servers"),
        "the prompt should summarize MCP servers: {session}"
    );
    assert!(
        session.contains("`deferred`, 4 tools"),
        "the prompt should mark the server deferred: {session}"
    );
    assert!(
        session.contains("- **ToolSearch**:"),
        "the prompt should advertise ToolSearch: {session}"
    );
    assert!(
        !session.contains("- **mcp__fixture__echo**:"),
        "a deferred tool must not be listed eagerly: {session}"
    );
    assert!(
        session.contains("echo: hello"),
        "the activated tool should run: {session}"
    );

    let requests = mock_server
        .received_requests()
        .await
        .expect("recorded requests");
    assert_eq!(requests.len(), 3, "search, tool, and final turns");
    let first = String::from_utf8_lossy(&requests[0].body);
    assert!(first.contains("\"ToolSearch\""), "{first}");
    assert!(
        !first.contains("\"mcp__fixture__echo\""),
        "deferred tool schema must not be sent before activation: {first}"
    );
    let second = String::from_utf8_lossy(&requests[1].body);
    assert!(
        second.contains("mcp__fixture__echo"),
        "the activated tool schema should be sent on the next turn: {second}"
    );

    let activation = session
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|record| record["type"] == "mcp_tools_activated")
        .expect("the session should record the MCP activation");
    assert_eq!(
        activation["tools"],
        serde_json::json!(["mcp__fixture__echo"]),
        "the activation record should carry only registered names"
    );
}

#[tokio::test]
async fn a_second_tool_search_reports_the_tool_as_already_active() {
    let env = TestEnv::new("cake-mcp-active-search-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    let script = write_fixture_server(&env);
    write_global_mcp_config(&env, &script, "modern", "deferred");
    mount_repeated_search_provider(&mock_server).await;

    let output = env
        .command()
        .arg("search twice for the fixture tool")
        .env(TEST_KEY, "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");
    assert!(
        output.status.success(),
        "cake should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let session = session_file_contents(&env);
    assert!(
        session.contains("Found 1 new MCP tool."),
        "the first search should load the tool: {session}"
    );
    assert!(
        session.contains("already active: mcp__fixture__echo."),
        "the second search should report the active tool: {session}"
    );
    assert_eq!(
        session.matches("\"type\":\"mcp_tools_activated\"").count(),
        1,
        "the second search must not record another activation: {session}"
    );
}

#[tokio::test]
async fn resumed_session_rehydrates_deferred_mcp_tool_activation() {
    let env = TestEnv::new("cake-mcp-resume-test");
    let first_server = MockServer::start().await;
    write_responses_settings(&env, &first_server.uri());
    let script = write_fixture_server(&env);
    write_global_mcp_config(&env, &script, "modern", "deferred");
    mount_deferred_provider(&first_server).await;

    let first = env
        .command()
        .arg("say hello through the deferred fixture")
        .env(TEST_KEY, "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");
    assert!(
        first.status.success(),
        "the first run should succeed. stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let session_id = session_id(&env);

    let resume_server = MockServer::start().await;
    write_responses_settings(&env, &resume_server.uri());
    mount_two_turn_provider(&resume_server).await;

    let resumed = env
        .command()
        .args([
            "--allow-session-model-change",
            "--resume",
            &session_id,
            "call the tool again",
        ])
        .env(TEST_KEY, "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake --resume");
    assert!(
        resumed.status.success(),
        "the resumed run should succeed. stderr: {}",
        String::from_utf8_lossy(&resumed.stderr)
    );

    let requests = resume_server
        .received_requests()
        .await
        .expect("recorded resume requests");
    assert_eq!(
        requests.len(),
        2,
        "the resumed run should call the tool without another search turn"
    );
    let first_request = String::from_utf8_lossy(&requests[0].body);
    assert!(
        first_request.contains("mcp__fixture__echo"),
        "the restored tool schema should be present on the first resumed request: {first_request}"
    );

    let second_resume_server = MockServer::start().await;
    write_responses_settings(&env, &second_resume_server.uri());
    mount_two_turn_provider(&second_resume_server).await;

    let resumed_again = env
        .command()
        .args([
            "--allow-session-model-change",
            "--resume",
            &session_id,
            "call the tool a third time",
        ])
        .env(TEST_KEY, "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute the second cake --resume");
    assert!(
        resumed_again.status.success(),
        "the second resumed run should succeed. stderr: {}",
        String::from_utf8_lossy(&resumed_again.stderr)
    );
    let requests = second_resume_server
        .received_requests()
        .await
        .expect("recorded second resume requests");
    assert_eq!(
        requests.len(),
        2,
        "a second resume should still call the tool without another search turn"
    );
    assert!(
        String::from_utf8_lossy(&requests[0].body).contains("mcp__fixture__echo"),
        "the restored tool schema should still be present on the second resume"
    );

    let session = session_file_contents(&env);
    assert_eq!(
        session.matches("\"type\":\"mcp_tools_activated\"").count(),
        1,
        "resume must not re-record an already-activated tool: {session}"
    );
}

#[tokio::test]
async fn explicit_tool_selection_falls_back_to_direct_registration() {
    let env = TestEnv::new("cake-mcp-selected-tool-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    let script = write_fixture_server(&env);
    write_global_mcp_config(&env, &script, "modern", "deferred");
    mount_two_turn_provider(&mock_server).await;

    let output = env
        .command()
        .args([
            "--tools",
            "mcp__fixture__echo",
            "say hello through the selected fixture",
        ])
        .env(TEST_KEY, "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert!(
        output.status.success(),
        "cake should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let session = session_file_contents(&env);
    assert!(
        session.contains("- **mcp__fixture__echo**:"),
        "an explicitly selected deferred tool should be listed directly: {session}"
    );
    assert!(
        !session.contains("- **ToolSearch**:"),
        "ToolSearch is not in the exact-name allowlist: {session}"
    );
    assert!(session.contains("echo: hello"));
}

#[tokio::test]
async fn legacy_mcp_server_also_works() {
    let env = TestEnv::new("cake-mcp-legacy-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    let script = write_fixture_server(&env);
    write_global_mcp_config(&env, &script, "legacy", "direct");
    mount_two_turn_provider(&mock_server).await;

    let output = env
        .command()
        .arg("say hello through the legacy fixture")
        .env(TEST_KEY, "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert!(
        output.status.success(),
        "cake should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(session_file_contents(&env).contains("echo: hello"));
}

#[tokio::test]
async fn hidden_mcp_server_is_neither_started_nor_advertised() {
    let env = TestEnv::new("cake-mcp-hidden-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    let config_dir = env.home_dir.join(".config").join("cake");
    fs::create_dir_all(&config_dir).expect("failed to create global config directory");
    fs::write(
        config_dir.join("mcp.json"),
        r#"{"mcpServers":{"broken":{"command":"/nonexistent/server","exposure":"hidden"}}}"#,
    )
    .expect("failed to write global mcp.json");

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    let output = env
        .command()
        .arg("say hello")
        .env(TEST_KEY, "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert!(
        output.status.success(),
        "cake should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("skipping MCP server"),
        "a hidden server must not be started: {stderr}"
    );
    let session = session_file_contents(&env);
    assert!(!session.contains("broken"), "{session}");
    assert!(!session.contains("- **ToolSearch**:"), "{session}");
}

#[tokio::test]
async fn read_only_sandbox_starts_no_mcp_server() {
    let env = TestEnv::new("cake-mcp-read-only-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    // A missing executable would warn if discovery ran; it must not.
    let config_dir = env.home_dir.join(".config").join("cake");
    fs::create_dir_all(&config_dir).expect("failed to create global config directory");
    fs::write(
        config_dir.join("mcp.json"),
        r#"{"mcpServers":{"broken":{"command":"/nonexistent/server"}}}"#,
    )
    .expect("failed to write global mcp.json");

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    let output = env
        .command()
        .args(["--sandbox", "read-only", "say hello"])
        .env(TEST_KEY, "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert!(
        output.status.success(),
        "cake should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("skipping MCP server"),
        "read-only must not start MCP servers: {stderr}"
    );
    let session = session_file_contents(&env);
    assert!(
        !session.contains("mcp__broken__"),
        "read-only must not register MCP tools: {session}"
    );
}

#[tokio::test]
async fn untrusted_project_mcp_config_fails_before_startup() {
    let env = TestEnv::new("cake-mcp-untrusted-test");
    let script = write_fixture_server(&env);
    let project_config = env.workspace_dir.join(".cake").join("mcp.json");
    fs::create_dir_all(project_config.parent().unwrap()).expect("failed to create .cake");
    fs::write(
        &project_config,
        serde_json::json!({
            "mcpServers": {
                "fixture": { "command": "python3", "args": [script.display().to_string()] }
            }
        })
        .to_string(),
    )
    .expect("failed to write project mcp.json");

    let output = env
        .command()
        .arg("say hello")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert_eq!(
        output.status.code(),
        Some(3),
        "untrusted project authority should exit 3. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(".cake/mcp.json"),
        "the diagnostic should name the authority file: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn a_missing_explicit_mcp_config_is_an_input_error() {
    let env = TestEnv::new("cake-mcp-missing-config-test");
    let output = env
        .command()
        .args([
            "--mcp-config",
            "/this/path/does/not/exist/mcp.json",
            "say hello",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert_eq!(
        output.status.code(),
        Some(3),
        "a missing --mcp-config file should exit 3. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("does not exist"),
        "the diagnostic should say the file is missing: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn request_tool_names(request: &serde_json::Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn tool_list_change_reaches_the_next_provider_request() {
    let env = TestEnv::new("cake-mcp-list-change-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());
    let script = write_fixture_server(&env);
    write_dynamic_mcp_config(&env, &script, "modern", "direct");

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mutate_call_response()))
        .up_to_n_times(1)
        .expect(1)
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    let output = env
        .command()
        .arg("change the tool list")
        .env(TEST_KEY, "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert!(
        output.status.success(),
        "cake should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let requests = mock_server
        .received_requests()
        .await
        .expect("recorded requests");
    assert_eq!(requests.len(), 2, "one mutation turn and one final turn");
    let first: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("first request body is JSON");
    let second: serde_json::Value =
        serde_json::from_slice(&requests[1].body).expect("second request body is JSON");

    let first_tools = request_tool_names(&first);
    assert!(
        first_tools.contains(&"mcp__fixture__mutate".to_string()),
        "{first_tools:?}"
    );
    assert!(
        first_tools.contains(&"mcp__fixture__fail".to_string()),
        "{first_tools:?}"
    );

    let second_tools = request_tool_names(&second);
    assert!(
        second_tools.contains(&"mcp__fixture__added".to_string()),
        "the added tool should reach the next request: {second_tools:?}"
    );
    assert!(
        !second_tools.contains(&"mcp__fixture__fail".to_string()),
        "the removed tool should disappear from the next request: {second_tools:?}"
    );
    assert!(
        !second_tools.contains(&"mcp__fixture__mutate".to_string()),
        "the mutation tool should disappear from the next request: {second_tools:?}"
    );
    let echo = second["tools"]
        .as_array()
        .and_then(|tools| {
            tools
                .iter()
                .find(|tool| tool["name"] == "mcp__fixture__echo")
        })
        .expect("echo should remain in the request");
    assert!(
        echo["description"]
            .as_str()
            .is_some_and(|description| description.contains("after the change")),
        "the changed definition should reach the request: {echo}"
    );
}
