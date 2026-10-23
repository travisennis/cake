//! Context-pressure classification and discard semantics (ahm task 034b).
//!
//! A length stop is never pressure by itself. Only independent evidence --- a
//! typed HTTP input overflow, or complete measured input above the configured
//! window --- can discard a response, and a discarded response is neither
//! persisted, streamed, nor executed while its attempt usage still settles.

use super::*;
use crate::config::OutputSchema;
use crate::config::model::ApiType;
use crate::types::SessionRecord;
use std::sync::{Arc, Mutex};

use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

const CONTEXT_WINDOW: u32 = 1000;
const OVER_WINDOW_INPUT: u64 = 5000;

fn responses_over_window_length_stop() -> serde_json::Value {
    serde_json::json!({
        "id": "resp-over-window",
        "status": "incomplete",
        "incomplete_details": {"reason": "max_output_tokens"},
        "output": [
            {
                "type": "reasoning",
                "id": "r-1",
                "summary": ["thinking..."]
            },
            {
                "type": "function_call",
                "id": "fc-1",
                "call_id": "call-1",
                "name": "Read",
                "arguments": "{\"path\":\"/tmp/x\"}"
            }
        ],
        "usage": {
            "input_tokens": OVER_WINDOW_INPUT,
            "output_tokens": 5,
            "total_tokens": OVER_WINDOW_INPUT + 5
        }
    })
}

fn over_window_input_without_termination() -> serde_json::Value {
    serde_json::json!({
        "id": "resp-over-window",
        "output": [
            {
                "type": "reasoning",
                "id": "r-1",
                "summary": ["thinking..."]
            }
        ],
        "usage": {
            "input_tokens": OVER_WINDOW_INPUT,
            "output_tokens": 5,
            "total_tokens": OVER_WINDOW_INPUT + 5
        }
    })
}

fn over_window_input_with_partial_usage() -> serde_json::Value {
    serde_json::json!({
        "id": "resp-over-window",
        "status": "incomplete",
        "incomplete_details": {"reason": "max_output_tokens"},
        "output": [
            {
                "type": "reasoning",
                "id": "r-1",
                "summary": ["thinking..."]
            }
        ],
        "usage": {
            "input_tokens": OVER_WINDOW_INPUT,
            "output_tokens": 5
        }
    })
}

fn over_window_length_stop_with_text() -> serde_json::Value {
    serde_json::json!({
        "id": "resp-cap",
        "status": "incomplete",
        "incomplete_details": {"reason": "max_output_tokens"},
        "output": [{
            "type": "message",
            "id": "msg-partial",
            "status": "incomplete",
            "content": [{"type": "output_text", "text": "partial answer"}]
        }],
        "usage": {
            "input_tokens": OVER_WINDOW_INPUT,
            "output_tokens": 1000,
            "total_tokens": OVER_WINDOW_INPUT + 1000
        }
    })
}

fn message_response(text: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "resp-final",
        "status": "completed",
        "output": [{
            "type": "message",
            "id": "msg-final",
            "status": "completed",
            "content": [{"type": "output_text", "text": text}]
        }],
        "usage": {
            "input_tokens": 10,
            "output_tokens": 5,
            "total_tokens": 15
        }
    })
}

fn over_window_message_response(text: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "resp-final",
        "status": "completed",
        "output": [{
            "type": "message",
            "id": "msg-final",
            "status": "completed",
            "content": [{"type": "output_text", "text": text}]
        }],
        "usage": {
            "input_tokens": OVER_WINDOW_INPUT,
            "output_tokens": 5,
            "total_tokens": OVER_WINDOW_INPUT + 5
        }
    })
}

type PersistedRecords = Arc<Mutex<Vec<SessionRecord>>>;
type StreamedRecords = Arc<Mutex<Vec<String>>>;

