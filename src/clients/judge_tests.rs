use super::*;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Minimal model config pointed at a wiremock server.
fn test_config(base_url: String) -> ResolvedModelConfig {
    let model_config = crate::config::model::ModelConfig {
        model: "agent/model".to_string(),
        api_type: crate::config::model::ApiType::ChatCompletions,
        base_url,
        api_key_env: "JUDGE_TEST_KEY".to_string(),
        provider: None,
        provider_headers: None,
        temperature: Some(0.0),
        top_p: None,
        max_output_tokens: Some(128),
        reasoning_effort: None,
        reasoning_summary: None,
        reasoning_max_tokens: None,
        providers: vec![],
    };
    ResolvedModelConfig {
        model_config,
        api_key: "test-key".to_string(),
    }
}

fn chat_response(content: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-judge",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": "stop"
        }]
    })
}

fn request(command: &str, reason: Option<&str>) -> JudgeRequest {
    JudgeRequest::new(
        command.to_string(),
        std::path::PathBuf::from("/work/project"),
        reason.map(str::to_string),
    )
}

fn judge_client(mock_server: &MockServer) -> JudgeClient {
    JudgeClient::new(test_config(mock_server.uri()), Duration::from_secs(5))
}

#[tokio::test]
async fn judge_returns_block_verdict() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(
            r#"{"verdict":"block","code":"git-force-push","message":"Prefer push --force-with-lease.","confidence":0.93}"#,
        )))
        .mount(&mock_server)
        .await;

    let verdict = judge_client(&mock_server)
        .judge(request("git push --force", None))
        .await
        .unwrap();

    assert_eq!(verdict.decision, JudgeDecision::Block);
    assert_eq!(verdict.code.as_deref(), Some("git-force-push"));
    assert_eq!(verdict.message, "Prefer push --force-with-lease.");
    assert_eq!(verdict.confidence, Some(0.93));
}

#[tokio::test]
async fn judge_parses_allow_without_code() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(
            r#"{"verdict":"allow","message":"Safe","confidence":0.9}"#,
        )))
        .mount(&mock_server)
        .await;

    let verdict = judge_client(&mock_server)
        .judge(request("git status", None))
        .await
        .unwrap();

    assert_eq!(verdict.decision, JudgeDecision::Allow);
    assert_eq!(verdict.code, None);
}

#[tokio::test]
async fn judge_round_trips_command_cwd_and_reason() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(chat_response(r#"{"verdict":"allow","message":"Safe"}"#)),
        )
        .mount(&mock_server)
        .await;

    let reason = "I want to inspect the working tree";
    let verdict = judge_client(&mock_server)
        .judge(request("git status", Some(reason)))
        .await
        .unwrap();
    assert_eq!(verdict.decision, JudgeDecision::Allow);

    let requests = mock_server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["model"], "agent/model");
    let messages = body["messages"].as_array().unwrap();
    let user_message = messages.iter().find(|m| m["role"] == "user").unwrap();
    let content = user_message["content"].as_str().unwrap();
    assert!(content.contains("git status"));
    assert!(content.contains("/work/project"));
    assert!(content.contains(reason));
    assert!(content.contains("untrusted"));
}

#[tokio::test]
async fn judge_model_override_swaps_model_identifier() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(chat_response(r#"{"verdict":"allow","message":"Safe"}"#)),
        )
        .mount(&mock_server)
        .await;

    let client = judge_client(&mock_server).with_model_override(Some("judge-model-v2"));
    assert_eq!(client.model_name(), "judge-model-v2");
    client.judge(request("git status", None)).await.unwrap();

    let requests = mock_server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["model"], "judge-model-v2");
}

#[tokio::test]
async fn judge_timeout_yields_timeout_error() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(chat_response(r#"{"verdict":"allow","message":"Safe"}"#))
                .set_delay(Duration::from_millis(500)),
        )
        .mount(&mock_server)
        .await;

    let client = JudgeClient::new(test_config(mock_server.uri()), Duration::from_millis(50));
    let error = client.judge(request("git status", None)).await.unwrap_err();
    assert!(matches!(error, JudgeError::Timeout(_)));
}

#[tokio::test]
async fn judge_http_error_yields_transport_error() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("provider exploded"))
        .mount(&mock_server)
        .await;

    let error = judge_client(&mock_server)
        .judge(request("git status", None))
        .await
        .unwrap_err();
    assert!(matches!(error, JudgeError::Transport(ref msg) if msg.contains("500")));
}

#[tokio::test]
async fn judge_malformed_verdict_yields_malformed_error() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(chat_response("I do not judge commands.")),
        )
        .mount(&mock_server)
        .await;

    let error = judge_client(&mock_server)
        .judge(request("git status", None))
        .await
        .unwrap_err();
    assert!(matches!(error, JudgeError::Malformed(_)));
}

#[tokio::test]
async fn judge_refusal_yields_refusal_error() {
    let mock_server = MockServer::start().await;
    let mut body = chat_response(""); // content irrelevant; refusal wins
    body["choices"][0]["message"]["refusal"] = serde_json::json!("I cannot judge commands");
    body["choices"][0]["message"]["content"] = serde_json::Value::Null;
    body["choices"][0]["finish_reason"] = serde_json::json!("refusal");
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&mock_server)
        .await;

    let error = judge_client(&mock_server)
        .judge(request("git status", None))
        .await
        .unwrap_err();
    assert!(matches!(error, JudgeError::Refusal));
}

#[tokio::test]
async fn judge_works_through_responses_backend() {
    let mock_server = MockServer::start().await;
    let body = serde_json::json!({
        "id": "resp-judge",
        "output": [{
            "type": "message",
            "id": "msg-1",
            "status": "completed",
            "content": [
                {"type": "output_text", "text": r#"{"verdict":"warn","code":"rg-replace-footgun","message":"Prefer rg -l.","confidence":0.7}"#}
            ]
        }],
        "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}
    });
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&mock_server)
        .await;

    let mut config = test_config(mock_server.uri());
    config.model_config.api_type = crate::config::model::ApiType::Responses;
    let client = JudgeClient::new(config, Duration::from_secs(5));
    let verdict = client.judge(request("rg -rn foo", None)).await.unwrap();

    assert_eq!(verdict.decision, JudgeDecision::Warn);
    assert_eq!(verdict.code.as_deref(), Some("rg-replace-footgun"));
}

// =============================================================================
// parse_verdict unit tests
// =============================================================================

#[test]
fn parse_verdict_rejects_unknown_verdict() {
    let error = parse_verdict(r#"{"verdict":"maybe","message":"x"}"#).unwrap_err();
    assert!(matches!(error, JudgeError::Malformed(_)));
}

#[test]
fn parse_verdict_rejects_out_of_range_confidence() {
    let error = parse_verdict(r#"{"verdict":"allow","message":"x","confidence":1.5}"#).unwrap_err();
    assert!(matches!(error, JudgeError::Malformed(_)));
}

#[test]
fn parse_verdict_recovers_raw_control_characters() {
    let content = "{\"verdict\":\"block\",\"code\":\"destructive-rm\",\"message\":\"line1\nline2\",\"confidence\":0.8}";
    let verdict = parse_verdict(content).unwrap();
    assert_eq!(verdict.decision, JudgeDecision::Block);
    assert!(verdict.message.contains('\n'));
}

#[test]
fn parse_verdict_missing_message_is_malformed() {
    let error = parse_verdict(r#"{"verdict":"block"}"#).unwrap_err();
    assert!(matches!(error, JudgeError::Malformed(_)));
}
