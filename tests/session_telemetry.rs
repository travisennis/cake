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

fn reasoning_only_response() -> serde_json::Value {
    serde_json::json!({
        "id": "resp-reasoning",
        "status": "incomplete",
        "incomplete_details": {
            "reason": "max_output_tokens"
        },
        "output": [
            {
                "type": "reasoning",
                "id": "r-1",
                "summary": [{
                    "type": "summary_text",
                    "text": "partial reasoning"
                }]
            }
        ],
        "usage": {
            "input_tokens": 10,
            "output_tokens": 5,
            "total_tokens": 15
        }
    })
}

/// An SSE `response.failed` event matching the observed provider shape
/// (openai/codex#1002): both `type` and `code` set, plus an id and message.
fn response_failed_body(id: &str, code: &str, message: &str) -> String {
    format!(
        "data: {}\n\n",
        serde_json::json!({
            "type": "response.failed",
            "response": {
                "id": id,
                "error": {
                    "message": message,
                    "type": code,
                    "code": code,
                }
            }
        })
    )
}

fn response_failed_body_with_usage(id: &str, code: &str, message: &str) -> String {
    format!(
        "data: {}\n\n",
        serde_json::json!({
            "type": "response.failed",
            "response": {
                "id": id,
                "error": {
                    "message": message,
                    "type": code,
                    "code": code,
                },
                "usage": {
                    "input_tokens": 11,
                    "output_tokens": 7,
                    "total_tokens": 18,
                },
            }
        })
    )
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

#[tokio::test]
async fn response_failed_server_error_retries_and_recovers() {
    let env = TestEnv::new("cake-response-failed-retry-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(response_failed_body_with_usage(
                "resp-fail",
                "server_error",
                "provider exploded",
            )),
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

    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["usage"]["total_tokens"], 33);
    let session = session_records(&env);
    let usages = session
        .iter()
        .filter(|record| record["type"] == "turn_usage")
        .collect::<Vec<_>>();
    assert_eq!(
        usages.len(),
        2,
        "each reported retry attempt should be audited"
    );
    assert_eq!(usages[0]["attempt"], 1);
    assert_eq!(usages[0]["terminal_class"], "response_failed");
    assert_eq!(usages[0]["usage"]["total_tokens"], 18);
    assert_eq!(usages[1]["attempt"], 2);
    assert_eq!(usages[1]["terminal_class"], "completed");
    assert_eq!(usages[1]["usage"]["total_tokens"], 15);

    let records = telemetry_records(&env);
    let api_attempts = records
        .iter()
        .filter(|record| record["type"] == "api_attempt")
        .collect::<Vec<_>>();

    assert_eq!(api_attempts.len(), 2, "{records:#?}");
    let failed = api_attempts
        .iter()
        .find(|record| record["error"].as_str().is_some())
        .expect("expected a failed attempt");
    assert_eq!(failed["terminal_class"], "response_failed");
    assert_eq!(failed["provider_request_id"], "resp-fail");
    assert_eq!(failed["responses_failed"]["code"], "server_error");
    assert_eq!(failed["responses_failed"]["type"], "server_error");
    assert!(
        failed["responses_failed"]
            .get("provider_request_id")
            .is_none(),
        "provider request id should not be duplicated in responses_failed"
    );
    assert!(
        records.iter().any(|record| {
            record["type"] == "retry_scheduled" && record["reason"] == "server_error"
        }),
        "{records:#?}"
    );
    assert!(
        records.iter().any(|record| {
            record["type"] == "api_attempt" && record["terminal_class"] == "completed"
        }),
        "completed attempt should be classified: {records:#?}"
    );
}

#[tokio::test]
async fn response_failed_usage_is_persisted_in_session_totals_without_stream_record() {
    let env = TestEnv::new("cake-response-failed-usage-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(response_failed_body_with_usage(
                "resp-usage",
                "invalid_request_error",
                "bad request",
            )),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let output = env
        .command()
        .arg("--output-format")
        .arg("stream-json")
        .arg("test prompt")
        .env("SESSION_TELEMETRY_TEST_KEY", "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert!(
        output.status.success(),
        "stream-json should represent the provider failure in task_complete. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stream = String::from_utf8(output.stdout).expect("stream-json should be UTF-8");
    let stream_records = stream
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(
        stream_records
            .iter()
            .all(|record| record["type"] != "turn_usage"),
        "session-only usage audit records must not enter stream-json: {stream_records:#?}"
    );
    let complete = stream_records
        .iter()
        .find(|record| record["type"] == "task_complete")
        .expect("stream-json should contain task_complete");
    assert_eq!(complete["subtype"], "error_during_execution");
    assert_eq!(complete["usage"]["input_tokens"], 11);
    assert_eq!(complete["usage"]["output_tokens"], 7);
    assert_eq!(complete["usage"]["total_tokens"], 18);

    let session = session_records(&env);
    let usage = session
        .iter()
        .find(|record| record["type"] == "turn_usage")
        .expect("failed provider usage should be persisted");
    assert_eq!(usage["turn"], 1);
    assert_eq!(usage["attempt"], 1);
    assert_eq!(usage["terminal_class"], "response_failed");
    assert_eq!(usage["usage"]["total_tokens"], 18);

    let telemetry = telemetry_records(&env);
    let attempt = telemetry
        .iter()
        .find(|record| record["type"] == "api_attempt")
        .expect("provider attempt telemetry should exist");
    assert_eq!(attempt["usage"]["total_tokens"], 18);
}