fn captured_agent(base_url: &str) -> (Agent, PersistedRecords, StreamedRecords) {
    let persisted = Arc::new(Mutex::new(Vec::new()));
    let streamed = Arc::new(Mutex::new(Vec::new()));
    let persisted_clone = Arc::clone(&persisted);
    let streamed_clone = Arc::clone(&streamed);
    let agent = test_agent_for(ApiType::Responses, base_url)
        .with_tools(crate::clients::tools::read_tool_registry())
        .with_context_window(Some(CONTEXT_WINDOW))
        .with_persist_callback(move |record| {
            persisted_clone.lock().unwrap().push(record.clone());
            Ok(())
        })
        .with_streaming_json(move |json| {
            streamed_clone.lock().unwrap().push(json.to_string());
        });
    (agent, persisted, streamed)
}

fn streamed_types(records: &StreamedRecords) -> Vec<String> {
    records
        .lock()
        .unwrap()
        .iter()
        .map(|json| {
            serde_json::from_str::<serde_json::Value>(json)
                .unwrap()
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

fn persisted_types(records: &Arc<Mutex<Vec<SessionRecord>>>) -> Vec<&'static str> {
    records
        .lock()
        .unwrap()
        .iter()
        .map(|record| match record {
            SessionRecord::Message(_) => "message",
            SessionRecord::FunctionCall(_) => "function_call",
            SessionRecord::FunctionCallOutput(_) => "function_call_output",
            SessionRecord::Reasoning(_) => "reasoning",
            SessionRecord::TurnUsage(_) => "turn_usage",
            _ => "other",
        })
        .collect()
}

#[tokio::test]
async fn pressure_discards_the_response_without_persisting_streaming_or_executing() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(responses_over_window_length_stop()))
        .expect(1)
        .mount(&mock_server)
        .await;

    let (mut agent, persisted, streamed) = captured_agent(&mock_server.uri());
    let error = agent.send("go".to_string()).await.unwrap_err();
    let cutoff = error
        .downcast_ref::<crate::types::CutOffError>()
        .expect("a discarded pressure response ends as a cut-off");

    assert_eq!(
        cutoff.detail,
        "The model's response was discarded because the reported input (5000 tokens) exceeded \
         the configured context window (1000 tokens). Raise or remove the model's \
         `context_window`, or start a new session; resuming this session re-sends the same \
         input. Session: 550e8400-e29b-41d4-a716-446655440000."
    );
    for claim in [
        "output limit",
        "token_limit",
        "max_output_tokens",
        "provider",
    ] {
        assert!(
            !cutoff.detail.contains(claim),
            "the message must stay neutral about the cause: {claim}"
        );
    }
    assert!(
        !cutoff.detail.contains("cake --resume"),
        "resuming cannot fix the over-window input: {}",
        cutoff.detail
    );

    // Exactly one provider attempt: the identical request is never retried.
    assert_eq!(mock_server.received_requests().await.unwrap().len(), 1);
    // Nothing from the discarded turn was processed, stored, streamed, or run.
    assert_eq!(agent.turn_count(), 0);
    assert_eq!(agent.tool_call_count, 0);
    assert_eq!(agent.history().len(), 2, "system and user items only");
    assert!(agent.history().iter().all(|item| matches!(
        item,
        ConversationItem::Message {
            role: Role::System | Role::User,
            ..
        }
    )));

    let records = persisted_types(&persisted);
    assert_eq!(
        records,
        vec!["message", "turn_usage"],
        "only the user message and the attempt usage persist"
    );
    let streamed = streamed_types(&streamed);
    assert!(
        streamed.iter().all(|record| !matches!(
            record.as_str(),
            "function_call" | "function_call_output" | "reasoning"
        )),
        "discarded items must not stream: {streamed:?}"
    );
    assert_eq!(
        streamed
            .iter()
            .filter(|record| record.as_str() == "turn_usage")
            .count(),
        1,
        "the discarded attempt still settles usage once: {streamed:?}"
    );

    // The attempt's reported usage still settles once.
    assert_eq!(agent.total_usage().input_tokens, OVER_WINDOW_INPUT);
    assert_eq!(agent.total_usage().output_tokens, 5);
    let usage = persisted
        .lock()
        .unwrap()
        .iter()
        .filter_map(|record| match record {
            SessionRecord::TurnUsage(data) => Some(data.usage),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].input_tokens, OVER_WINDOW_INPUT);
}

