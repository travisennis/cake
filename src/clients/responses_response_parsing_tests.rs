//! Tests for parsing raw HTTP responses.

use super::*;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn to_api_input_json(item: &ConversationItem) -> serde_json::Value {
    serde_json::to_value(ResponsesApiInputItem::from(item))
        .expect("Responses API input DTO serialization should be infallible")
}

/// Create a minimal valid response JSON
fn minimal_valid_response() -> serde_json::Value {
    serde_json::json!({
        "output": [{
            "type": "message",
            "id": "msg-1",
            "status": "completed",
            "content": [{
                "type": "output_text",
                "text": "Hello!"
            }]
        }]
    })
}

#[tokio::test]
async fn parse_response_valid_json() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(minimal_valid_response()))
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/responses", mock_server.uri()))
        .send()
        .await
        .unwrap();

    let result = parse_response(response).await;
    assert!(result.is_ok());
    let turn_result = result.unwrap();
    assert_eq!(turn_result.items.len(), 1);
}

#[tokio::test]
async fn parse_response_invalid_json() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not valid json{broken"))
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/responses", mock_server.uri()))
        .send()
        .await
        .unwrap();

    let result = parse_response(response).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn parse_response_empty_body() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(""))
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/responses", mock_server.uri()))
        .send()
        .await
        .unwrap();

    let result = parse_response(response).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn parse_response_missing_output_field_fails() {
    let mock_server = MockServer::start().await;

    // Response without "output" field - should fail because output is required
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "resp-123",
            "status": "completed"
        })))
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/responses", mock_server.uri()))
        .send()
        .await
        .unwrap();

    let result = parse_response(response).await;
    // Should fail because "output" is a required field
    assert!(result.is_err());
}

#[tokio::test]
async fn parse_response_function_call_missing_required_fields_fails() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "resp-123",
            "output": [{
                "type": "function_call",
                "call_id": "call-1",
                "arguments": "{}"
            }]
        })))
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/responses", mock_server.uri()))
        .send()
        .await
        .unwrap();

    let error = parse_response(response).await.unwrap_err();
    let message = error.to_string();
    assert!(message.contains("malformed Responses API function_call"));
    assert!(message.contains("resp-123"));
    assert!(message.contains("id, name"));
}

#[tokio::test]
async fn parse_response_with_usage() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "output": [{
                "type": "message",
                "id": "msg-1",
                "status": "completed",
                "content": [{
                    "type": "output_text",
                    "text": "Hello!"
                }]
            }],
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "total_tokens": 150,
                "input_tokens_details": {
                    "cached_tokens": 20
                },
                "output_tokens_details": {
                    "reasoning_tokens": 10
                }
            }
        })))
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/responses", mock_server.uri()))
        .send()
        .await
        .unwrap();

    let result = parse_response(response).await;
    assert!(result.is_ok());
    let turn_result = result.unwrap();
    assert!(turn_result.usage.is_some());
    let usage = turn_result.usage.unwrap();
    assert_eq!(usage.input_tokens, 100);
    assert_eq!(usage.output_tokens, 50);
    assert_eq!(usage.total_tokens, 150);
    assert_eq!(usage.input_tokens_details.cached_tokens, 20);
    assert_eq!(usage.output_tokens_details.reasoning_tokens, 10);
}

#[tokio::test]
async fn parse_response_partial_usage() {
    let mock_server = MockServer::start().await;

    // Response with partial usage (some fields missing)
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "output": [{
                "type": "message",
                "id": "msg-1",
                "content": [{
                    "type": "output_text",
                    "text": "Hello!"
                }]
            }],
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50
                // total_tokens and details are missing
            }
        })))
        .mount(&mock_server)
        .await;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/responses", mock_server.uri()))
        .send()
        .await
        .unwrap();

    let result = parse_response(response).await;
    assert!(result.is_ok());
    let turn_result = result.unwrap();
    let usage = turn_result.usage.unwrap();
    // Should use defaults for missing fields
    assert_eq!(usage.input_tokens, 100);
    assert_eq!(usage.output_tokens, 50);
    assert_eq!(usage.total_tokens, 0); // Default
    assert_eq!(usage.input_tokens_details.cached_tokens, 0); // Default
    assert_eq!(usage.output_tokens_details.reasoning_tokens, 0); // Default
}

#[test]
fn to_api_input_user_message() {
    let item = ConversationItem::Message {
        role: Role::User,
        content: "Hello".to_string(),
        id: None,
        status: None,
        timestamp: None,
    };
    let json = to_api_input_json(&item);
    assert_eq!(json["type"], "message");
    assert_eq!(json["role"], "user");
    assert_eq!(json["content"][0]["type"], "input_text");
    assert_eq!(json["content"][0]["text"], "Hello");
}

