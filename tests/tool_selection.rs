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