#[tokio::test]
async fn chat_length_stop_below_cap_over_the_window_is_pressure() {
    let mock_server = MockServer::start().await;
    let body = serde_json::json!({
        "id": "chatcmpl-over-window",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "partial answer"},
            "finish_reason": "length"
        }],
        "usage": {
            "prompt_tokens": OVER_WINDOW_INPUT,
            "completion_tokens": 5,
            "total_tokens": OVER_WINDOW_INPUT + 5
        }
    });
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(&mock_server)
        .await;

    let mut agent = test_agent_for(ApiType::ChatCompletions, &mock_server.uri())
        .with_context_window(Some(CONTEXT_WINDOW));
    let error = agent.send("go".to_string()).await.unwrap_err();

    assert!(error.downcast_ref::<crate::types::CutOffError>().is_some());
    assert_eq!(mock_server.received_requests().await.unwrap().len(), 1);
    assert_eq!(agent.turn_count(), 0);
    assert_eq!(agent.history().len(), 2);
}

#[tokio::test]
async fn no_termination_over_the_window_is_pressure() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(over_window_input_without_termination()),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let (mut agent, persisted, streamed) = captured_agent(&mock_server.uri());
    let error = agent.send("go".to_string()).await.unwrap_err();

    assert!(
        error.downcast_ref::<crate::types::CutOffError>().is_some(),
        "an unclassified stop over the window is discarded: {error:#}"
    );
    assert_eq!(mock_server.received_requests().await.unwrap().len(), 1);
    assert_eq!(agent.turn_count(), 0);
    assert_eq!(agent.history().len(), 2);
    assert_eq!(persisted_types(&persisted), vec!["message", "turn_usage"]);
    let streamed = streamed_types(&streamed);
    assert!(
        streamed.iter().all(|record| record.as_str() != "reasoning"),
        "discarded reasoning must not stream: {streamed:?}"
    );
    assert_eq!(
        streamed
            .iter()
            .filter(|record| record.as_str() == "turn_usage")
            .count(),
        1
    );
}

#[tokio::test]
async fn partial_usage_over_the_window_does_not_establish_pressure() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(over_window_input_with_partial_usage()),
        )
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(message_response("Hello!")))
        .mount(&mock_server)
        .await;

    let mut agent = test_agent_for(ApiType::Responses, &mock_server.uri())
        .with_context_window(Some(CONTEXT_WINDOW));
    let result = agent.send("go".to_string()).await.unwrap();

    assert_eq!(result, "Hello!");
    assert_eq!(agent.turn_count(), 2);
}

#[tokio::test]
async fn genuine_cap_stop_over_the_window_still_gets_semantic_recovery() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(over_window_length_stop_with_text()))
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(message_response("Hello!")))
        .mount(&mock_server)
        .await;

    let mut agent = test_agent_for(ApiType::Responses, &mock_server.uri())
        .with_context_window(Some(CONTEXT_WINDOW))
        .with_max_output_tokens(Some(1000));
    let result = agent.send("go".to_string()).await.unwrap();

    assert_eq!(result, "Hello!");
    // A genuine cap stop is not discarded: its partial text stays in history
    // and a bounded continuation follows.
    assert!(agent.history().iter().any(|item| matches!(
        item,
        ConversationItem::Message { role: Role::Assistant, content, .. }
            if content == "partial answer"
    )));
    assert_eq!(agent.turn_count(), 2);
    assert_eq!(mock_server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn ambiguous_below_cap_length_stop_within_the_window_is_not_pressure() {
    let mock_server = MockServer::start().await;
    let body = serde_json::json!({
        "id": "resp-early",
        "status": "incomplete",
        "incomplete_details": {"reason": "max_output_tokens"},
        "output": [{
            "type": "message",
            "id": "msg-partial",
            "status": "incomplete",
            "content": [{"type": "output_text", "text": "partial answer"}]
        }],
        "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}
    });
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(message_response("Hello!")))
        .mount(&mock_server)
        .await;

    let mut agent = test_agent_for(ApiType::Responses, &mock_server.uri())
        .with_context_window(Some(CONTEXT_WINDOW))
        .with_max_output_tokens(Some(1000));
    let result = agent.send("go".to_string()).await.unwrap();

    assert_eq!(result, "Hello!");
    assert_eq!(agent.turn_count(), 2);
}