#[test]
fn to_api_input_assistant_message_uses_output_text() {
    let item = ConversationItem::Message {
        role: Role::Assistant,
        content: "Hi".to_string(),
        id: Some("msg-1".to_string()),
        status: Some("completed".to_string()),
        timestamp: None,
    };
    let json = to_api_input_json(&item);
    assert_eq!(json["role"], "assistant");
    assert_eq!(json["content"][0]["type"], "output_text");
    assert_eq!(json["content"][0]["text"], "Hi");
    assert_eq!(json["id"], "msg-1");
    assert_eq!(json["status"], "completed");
}

#[test]
fn to_api_input_system_message() {
    let item = ConversationItem::Message {
        role: Role::System,
        content: "You are helpful".to_string(),
        id: None,
        status: None,
        timestamp: None,
    };
    let json = to_api_input_json(&item);
    assert_eq!(json["role"], "system");
    assert_eq!(json["content"][0]["type"], "input_text");
}

#[test]
fn to_api_input_tool_message() {
    let item = ConversationItem::Message {
        role: Role::Tool,
        content: "tool result".to_string(),
        id: None,
        status: None,
        timestamp: None,
    };
    let json = to_api_input_json(&item);
    assert_eq!(json["role"], "tool");
    assert_eq!(json["content"][0]["type"], "input_text");
}