#[tokio::test]
async fn discarded_overflow_usage_is_settled_before_retry() {
    let env = TestEnv::new("cake-overflow-usage-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": {
                "message": "input length and max_tokens exceed context limit: 12000 + 5000 > 16384"
            },
            "usage": {
                "input_tokens": 12,
                "output_tokens": 3,
                "total_tokens": 15
            }
        })))
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
        "overflow recovery should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["usage"]["total_tokens"], 30);

    let session = session_records(&env);
    let usages = session
        .iter()
        .filter(|record| record["type"] == "turn_usage")
        .collect::<Vec<_>>();
    assert_eq!(usages.len(), 2, "both billed attempts should be audited");
    assert_eq!(usages[0]["turn"], 1);
    assert_eq!(usages[0]["attempt"], 1);
    assert_eq!(usages[0]["terminal_class"], "http");
    assert_eq!(usages[0]["usage"]["total_tokens"], 15);
    assert_eq!(usages[1]["turn"], 1);
    assert_eq!(usages[1]["attempt"], 2);
    assert_eq!(usages[1]["terminal_class"], "completed");
    assert_eq!(usages[1]["usage"]["total_tokens"], 15);
}

#[tokio::test]
async fn response_failed_semantic_error_is_terminal() {
    let env = TestEnv::new("cake-response-failed-semantic-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(response_failed_body(
                "resp-auth",
                "invalid_request_error",
                "bad request",
            )),
        )
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
        !output.status.success(),
        "cake should fail on a semantic response.failed"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Responses API stream failed: bad request"),
        "user-facing error should stay compatible"
    );

    let records = telemetry_records(&env);
    let api_attempts = records
        .iter()
        .filter(|record| record["type"] == "api_attempt")
        .collect::<Vec<_>>();
    assert_eq!(api_attempts.len(), 1, "{records:#?}");
    assert_eq!(api_attempts[0]["terminal_class"], "response_failed");
    assert_eq!(
        api_attempts[0]["responses_failed"]["code"],
        "invalid_request_error"
    );
    assert!(
        api_attempts[0]["usage"].is_null(),
        "usage stays unavailable"
    );
    assert!(
        !records
            .iter()
            .any(|record| record["type"] == "retry_scheduled"),
        "semantic failures must not retry: {records:#?}"
    );
}