#[tokio::test]
async fn completed_content_over_the_window_is_discarded() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(over_window_message_response("Hello!")),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let (mut agent, persisted, streamed) = captured_agent(&mock_server.uri());
    let error = agent.send("go".to_string()).await.unwrap_err();
    let cutoff = error
        .downcast_ref::<crate::types::CutOffError>()
        .expect("a completed response over the window is still discarded");

    assert!(
        cutoff.detail.contains("reported input (5000 tokens)"),
        "{}",
        cutoff.detail
    );
    // No message was persisted or streamed, no request was retried, and the
    // attempt's usage still settled once.
    assert_eq!(mock_server.received_requests().await.unwrap().len(), 1);
    assert_eq!(agent.turn_count(), 0);
    assert_eq!(agent.history().len(), 2);
    assert_eq!(persisted_types(&persisted), vec!["message", "turn_usage"]);
    assert!(
        streamed
            .lock()
            .unwrap()
            .iter()
            .all(|record| !record.contains("Hello!"))
    );
    assert_eq!(agent.total_usage().input_tokens, OVER_WINDOW_INPUT);
}

#[tokio::test]
async fn completed_answer_within_the_window_is_returned() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(message_response("Hello!")))
        .expect(1)
        .mount(&mock_server)
        .await;

    let mut agent = test_agent_for(ApiType::Responses, &mock_server.uri())
        .with_context_window(Some(CONTEXT_WINDOW));
    let result = agent.send("go".to_string()).await.unwrap();

    assert_eq!(result, "Hello!");
    assert_eq!(agent.turn_count(), 1);
}

#[tokio::test]
async fn tool_call_over_the_window_executes_no_tool() {
    let mock_server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("observed.txt");
    std::fs::write(&file_path, "workspace contents\n").unwrap();
    let read_arguments = serde_json::json!({ "path": file_path }).to_string();
    let tool_call = serde_json::json!({
        "id": "resp-tool",
        "status": "completed",
        "output": [{
            "type": "function_call",
            "id": "fc-1",
            "call_id": "call-1",
            "name": "Read",
            "arguments": read_arguments
        }],
        "usage": {
            "input_tokens": OVER_WINDOW_INPUT,
            "output_tokens": 5,
            "total_tokens": OVER_WINDOW_INPUT + 5
        }
    });

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tool_call))
        .expect(1)
        .mount(&mock_server)
        .await;

    let (mut agent, persisted, _streamed) = captured_agent(&mock_server.uri());
    let error = agent.send("go".to_string()).await.unwrap_err();

    assert!(
        error.downcast_ref::<crate::types::CutOffError>().is_some(),
        "a tool-call turn over the window is discarded: {error:#}"
    );
    assert_eq!(mock_server.received_requests().await.unwrap().len(), 1);
    assert_eq!(agent.tool_call_count, 0);
    assert_eq!(agent.turn_count(), 0);
    // Neither the call nor any output reached the transcript or the session.
    assert_eq!(agent.history().len(), 2);
    assert_eq!(persisted_types(&persisted), vec!["message", "turn_usage"]);
}