#[test]
fn to_api_input_function_call() {
    let item = ConversationItem::FunctionCall {
        id: "fc-1".to_string(),
        call_id: "call-1".to_string(),
        name: "bash".to_string(),
        arguments: r#"{"cmd":"ls"}"#.to_string(),
        timestamp: None,
    };
    let json = to_api_input_json(&item);
    assert_eq!(json["type"], "function_call");
    assert_eq!(json["id"], "fc-1");
    assert_eq!(json["call_id"], "call-1");
    assert_eq!(json["name"], "bash");
    assert_eq!(json["arguments"], r#"{"cmd":"ls"}"#);
}

#[test]
fn to_api_input_function_call_output() {
    let item = ConversationItem::FunctionCallOutput {
        call_id: "call-1".to_string(),
        output: "file.txt".to_string(),
        timestamp: None,
    };
    let json = to_api_input_json(&item);
    assert_eq!(json["type"], "function_call_output");
    assert_eq!(json["call_id"], "call-1");
    assert_eq!(json["output"], "file.txt");
}

#[test]
fn to_api_input_reasoning() {
    let item = ConversationItem::Reasoning {
        id: "r-1".to_string(),
        summary: Some(vec!["thinking...".to_string()]),
        encrypted_content: None,
        content: None,
        timestamp: None,
    };
    let json = to_api_input_json(&item);
    assert_eq!(json["type"], "reasoning");
    assert_eq!(json["id"], "r-1");
    assert_eq!(json["summary"][0]["type"], "summary_text");
    assert_eq!(json["summary"][0]["text"], "thinking...");
}

#[test]
fn to_api_input_reasoning_multiple_summaries() {
    let item = ConversationItem::Reasoning {
        id: "r-2".to_string(),
        summary: Some(vec!["step 1".to_string(), "step 2".to_string()]),
        encrypted_content: None,
        content: None,
        timestamp: None,
    };
    let json = to_api_input_json(&item);
    assert_eq!(json["summary"].as_array().unwrap().len(), 2);
}

#[test]
fn to_api_input_reasoning_with_encrypted_content() {
    let item = ConversationItem::Reasoning {
        id: "r-1".to_string(),
        summary: Some(vec!["thinking...".to_string()]),
        encrypted_content: Some("gAAAAABencrypted...".to_string()),
        content: None,
        timestamp: None,
    };
    let json = to_api_input_json(&item);
    assert_eq!(json["type"], "reasoning");
    assert_eq!(json["encrypted_content"], "gAAAAABencrypted...");
}

#[test]
fn to_api_input_reasoning_without_encrypted_content_omits_field() {
    let item = ConversationItem::Reasoning {
        id: "r-1".to_string(),
        summary: Some(vec!["thinking...".to_string()]),
        encrypted_content: None,
        content: None,
        timestamp: None,
    };
    let json = to_api_input_json(&item);
    assert!(json.get("encrypted_content").is_none());
}

#[test]
fn to_api_input_reasoning_with_content() {
    let item = ConversationItem::Reasoning {
        id: "r-1".to_string(),
        summary: Some(vec!["thinking...".to_string()]),
        encrypted_content: None,
        timestamp: None,
        content: Some(vec![crate::types::ReasoningContent {
            content_type: ReasoningContentKind::ReasoningText,
            text: Some("deep thoughts".to_string()),
        }]),
    };
    let json = to_api_input_json(&item);
    assert_eq!(json["content"][0]["type"], "reasoning_text");
    assert_eq!(json["content"][0]["text"], "deep thoughts");
}

#[test]
fn to_api_input_reasoning_no_summary() {
    // When `summary` is `None`, the conversion produces an empty array
    // (`"summary": []`) rather than omitting the field. This is accepted
    // by the Responses API — see the comment in `From<&ConversationItem>`
    // for the rationale.
    let item = ConversationItem::Reasoning {
        id: "r-3".to_string(),
        summary: None,
        encrypted_content: None,
        content: None,
        timestamp: None,
    };
    let json = to_api_input_json(&item);
    assert_eq!(json["type"], "reasoning");
    assert_eq!(json["id"], "r-3");
    let summary_array = json["summary"].as_array().unwrap();
    assert!(
        summary_array.is_empty(),
        "expected empty summary array, got {summary_array:?}"
    );
}

#[test]
fn snapshot_user_message() {
    let item = ConversationItem::Message {
        role: Role::User,
        content: "Hello".to_string(),
        id: None,
        status: None,
        timestamp: None,
    };
    insta::assert_json_snapshot!("to_api_input_user_message", to_api_input_json(&item));
}

#[test]
fn snapshot_assistant_message_with_id_and_status() {
    let item = ConversationItem::Message {
        role: Role::Assistant,
        content: "Hi there".to_string(),
        id: Some("msg-1".to_string()),
        status: Some("completed".to_string()),
        timestamp: None,
    };
    insta::assert_json_snapshot!(
        "to_api_input_assistant_message_with_id_and_status",
        to_api_input_json(&item)
    );
}

#[test]
fn snapshot_system_message() {
    let item = ConversationItem::Message {
        role: Role::System,
        content: "You are cake".to_string(),
        id: None,
        status: None,
        timestamp: None,
    };
    insta::assert_json_snapshot!("to_api_input_system_message", to_api_input_json(&item));
}

#[test]
fn snapshot_function_call() {
    let item = ConversationItem::FunctionCall {
        id: "fc-1".to_string(),
        call_id: "call-1".to_string(),
        name: "bash".to_string(),
        arguments: r#"{"cmd":"ls"}"#.to_string(),
        timestamp: None,
    };
    insta::assert_json_snapshot!("to_api_input_function_call", to_api_input_json(&item));
}

#[test]
fn snapshot_function_call_output() {
    let item = ConversationItem::FunctionCallOutput {
        call_id: "call-1".to_string(),
        output: "file.txt\nother.txt".to_string(),
        timestamp: None,
    };
    insta::assert_json_snapshot!(
        "to_api_input_function_call_output",
        to_api_input_json(&item)
    );
}

#[test]
fn snapshot_reasoning_with_summary() {
    let item = ConversationItem::Reasoning {
        id: "r-1".to_string(),
        summary: Some(vec!["thinking...".to_string()]),
        encrypted_content: None,
        content: None,
        timestamp: None,
    };
    insta::assert_json_snapshot!(
        "to_api_input_reasoning_with_summary",
        to_api_input_json(&item)
    );
}

#[test]
fn snapshot_reasoning_with_encrypted_content() {
    let item = ConversationItem::Reasoning {
        id: "r-1".to_string(),
        summary: Some(vec!["thinking...".to_string()]),
        encrypted_content: Some("gAAAAABencrypted...".to_string()),
        content: None,
        timestamp: None,
    };
    insta::assert_json_snapshot!(
        "to_api_input_reasoning_with_encrypted_content",
        to_api_input_json(&item)
    );
}

#[test]
fn snapshot_reasoning_with_content_array() {
    let item = ConversationItem::Reasoning {
        id: "r-1".to_string(),
        summary: Some(vec!["thinking...".to_string()]),
        encrypted_content: None,
        content: Some(vec![crate::types::ReasoningContent {
            content_type: ReasoningContentKind::ReasoningText,
            text: Some("deep analysis".to_string()),
        }]),
        timestamp: None,
    };
    insta::assert_json_snapshot!(
        "to_api_input_reasoning_with_content_array",
        to_api_input_json(&item)
    );
}

#[test]
fn snapshot_reasoning_no_summary() {
    // When `summary` is `None`, the conversion produces `"summary": []`.
    // See the comment in `From<&ConversationItem>` for the rationale.
    let item = ConversationItem::Reasoning {
        id: "r-3".to_string(),
        summary: None,
        encrypted_content: None,
        content: None,
        timestamp: None,
    };
    insta::assert_json_snapshot!(
        "to_api_input_reasoning_no_summary",
        to_api_input_json(&item)
    );
}
