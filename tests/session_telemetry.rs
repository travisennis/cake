#![expect(clippy::expect_used, reason = "test code uses expect for assertions")]

mod support;

use std::{fs, process::Stdio};

use support::TestEnv;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn success_response() -> serde_json::Value {
    serde_json::json!({
        "id": "resp-123",
        "output": [
            {
                "type": "message",
                "id": "msg-1",
                "status": "completed",
                "content": [
                    {
                        "type": "output_text",
                        "text": "Hello!"
                    }
                ]
            }
        ],
        "usage": {
            "input_tokens": 10,
            "output_tokens": 5,
            "total_tokens": 15
        }
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
api_key_env = "SESSION_TELEMETRY_TEST_KEY"
api_type = "responses"
"#
    ));
}

fn only_file_in(dir: &std::path::Path) -> std::path::PathBuf {
    let entries = fs::read_dir(dir)
        .expect("directory should exist")
        .collect::<Result<Vec<_>, _>>()
        .expect("directory should be readable");
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one file in {}",
        dir.display()
    );
    entries[0].path()
}

fn telemetry_records(env: &TestEnv) -> Vec<serde_json::Value> {
    let session_file = only_file_in(&env.data_dir.join("sessions"));
    let session_id = session_file
        .file_stem()
        .expect("session file should have stem")
        .to_string_lossy();
    let telemetry_file = env
        .data_dir
        .join("session-telemetry")
        .join(format!("{session_id}.ndjson"));
    let contents =
        fs::read_to_string(&telemetry_file).expect("telemetry sidecar should be readable");

    contents
        .lines()
        .map(|line| serde_json::from_str(line).expect("telemetry line should be valid JSON"))
        .collect()
}

#[tokio::test]
async fn session_telemetry_creates_sidecar_on_success() {
    let env = TestEnv::new("cake-session-telemetry-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    let output = env
        .command()
        .arg("--output-format")
        .arg("json")
        .arg("test prompt")
        .env("SESSION_TELEMETRY_TEST_KEY", "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert!(
        output.status.success(),
        "cake should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let records = telemetry_records(&env);
    let types = records
        .iter()
        .map(|record| record["type"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();

    assert!(types.contains(&"telemetry_init"), "{types:?}");
    assert!(types.contains(&"api_attempt"), "{types:?}");
    assert!(types.contains(&"session_summary"), "{types:?}");
    assert!(
        records
            .iter()
            .all(|record| record["session_id"].is_string())
    );
    assert!(
        records
            .iter()
            .all(|record| record["invocation_id"].is_string())
    );
}

#[tokio::test]
async fn session_telemetry_records_retry_attempts() {
    let env = TestEnv::new("cake-session-telemetry-retry-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "0")
                .set_body_json(serde_json::json!({
                    "error": {
                        "message": "slow down"
                    }
                })),
        )
        .expect(1)
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    let output = env
        .command()
        .arg("--output-format")
        .arg("json")
        .arg("test prompt")
        .env("SESSION_TELEMETRY_TEST_KEY", "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert!(
        output.status.success(),
        "cake should succeed after retry. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let records = telemetry_records(&env);
    let api_attempts = records
        .iter()
        .filter(|record| record["type"] == "api_attempt")
        .count();
    assert_eq!(api_attempts, 2, "{records:#?}");
    assert!(
        records.iter().any(|record| {
            record["type"] == "retry_scheduled" && record["reason"] == "rate_limit"
        }),
        "{records:#?}"
    );
}