#[tokio::test]
async fn typed_http_input_overflow_is_terminal_and_carries_the_verdict() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": {
                "message": "input length and max_tokens exceed context limit: 16380 + 5000 > 16384",
                "type": "invalid_request_error"
            }
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let mut agent = test_agent_for(ApiType::Responses, &mock_server.uri());
    agent.history_mut().push(ConversationItem::Message {
        role: Role::User,
        content: "test".to_string(),
        id: None,
        status: None,
        timestamp: None,
        images: Vec::new(),
    });

    let error = agent.complete_turn(false).await.unwrap_err();
    let api_error = error
        .downcast_ref::<crate::exit_code::ApiError>()
        .expect("the terminal failure stays an ApiError");

    assert_eq!(api_error.status, 400);
    let overflow = api_error
        .input_overflow
        .expect("genuine input overflow is carried as a typed verdict");
    assert_eq!(overflow.input_tokens, 16380);
    assert_eq!(overflow.context_limit, 16384);
    assert!(!overflow.fits_with_reduced_output());
    // No allowance reduction was possible, so the request was not retried.
    assert_eq!(mock_server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn the_intended_cap_is_the_value_before_context_clamping() {
    let mock_server = MockServer::start().await;
    // The initial request overflows and its input fits, so the retry path
    // reduces the output allowance from the configured 5000 to 3360.
    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(body_partial_json(serde_json::json!({
            "max_output_tokens": 5000
        })))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": {
                "message": "input length and max_tokens exceed context limit: 12000 + 5000 > 16384",
                "type": "invalid_request_error"
            }
        })))
        .expect(1)
        .mount(&mock_server)
        .await;
    // The retry carries the clamped allowance and stops at exactly that many
    // reported output tokens.
    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(body_partial_json(serde_json::json!({
            "max_output_tokens": 3360
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "resp-clamped",
            "status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"},
            "output": [{
                "type": "message",
                "id": "msg-partial",
                "status": "incomplete",
                "content": [{"type": "output_text", "text": "partial answer"}]
            }],
            "usage": {
                "input_tokens": 12000,
                "output_tokens": 3360,
                "total_tokens": 15360
            }
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    // The window is known and the measured input is above it, so an ambiguous
    // stop discards the response. If the clamped 3360 were used as the intended
    // cap instead of the configured 5000, the stop would read as a genuine cap
    // stop, stay non-pressure, and continue with a semantic recovery.
    let mut agent = test_agent_for(ApiType::Responses, &mock_server.uri())
        .with_max_output_tokens(Some(5000))
        .with_context_window(Some(CONTEXT_WINDOW));
    let error = agent.send("go".to_string()).await.unwrap_err();

    assert!(
        error.downcast_ref::<crate::types::CutOffError>().is_some(),
        "a stop at the clamped allowance is ambiguous, not a genuine cap stop: {error:#}"
    );
    assert_eq!(mock_server.received_requests().await.unwrap().len(), 2);
    assert_eq!(agent.turn_count(), 0);
}

#[tokio::test]
async fn output_allowance_overflow_retry_does_not_report_input_overflow() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": {
                "message": "input length and max_tokens exceed context limit: 12000 + 5000 > 16384",
                "type": "invalid_request_error"
            }
        })))
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": {
                "message": "input length and max_tokens exceed context limit: 12000 + 3360 > 16384",
                "type": "invalid_request_error"
            }
        })))
        .mount(&mock_server)
        .await;

    let mut agent =
        test_agent_for(ApiType::Responses, &mock_server.uri()).with_max_output_tokens(Some(5000));
    agent.history_mut().push(ConversationItem::Message {
        role: Role::User,
        content: "test".to_string(),
        id: None,
        status: None,
        timestamp: None,
        images: Vec::new(),
    });

    let error = agent.complete_turn(false).await.unwrap_err();
    let api_error = error
        .downcast_ref::<crate::exit_code::ApiError>()
        .expect("the terminal failure stays an ApiError");

    assert_eq!(
        api_error.input_overflow, None,
        "an input that fits is an output-allowance mismatch, not input overflow"
    );
    assert_eq!(mock_server.received_requests().await.unwrap().len(), 2);
}

/// Matches an unconstrained correction retry: the corrective message is in the
/// input, no tools are offered, and no native schema constraint is attached.
#[derive(Debug)]
struct UnconstrainedCorrectionRequest;