#[tokio::test]
async fn semantic_incomplete_recovery_streams_once_and_records_retry_reason() {
    let env = TestEnv::new("cake-session-telemetry-semantic-recovery-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(reasoning_only_response()))
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
        .arg("stream-json")
        .arg("test prompt")
        .env("SESSION_TELEMETRY_TEST_KEY", "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert!(
        output.status.success(),
        "cake should recover successfully. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stream = String::from_utf8(output.stdout).expect("stream-json should be UTF-8");
    let stream_records = stream
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        stream_records
            .iter()
            .filter(|record| record["type"] == "task_complete")
            .count(),
        1,
        "{stream_records:#?}"
    );
    assert_eq!(
        stream_records.last().unwrap()["subtype"],
        "success",
        "{stream_records:#?}"
    );
    assert!(stream_records.iter().any(|record| {
        record["type"] == "reasoning"
            && record["summary"][0]["type"] == "summary_text"
            && record["summary"][0]["text"] == "partial reasoning"
    }));
    assert!(stream_records.iter().any(|record| {
        record["type"] == "message"
            && record["role"] == "user"
            && record["content"]
                .as_str()
                .is_some_and(|content| content.contains("provide the final answer now"))
    }));
    let task_ids = stream_records
        .iter()
        .filter_map(|record| record["task_id"].as_str())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(task_ids.len(), 1, "{stream_records:#?}");

    let telemetry = telemetry_records(&env);
    assert_eq!(
        telemetry
            .iter()
            .filter(|record| record["type"] == "api_attempt")
            .count(),
        2,
        "{telemetry:#?}"
    );
    assert!(telemetry.iter().any(|record| {
        record["type"] == "retry_scheduled"
            && record["reason"] == "semantic_incomplete"
            && record["delay_ms"] == 0
    }));
}

#[tokio::test]
async fn semantic_incomplete_recovery_reports_text_progress() {
    let env = TestEnv::new("cake-semantic-recovery-progress-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(reasoning_only_response()))
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
        .arg("test prompt")
        .env("SESSION_TELEMETRY_TEST_KEY", "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "Hello!\n");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "Retrying incomplete model turn (semantic_incomplete, attempt 1/1)\n"
    );
}

#[tokio::test]
async fn session_telemetry_records_compensation_events() {
    let env = TestEnv::new("cake-session-telemetry-compensation-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());

    let file = env.workspace_dir.join("notes.txt");
    fs::write(&file, "hello").expect("fixture file should be writable");

    // Turn 1: two Edit calls on the same file. The first carries trailing
    // garbage after its balanced object, which the repair pass removes; the
    // second is plain valid JSON. Both mutate the same path, so the scheduler
    // serializes them (one reordering).
    let clean_arguments =
        serde_json::json!({ "path": &file, "edits": [{ "old_text": "hello", "new_text": "hi" }] })
            .to_string();
    let repaired_arguments = format!("{clean_arguments}}}extra");
    let second_arguments =
        serde_json::json!({ "path": &file, "edits": [{ "old_text": "hi", "new_text": "hey" }] })
            .to_string();

    let tool_call_response = serde_json::json!({
        "id": "resp-tool",
        "output": [
            {
                "type": "function_call",
                "id": "fc-1",
                "call_id": "call-1",
                "name": "Edit",
                "arguments": repaired_arguments,
            },
            {
                "type": "function_call",
                "id": "fc-2",
                "call_id": "call-2",
                "name": "Edit",
                "arguments": second_arguments,
            },
        ],
        "usage": { "input_tokens": 10, "output_tokens": 5, "total_tokens": 15 },
    });

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tool_call_response))
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
        "cake should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let records = telemetry_records(&env);
    let compensations = records
        .iter()
        .filter(|record| record["type"] == "compensation")
        .collect::<Vec<_>>();
    let kinds = compensations
        .iter()
        .map(|record| {
            (
                record["kind"].as_str().unwrap_or_default(),
                record["detail"].as_str().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();

    assert!(
        kinds.contains(&("json_repair", "Edit")),
        "expected a json_repair compensation for Edit: {kinds:#?} in {records:#?}"
    );
    assert!(
        kinds.iter().any(|(kind, detail)| {
            *kind == "same_path_serialization" && detail.ends_with("notes.txt")
        }),
        "expected a same-path serialization compensation: {kinds:#?} in {records:#?}"
    );
    assert!(
        records
            .iter()
            .all(|record| record["session_id"].is_string()),
        "compensation records carry session identity"
    );
}

fn session_records(env: &TestEnv) -> Vec<serde_json::Value> {
    let session_file = only_file_in(&env.data_dir.join("sessions"));
    let contents = fs::read_to_string(&session_file).expect("session file should be readable");
    contents
        .lines()
        .map(|line| serde_json::from_str(line).expect("session line should be valid JSON"))
        .collect()
}

#[cfg(unix)]
fn stderr_tail(child: &mut std::process::Child) -> String {
    use std::io::Read;
    child
        .stderr
        .take()
        .map(|mut pipe| {
            let mut text = String::new();
            let _ = pipe.read_to_string(&mut text).ok();
            text
        })
        .unwrap_or_default()
}

#[cfg(unix)]
async fn wait_for_provider_request(mock_server: &MockServer) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let received = mock_server
            .received_requests()
            .await
            .is_some_and(|requests| !requests.is_empty());
        if received {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for the provider request"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[cfg(unix)]
async fn wait_for_session_record(env: &TestEnv, record_type: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let found = fs::read_dir(env.data_dir.join("sessions")).is_ok_and(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                fs::read_to_string(entry.path()).is_ok_and(|contents| {
                    contents.lines().any(|line| {
                        serde_json::from_str::<serde_json::Value>(line)
                            .is_ok_and(|record| record["type"] == record_type)
                    })
                })
            })
        });
        if found {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for session record {record_type}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// Cancelling cake with SIGTERM (the signal the cake-repl runner sends)
/// takes the same graceful interruption path as Ctrl-C: the session closes
/// with an interrupted `task_complete` record, the telemetry summary is
/// flushed, and the process exits 130.
#[cfg(unix)]
#[tokio::test]
async fn sigterm_interrupts_session_cleanly() {
    let env = TestEnv::new("cake-sigterm-interrupt-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());

    // Delay the provider response so the agent turn is still in flight when
    // the SIGTERM arrives.
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(success_response())
                .set_delay(std::time::Duration::from_secs(30)),
        )
        .mount(&mock_server)
        .await;

    let mut child = env
        .command()
        .arg("--output-format")
        .arg("json")
        .arg("test prompt")
        .env("SESSION_TELEMETRY_TEST_KEY", "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn cake");

    // Wait until the provider request arrives. By then the agent turn is in
    // flight and the SIGTERM handler is installed: the interrupt futures are
    // polled in the same first `select!` iteration that starts the turn, so
    // registration always precedes the network round trip.
    wait_for_provider_request(&mock_server).await;

    // SAFETY: `child.id()` is the PID of the cake process this test just
    // spawned and is still ours to signal; SIGTERM is exactly the signal the
    // cake-repl runner sends when cancelling a run.
    unsafe {
        libc::kill(
            i32::try_from(child.id()).expect("pid must fit in an i32"),
            libc::SIGTERM,
        );
    }

    let status = child.wait().expect("failed to wait for cake");
    assert_eq!(
        status.code(),
        Some(130),
        "cake should exit with the interrupted status. stderr: {}",
        stderr_tail(&mut child)
    );

    let session = session_records(&env);
    let complete = session
        .iter()
        .find(|record| record["type"] == "task_complete")
        .expect("session should end with a task_complete record");
    assert_eq!(complete["subtype"], "interrupted");
    assert_eq!(complete["is_error"], true);

    let telemetry = telemetry_records(&env);
    let summary = telemetry
        .iter()
        .find(|record| record["type"] == "session_summary")
        .expect("telemetry should contain a session_summary");
    assert_eq!(summary["success"], false);
}

/// A finalized provider attempt must reach the telemetry sidecar before retry
/// backoff. Otherwise SIGTERM can preserve its settled usage in the session and
/// summary while dropping the attempt that explains those tokens.
#[cfg(unix)]
#[tokio::test]
async fn sigterm_during_retry_keeps_settled_attempt_telemetry() {
    let env = TestEnv::new("cake-sigterm-retry-telemetry-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "30")
                .set_body_json(serde_json::json!({
                    "error": {"message": "retry later"},
                    "usage": {
                        "input_tokens": 11,
                        "output_tokens": 7,
                        "total_tokens": 18
                    }
                })),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let mut child = env
        .command()
        .arg("--output-format")
        .arg("json")
        .arg("test prompt")
        .env("SESSION_TELEMETRY_TEST_KEY", "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn cake");

    wait_for_session_record(&env, "turn_usage").await;
    // Settlement precedes retry classification by only synchronous work. Give
    // that work time to enter the 30-second backoff before interrupting it.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // SAFETY: this is the still-running Cake child spawned by this test.
    unsafe {
        libc::kill(
            i32::try_from(child.id()).expect("pid must fit in an i32"),
            libc::SIGTERM,
        );
    }

    let status = child.wait().expect("failed to wait for cake");
    assert_eq!(
        status.code(),
        Some(130),
        "cake should exit with the interrupted status. stderr: {}",
        stderr_tail(&mut child)
    );

    let session = session_records(&env);
    let usage = session
        .iter()
        .find(|record| record["type"] == "turn_usage")
        .expect("session should preserve the failed attempt usage");
    assert_eq!(usage["usage"]["total_tokens"], 18);
    let complete = session
        .iter()
        .find(|record| record["type"] == "task_complete")
        .expect("session should end with task_complete");
    assert_eq!(complete["subtype"], "interrupted");
    assert_eq!(complete["usage"]["total_tokens"], 18);

    let telemetry = telemetry_records(&env);
    let attempt = telemetry
        .iter()
        .find(|record| record["type"] == "api_attempt")
        .expect("finalized attempt must survive interruption during backoff");
    assert_eq!(attempt["terminal_class"], "http");
    assert_eq!(attempt["usage"]["total_tokens"], 18);
    assert!(
        telemetry
            .iter()
            .any(|record| record["type"] == "retry_scheduled"),
        "retry scheduling must be durable before backoff: {telemetry:#?}"
    );
    let summary = telemetry
        .iter()
        .find(|record| record["type"] == "session_summary")
        .expect("telemetry should contain a session_summary");
    assert_eq!(summary["usage"]["total_tokens"], 18);
}
