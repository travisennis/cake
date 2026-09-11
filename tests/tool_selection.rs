//! End-to-end coverage for explicit tool selection.

#![expect(clippy::expect_used, reason = "test code uses expect for assertions")]

mod support;

use std::{fs, process::Stdio};

use support::TestEnv;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TEST_KEY: &str = "TOOL_SELECTION_TEST_KEY";

fn success_response() -> serde_json::Value {
    serde_json::json!({
        "id": "resp-tool-selection",
        "output": [{
            "type": "message",
            "id": "msg-tool-selection",
            "status": "completed",
            "content": [{ "type": "output_text", "text": "Done." }]
        }],
        "usage": { "input_tokens": 10, "output_tokens": 5, "total_tokens": 15 }
    })
}

#[tokio::test]
async fn settings_filter_provider_tools_prompt_and_session_metadata() {
    let env = TestEnv::new("cake-tool-selection-test");
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    env.write_project_settings(&format!(
        r#"
default_model = "test"

[[models]]
name = "test"
model = "test-model"
base_url = "{}"
api_key_env = "{}"
api_type = "responses"

[tools]
enabled = ["Read", "NoSuchTool"]
"#,
        mock_server.uri(),
        TEST_KEY
    ));

    let output = env
        .command()
        .args(["inspect the selected tools"])
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
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("warning: configured tool 'NoSuchTool' is not available in this session"),
        "unknown configured tools should be warned about"
    );

    let requests = mock_server
        .received_requests()
        .await
        .expect("recorded requests");
    assert_eq!(requests.len(), 1);
    let request: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("request JSON");
    assert_eq!(request["tools"].as_array().map(Vec::len), Some(1));
    assert_eq!(request["tools"][0]["name"], "Read");
    assert_eq!(request["tool_choice"], "auto");
    let instructions = request["instructions"].as_str().expect("instructions");
    assert!(instructions.contains("- **Read**:"));
    assert!(!instructions.contains("- **Bash**:"));
    assert!(!instructions.contains("- **Edit**:"));
    assert!(!instructions.contains("- **Write**:"));

    let session_file = fs::read_dir(env.data_dir.join("sessions"))
        .expect("sessions directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .expect("new session file");
    let session_contents = fs::read_to_string(session_file).expect("session file");
    let first_line = session_contents.lines().next().expect("session metadata");
    let metadata: serde_json::Value = serde_json::from_str(first_line).expect("metadata JSON");
    assert_eq!(metadata["tools"], serde_json::json!(["Read"]));
}

#[tokio::test]
async fn empty_tool_selection_omits_provider_tool_fields() {
    let env = TestEnv::new("cake-empty-tool-selection-test");
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    env.write_project_settings(&format!(
        r#"
default_model = "test"

[[models]]
name = "test"
model = "test-model"
base_url = "{}"
api_key_env = "{}"
api_type = "responses"

[tools]
enabled = []
"#,
        mock_server.uri(),
        TEST_KEY
    ));

    let output = env
        .command()
        .args(["respond without tools"])
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
    let request: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("request JSON");
    assert!(request.get("tools").is_none());
    assert!(request.get("tool_choice").is_none());
}

#[tokio::test]
async fn selected_tool_description_omits_disabled_tools() {
    let env = TestEnv::new("cake-tool-description-selection-test");
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    env.write_project_settings(&format!(
        r#"
default_model = "test"

[[models]]
name = "test"
model = "test-model"
base_url = "{}"
api_key_env = "{}"
api_type = "responses"

[tools]
enabled = ["Bash"]
"#,
        mock_server.uri(),
        TEST_KEY
    ));

    let output = env
        .command()
        .args(["inspect the Bash tool description"])
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
    let request: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("request JSON");
    let description = request["tools"][0]["description"]
        .as_str()
        .expect("Bash description");
    assert!(description.contains("Content search: Use rg"));
    assert!(!description.contains("Read"));
    assert!(!description.contains("Edit"));
    assert!(!description.contains("Write"));
}