impl Match for UnconstrainedCorrectionRequest {
    fn matches(&self, request: &Request) -> bool {
        let Ok(body) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
            return false;
        };
        let has_corrective = body["input"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item["content"][0]["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("failed output schema validation"))
            })
        });
        has_corrective && body.get("tools").is_none() && body.get("text").is_none()
    }
}

/// Matches a Responses correction request: the corrective message is in the
/// input, no tools are offered, and the native schema constraint is attached.
#[derive(Debug)]
struct ConstrainedCorrectionRequest;

impl Match for ConstrainedCorrectionRequest {
    fn matches(&self, request: &Request) -> bool {
        let Ok(body) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
            return false;
        };
        let has_corrective = body["input"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item["content"][0]["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("failed output schema validation"))
            })
        });
        has_corrective
            && body.get("tools").is_none()
            && body["text"]["format"]["type"] == "json_schema"
    }
}

fn test_schema() -> Arc<OutputSchema> {
    let raw = serde_json::json!({
        "type": "object",
        "properties": {"summary": {"type": "string"}},
        "required": ["summary"],
        "additionalProperties": false
    });
    let validator = jsonschema::draft202012::new(&raw).unwrap();
    Arc::new(OutputSchema {
        name: "final_output".to_string(),
        raw,
        validator,
    })
}

#[tokio::test]
async fn typed_input_overflow_on_a_constraint_turn_is_not_a_constraint_rejection() {
    let mock_server = MockServer::start().await;
    // An unconstrained retry would match this mock; the typed overflow must end
    // the run instead of buying another request.
    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(UnconstrainedCorrectionRequest)
        .respond_with(ResponseTemplate::new(200).set_body_json(over_window_length_stop_with_text()))
        .expect(0)
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "resp-1",
            "output": [{
                "type": "message",
                "id": "msg-1",
                "status": "completed",
                "content": [{"type": "output_text", "text": "not json"}]
            }],
            "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}
        })))
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(ConstrainedCorrectionRequest)
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": {
                "message": "input length and max_tokens exceed context limit: 16380 + 5000 > 16384",
                "type": "invalid_request_error"
            }
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let mut agent =
        test_agent_for(ApiType::Responses, &mock_server.uri()).with_output_schema(test_schema());
    let error = agent.send("go".to_string()).await.unwrap_err();
    let api_error = error
        .downcast_ref::<crate::exit_code::ApiError>()
        .expect("the typed overflow stays the terminal provider error");

    assert_eq!(api_error.status, 400);
    assert!(
        api_error
            .input_overflow
            .is_some_and(|overflow| !overflow.fits_with_reduced_output()),
        "the genuine input overflow keeps its typed verdict"
    );
    assert_eq!(
        mock_server.received_requests().await.unwrap().len(),
        2,
        "the constrained correction request is not retried unconstrained"
    );
}

#[tokio::test]
async fn pressure_during_a_correction_turn_keeps_correction_mode_intact() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "resp-1",
            "output": [{
                "type": "message",
                "id": "msg-1",
                "status": "completed",
                "content": [{"type": "output_text", "text": "not json"}]
            }],
            "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}
        })))
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(ConstrainedCorrectionRequest)
        .respond_with(ResponseTemplate::new(200).set_body_json(over_window_length_stop_with_text()))
        .expect(1)
        .mount(&mock_server)
        .await;

    let (agent, persisted, _streamed) = captured_agent(&mock_server.uri());
    let mut agent = agent.with_output_schema(test_schema());
    let error = agent.send("go".to_string()).await.unwrap_err();

    assert!(
        error.downcast_ref::<crate::types::CutOffError>().is_some(),
        "a pressure verdict ends as a cut-off, not schema exhaustion: {error:#}"
    );
    // The correction turn ran tools-disabled with its schema constraint, and
    // its response was discarded without incrementing the correction count.
    assert_eq!(agent.turn_count(), 1);
    assert_eq!(mock_server.received_requests().await.unwrap().len(), 2);
    assert!(persisted_types(&persisted).contains(&"turn_usage"));
}
