//! Provider image rejections degrade the turn, never the task.
//!
//! A provider that cannot read one inline image rejects the whole request, so
//! the agent must drop the images, tell the model, and retry the turn. A
//! rejection that never mentions an image must keep its pixels and its terminal
//! error.

use super::*;
use crate::config::model::ApiType;
use crate::types::ImagePart;
use std::sync::{Arc, Mutex};

use wiremock::matchers::{method, path};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

/// Chat Completions reply that ends the turn with a final assistant message.
fn final_chat_response() -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-123",
        "model": "test-model",
        "choices": [
            {
                "index": 0,
                "message": { "role": "assistant", "content": "Hello!" },
                "finish_reason": "stop"
            }
        ],
        "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
    })
}

/// The exact rejection shape a provider returns for an unreadable image.
fn image_rejection_response() -> serde_json::Value {
    serde_json::json!({
        "error": {
            "code": "invalid_request_error",
            "message": "Upstream request failed: [invalid_request_error] \
                        .messages[152].image[0]: You have uploaded an unsupported image. \
                        Please make sure your image is valid and has one of the following \
                        formats: webp, png, jpeg, and gif.",
            "param": "",
            "type": "invalid_request_error"
        }
    })
}

/// Matches a request whose message content carries an inline image part.
#[derive(Debug)]
struct RequestsImage;

impl Match for RequestsImage {
    fn matches(&self, request: &Request) -> bool {
        String::from_utf8_lossy(&request.body).contains("image_url")
    }
}

/// Matches a request with no inline image part.
#[derive(Debug)]
struct RequestsNoImage;

impl Match for RequestsNoImage {
    fn matches(&self, request: &Request) -> bool {
        !String::from_utf8_lossy(&request.body).contains("image_url")
    }
}

/// A conversation with one tool-result image, as a `ReadImage` call leaves it.
fn agent_with_tool_output_image(base_url: &str) -> Agent {
    let mut agent = test_agent_for(ApiType::ChatCompletions, base_url);
    agent
        .history_mut()
        .push(ConversationItem::FunctionCallOutput {
            call_id: "call-image".to_string(),
            output: "ReadImage /tmp/digest.jpg (1390799 bytes, image/jpeg)".to_string(),
            timestamp: None,
            images: vec![ImagePart {
                media_type: "image/jpeg".to_string(),
                data_base64: "AAAA".to_string(),
                content_id: None,
            }],
        });
    agent
}

/// Every inline image carried by the history.
fn history_images(agent: &Agent) -> Vec<ImagePart> {
    agent
        .history()
        .iter()
        .flat_map(|item| match item {
            ConversationItem::Message { images, .. }
            | ConversationItem::FunctionCallOutput { images, .. } => images.clone(),
            ConversationItem::FunctionCall { .. } | ConversationItem::Reasoning { .. } => {
                Vec::new()
            },
        })
        .collect()
}

#[tokio::test]
async fn image_rejection_drops_images_and_retries_the_turn() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(RequestsImage)
        .respond_with(ResponseTemplate::new(400).set_body_json(image_rejection_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(RequestsNoImage)
        .respond_with(ResponseTemplate::new(200).set_body_json(final_chat_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    let mut agent = agent_with_tool_output_image(&mock_server.uri());
    let answer = agent
        .send("what is in the image?".to_string())
        .await
        .expect("the task must survive a rejected image");

    assert_eq!(answer, "Hello!");
    assert!(
        history_images(&agent).is_empty(),
        "the rejected image must not be retried"
    );

    let note = agent
        .history()
        .iter()
        .find_map(|item| match item {
            ConversationItem::Message {
                role: Role::User,
                content,
                ..
            } if content.contains("unsupported image") => Some(content),
            _ => None,
        })
        .expect("the model must be told why its images disappeared");
    assert!(note.contains("cake removed 1 image(s)"), "note was: {note}");
}

#[tokio::test]
async fn unrelated_bad_request_keeps_images_and_fails_the_task() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": {
                "code": "invalid_request_error",
                "message": "max_tokens must be less than or equal to 4096",
                "type": "invalid_request_error"
            }
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let mut agent = agent_with_tool_output_image(&mock_server.uri());
    let error = agent
        .send("what is in the image?".to_string())
        .await
        .expect_err("an unrelated rejection stays terminal");

    assert_eq!(
        error
            .downcast_ref::<crate::exit_code::ApiError>()
            .map(|api_error| api_error.status),
        Some(400)
    );
    assert_eq!(
        history_images(&agent).len(),
        1,
        "an unrelated rejection must not cost the run its image"
    );
}

/// The recovery note is a normal conversation item, so the session records it
/// and a resumed run sees the same explanation the model saw.
#[tokio::test]
async fn image_recovery_note_is_persisted() {
    let persisted = Arc::new(Mutex::new(Vec::new()));
    let captures = Arc::clone(&persisted);
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(RequestsImage)
        .respond_with(ResponseTemplate::new(400).set_body_json(image_rejection_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(RequestsNoImage)
        .respond_with(ResponseTemplate::new(200).set_body_json(final_chat_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    let mut agent =
        agent_with_tool_output_image(&mock_server.uri()).with_persist_callback(move |record| {
            captures.lock().unwrap().push(record.clone());
            Ok(())
        });
    agent
        .send("what is in the image?".to_string())
        .await
        .expect("the task must survive a rejected image");

    let note = persisted
        .lock()
        .unwrap()
        .iter()
        .find_map(|record| match record {
            SessionRecord::Message(message) if message.content.contains("cake removed") => {
                Some(message.content.clone())
            },
            _ => None,
        })
        .expect("the recovery note must reach the session file");
    assert!(note.contains("1 image(s)"), "note was: {note}");
}