/// Read the `tools` recorded in the new session's metadata record.
fn session_tools(env: &TestEnv) -> Vec<String> {
    let session_file = fs::read_dir(env.data_dir.join("sessions"))
        .expect("sessions directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .expect("new session file");
    let contents = fs::read_to_string(session_file).expect("session file");
    let first_line = contents.lines().next().expect("session metadata");
    let metadata: serde_json::Value = serde_json::from_str(first_line).expect("metadata JSON");
    metadata["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|value| value.as_str().expect("tool name").to_string())
        .collect()
}

/// Provider tool definitions carry the registered name under `name`.
fn request_tool_names(request: &serde_json::Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .map(|tool| tool["name"].as_str().expect("tool name").to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn cli_tools_override_settings() {
    let env = TestEnv::new("cake-cli-tools-override-test");
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    env.write_project_settings(&format!(
        r#"
default_model = "test"

[[models]]
name = "test"
model = "test-model"
base_url = "{}"
api_key_env = "{}"
api_type = "responses"

[tools]
enabled = ["Bash"]
"#,
        mock_server.uri(),
        TEST_KEY
    ));

    let output = env
        .command()
        .args(["--tools", "Read,Edit", "inspect the selected tools"])
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
    assert_eq!(requests.len(), 1);
    let request: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("request JSON");
    let mut names = request_tool_names(&request);
    names.sort();
    assert_eq!(names, vec!["Edit".to_string(), "Read".to_string()]);
    let instructions = request["instructions"].as_str().expect("instructions");
    assert!(instructions.contains("- **Read**:"));
    assert!(instructions.contains("- **Edit**:"));
    assert!(!instructions.contains("- **Bash**:"));

    let mut recorded = session_tools(&env);
    recorded.sort();
    assert_eq!(recorded, vec!["Edit".to_string(), "Read".to_string()]);
}

#[tokio::test]
async fn cli_tools_override_profile_selection() {
    let env = TestEnv::new("cake-cli-tools-profile-test");
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    env.write_project_settings(&format!(
        r#"
default_model = "test"

[[models]]
name = "test"
model = "test-model"
base_url = "{}"
api_key_env = "{}"
api_type = "responses"

[profiles.review.tools]
enabled = ["Read"]
"#,
        mock_server.uri(),
        TEST_KEY
    ));

    let output = env
        .command()
        .args([
            "--profile",
            "review",
            "--tools",
            "Edit",
            "inspect the selected tools",
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

    let requests = mock_server
        .received_requests()
        .await
        .expect("recorded requests");
    let request: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("request JSON");
    assert_eq!(request_tool_names(&request), vec!["Edit".to_string()]);
    assert_eq!(session_tools(&env), vec!["Edit".to_string()]);
}

#[tokio::test]
async fn no_tools_omits_provider_fields_and_records_empty_selection() {
    let env = TestEnv::new("cake-no-tools-test");
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    env.write_project_settings(&format!(
        r#"
default_model = "test"

[[models]]
name = "test"
model = "test-model"
base_url = "{}"
api_key_env = "{}"
api_type = "responses"
"#,
        mock_server.uri(),
        TEST_KEY
    ));

    let output = env
        .command()
        .args(["--no-tools", "respond without tools"])
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
    let request: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("request JSON");
    assert!(request.get("tools").is_none());
    assert!(request.get("tool_choice").is_none());
    assert!(session_tools(&env).is_empty());
}

#[tokio::test]
async fn cli_unknown_tool_name_warns_and_is_not_registered() {
    let env = TestEnv::new("cake-cli-tools-unknown-test");
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    env.write_project_settings(&format!(
        r#"
default_model = "test"

[[models]]
name = "test"
model = "test-model"
base_url = "{}"
api_key_env = "{}"
api_type = "responses"
"#,
        mock_server.uri(),
        TEST_KEY
    ));

    let output = env
        .command()
        .args(["--tools", "Read,NoSuchTool", "inspect the selected tools"])
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
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("warning: configured tool 'NoSuchTool' is not available in this session"),
        "unknown CLI tools should be warned about"
    );

    let requests = mock_server
        .received_requests()
        .await
        .expect("recorded requests");
    let request: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("request JSON");
    assert_eq!(request_tool_names(&request), vec!["Read".to_string()]);
    assert_eq!(session_tools(&env), vec!["Read".to_string()]);
}

#[tokio::test]
async fn read_only_sandbox_filters_cli_tool_selection() {
    let env = TestEnv::new("cake-cli-tools-read-only-test");
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    env.write_project_settings(&format!(
        r#"
default_model = "test"

[[models]]
name = "test"
model = "test-model"
base_url = "{}"
api_key_env = "{}"
api_type = "responses"
"#,
        mock_server.uri(),
        TEST_KEY
    ));

    let output = env
        .command()
        .args([
            "--sandbox",
            "read-only",
            "--tools",
            "Edit",
            "inspect the selected tools",
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
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("warning: configured tool 'Edit' is not available in this session"),
        "sandbox-filtered CLI tools should be warned about"
    );

    let requests = mock_server
        .received_requests()
        .await
        .expect("recorded requests");
    let request: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("request JSON");
    assert!(request.get("tools").is_none());
    assert!(session_tools(&env).is_empty());
}
