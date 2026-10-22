//! Provider-request growth invariant for one agent invocation.
//!
//! Provider prompt caches key on message-prefix identity, so within a single
//! `Agent::send` every provider request must repeat the previous request's
//! message sequence exactly and append only at the tail. A later request that
//! inserts, rewrites, or drops an earlier message invalidates the cache from
//! that point and multiplies the cost of every following turn.
//!
//! Each test drives a scripted provider that records every request body, then
//! asserts the recorded message sequences are exact prefix-extensions. Cake has
//! no sanctioned exceptions: when #343 lands, compaction becomes the one
//! deliberate boundary and [`tail_only_violation`] gains a single skip point.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::*;
use crate::config::model::{ApiType, ResolvedModelConfig};
use crate::config::output_schema::OutputSchema;

/// A scripted provider that records each request body before answering in
/// order. A request beyond the script panics so an unexpected retry cannot
/// silently weaken a test.
struct ScriptedProvider {
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl ScriptedProvider {
    fn new() -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    async fn mount(&self, server: &MockServer, endpoint: &str, responses: Vec<ResponseTemplate>) {
        let requests = Arc::clone(&self.requests);
        let next = AtomicUsize::new(0);
        Mock::given(method("POST"))
            .and(path(endpoint))
            .respond_with(move |request: &Request| {
                let body = serde_json::from_slice::<serde_json::Value>(&request.body)
                    .expect("provider request body must be JSON");
                requests.lock().unwrap().push(body);
                let index = next.fetch_add(1, Ordering::SeqCst);
                responses
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| panic!("provider received unexpected request {index}"))
            })
            .mount(server)
            .await;
    }

    /// The message sequence of every recorded request, in request order.
    fn sequences(&self, key: &str) -> Vec<Vec<serde_json::Value>> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|body| {
                body.get(key)
                    .and_then(serde_json::Value::as_array)
                    .unwrap_or_else(|| panic!("request body has no `{key}` array: {body}"))
                    .clone()
            })
            .collect()
    }
}

/// The first provider request that does not extend its predecessor, or `None`
/// when every request only appends.
///
/// This is deliberately strict: an equal pair is fine (a retry resends the same
/// history), but any shorter sequence or any changed earlier message is a
/// violation. Cake has no sanctioned compaction boundary yet; #343 adds the
/// single skip point here when it lands.
fn tail_only_violation(requests: &[Vec<serde_json::Value>]) -> Option<String> {
    for (index, pair) in requests.windows(2).enumerate() {
        let (previous, next) = (&pair[0], &pair[1]);
        let request = index + 1;
        if next.len() < previous.len() {
            return Some(format!(
                "provider request {request} shrank its message sequence from {} to {} items; \
                 requests must only append at the tail",
                previous.len(),
                next.len()
            ));
        }
        if let Some(position) =
            (0..previous.len()).find(|&position| next[position] != previous[position])
        {
            return Some(format!(
                "provider request {request} rewrote message {position}; requests must only \
                 append at the tail\n  previous: {}\n  next:     {}",
                previous[position], next[position]
            ));
        }
    }
    None
}

fn assert_tail_only_growth(requests: &[Vec<serde_json::Value>]) {
    if let Some(violation) = tail_only_violation(requests) {
        panic!("{violation}");
    }
}

fn test_resolved_model_config(api_type: ApiType, base_url: &str) -> ResolvedModelConfig {
    ResolvedModelConfig {
        model_config: crate::config::model::ModelConfig {
            supports_images: true,
            model: "test-model".to_string(),
            api_type,
            base_url: base_url.to_string(),
            api_key_env: "TEST_API_KEY".to_string(),
            provider: None,
            provider_headers: None,
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            context_window: None,
            reasoning_effort: None,
            reasoning_summary: None,
            reasoning_max_tokens: None,
            providers: vec![],
        },
        api_key: "test-key".to_string(),
    }
}

fn test_agent(api_type: ApiType, base_url: &str) -> Agent {
    Agent::new(
        test_resolved_model_config(api_type, base_url),
        &[(Role::System, "test system prompt".to_string())],
    )
    .with_session_id(uuid::uuid!("550e8400-e29b-41d4-a716-446655440000"))
    .with_task_id(uuid::uuid!("550e8400-e29b-41d4-a716-446655440001"))
}

fn responses_tool_call(call_id: &str, read_path: &str) -> serde_json::Value {
    serde_json::json!({
        "id": format!("resp-{call_id}"),
        "output": [{
            "type": "function_call",
            "id": format!("fc-{call_id}"),
            "call_id": call_id,
            "name": "Read",
            "arguments": serde_json::json!({ "path": read_path }).to_string()
        }],
        "usage": { "input_tokens": 1, "output_tokens": 1, "total_tokens": 2 }
    })
}

fn responses_text(text: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "resp-text",
        "output": [{
            "type": "message",
            "id": "msg-text",
            "status": "completed",
            "content": [{ "type": "output_text", "text": text }]
        }],
        "usage": { "input_tokens": 1, "output_tokens": 1, "total_tokens": 2 }
    })
}

/// A truncated Chat Completions turn with no content: the loop must append a
/// continuation prompt before asking again.
fn chat_truncated() -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-truncated",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": null },
            "finish_reason": "length"
        }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
    })
}

fn chat_text(text: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-text",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": text },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
    })
}

fn test_schema() -> Arc<OutputSchema> {
    let raw = serde_json::json!({
        "type": "object",
        "properties": { "summary": { "type": "string" } },
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

#[test]
fn prefix_check_accepts_tail_growth() {
    let first = vec![serde_json::json!({ "role": "user", "content": "one" })];
    let second = vec![
        first[0].clone(),
        serde_json::json!({ "role": "assistant", "content": "two" }),
    ];

    assert_eq!(tail_only_violation(&[first, second]), None);
}

#[test]
fn prefix_check_rejects_an_insertion_before_the_tail() {
    let first = vec![serde_json::json!({ "role": "user", "content": "one" })];
    let second = vec![
        serde_json::json!({ "role": "developer", "content": "injected" }),
        first[0].clone(),
    ];

    let violation =
        tail_only_violation(&[first, second]).expect("an insertion must fail the invariant");
    assert!(violation.contains("rewrote message 0"), "{violation}");
}

#[test]
fn prefix_check_rejects_truncation_and_rewrite() {
    let first = vec![
        serde_json::json!({ "role": "user", "content": "one" }),
        serde_json::json!({ "role": "assistant", "content": "two" }),
    ];
    let truncated = vec![first[0].clone()];
    let rewritten = vec![
        first[0].clone(),
        serde_json::json!({ "role": "assistant", "content": "rewritten" }),
    ];

    assert!(tail_only_violation(&[first.clone(), truncated]).is_some());
    assert!(tail_only_violation(&[first, rewritten]).is_some());
}

/// Ordinary tool turns: each request carries the previous turn's call and
/// output unchanged and appends the next turn's items.
#[tokio::test]
async fn responses_tool_turns_grow_only_at_the_tail() {
    let server = MockServer::start().await;
    let fixture_dir = tempfile::TempDir::new_in(std::env::current_dir().unwrap()).unwrap();
    let fixture_path = fixture_dir.path().join("tail-only-input.txt");
    std::fs::write(&fixture_path, "alpha\n").unwrap();
    let read_path = fixture_path.display().to_string();

    let provider = ScriptedProvider::new();
    provider
        .mount(
            &server,
            "/responses",
            vec![
                ResponseTemplate::new(200).set_body_json(responses_tool_call("call-1", &read_path)),
                ResponseTemplate::new(200).set_body_json(responses_tool_call("call-2", &read_path)),
                ResponseTemplate::new(200).set_body_json(responses_text("done")),
            ],
        )
        .await;

    let mut agent = test_agent(ApiType::Responses, &server.uri())
        .with_tools(crate::clients::tools::read_tool_registry());
    let result = agent.send("read the fixture".to_string()).await.unwrap();

    assert_eq!(result, "done");
    let requests = provider.sequences("input");
    assert_eq!(requests.len(), 3);
    assert_tail_only_growth(&requests);
}

/// Semantic recovery appends its continuation prompt after the truncated turn;
/// it must never splice it in before the previous tail. This also covers the
/// Chat Completions message shape.
#[tokio::test]
async fn chat_completions_semantic_recovery_appends_at_the_tail() {
    let server = MockServer::start().await;
    let provider = ScriptedProvider::new();
    provider
        .mount(
            &server,
            "/chat/completions",
            vec![
                ResponseTemplate::new(200).set_body_json(chat_truncated()),
                ResponseTemplate::new(200).set_body_json(chat_text("done")),
            ],
        )
        .await;

    let mut agent = test_agent(ApiType::ChatCompletions, &server.uri());
    let result = agent.send("answer".to_string()).await.unwrap();

    assert_eq!(result, "done");
    let requests = provider.sequences("messages");
    assert_eq!(requests.len(), 2);
    assert_tail_only_growth(&requests);
    let last = requests[1].last().expect("second request has messages");
    assert_eq!(
        last["content"].as_str(),
        Some(crate::clients::agent::agent_loop::SEMANTIC_RECOVERY_PROMPT),
        "the continuation prompt must be the new tail"
    );
}

/// Schema correction is another synthetic user turn; it too must append.
#[tokio::test]
async fn schema_correction_appends_at_the_tail() {
    let server = MockServer::start().await;
    let provider = ScriptedProvider::new();
    provider
        .mount(
            &server,
            "/responses",
            vec![
                ResponseTemplate::new(200).set_body_json(responses_text("not json")),
                ResponseTemplate::new(200).set_body_json(responses_text("{\"summary\":\"ok\"}")),
            ],
        )
        .await;

    let mut agent = test_agent(ApiType::Responses, &server.uri()).with_output_schema(test_schema());
    let result = agent.send("answer".to_string()).await.unwrap();

    assert_eq!(result, "{\"summary\":\"ok\"}");
    let requests = provider.sequences("input");
    assert_eq!(requests.len(), 2);
    assert_tail_only_growth(&requests);
    let last = requests[1].last().expect("second request has input");
    assert!(
        last["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("failed output schema validation")),
        "the corrective prompt must be the new tail: {last}"
    );
}

/// A checkpoint commit failure reported through the production persist
/// callback ends the invocation before the next provider request.
///
/// The agent's session callback appends a `FunctionCall` record while the
/// scripted turn's tool is still pending. Committing a checkpoint at that point
/// fails at the injected stage, and the loop must surface the commit error
/// without appending or executing the tool output. A flush failure leaves no
/// record; a sync failure leaves the already-flushed, complete checkpoint.
#[tokio::test]
async fn failed_checkpoint_commit_through_the_persist_callback_stops_the_loop() {
    use crate::config::session::{Session, SessionWriter, WriterFault};
    use crate::types::compaction::test_support;

    let session_id = "550e8400-e29b-41d4-a716-446655440000";
    for stage in [WriterFault::Flush, WriterFault::Sync] {
        let server = MockServer::start().await;
        let fixture_dir = tempfile::TempDir::new_in(std::env::current_dir().unwrap()).unwrap();
        let fixture_path = fixture_dir.path().join("checkpoint-failure-input.txt");
        std::fs::write(&fixture_path, "alpha\n").unwrap();
        let read_path = fixture_path.display().to_string();

        // Only one response is scripted; the provider panics on a second request.
        let provider = ScriptedProvider::new();
        provider
            .mount(
                &server,
                "/responses",
                vec![
                    ResponseTemplate::new(200)
                        .set_body_json(responses_tool_call("call-1", &read_path)),
                ],
            )
            .await;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(
            &path,
            format!(
                r#"{{"type":"session_meta","format_version":4,"session_id":"{session_id}","timestamp":"2026-01-01T00:00:00Z","working_directory":"/work","tools":[]}}"#
            ) + "\n",
        )
        .unwrap();
        let writer = SessionWriter::new(Session::open_for_append(&path).unwrap());
        let mut checkpoint = test_support::checkpoint("checkpoint-1", "retired work", Vec::new());
        checkpoint.session_id = session_id.to_string();
        writer.fail_next_checkpoint(stage);

        let for_agent = writer.clone();
        let mut agent = test_agent(ApiType::Responses, &server.uri())
            .with_persist_callback(move |record| {
                if matches!(record, SessionRecord::FunctionCall(_)) {
                    for_agent.commit_checkpoint(&checkpoint)?;
                }
                for_agent.append_record(record)
            })
            .with_tools(crate::clients::tools::read_tool_registry());

        let error = agent
            .send("read the fixture".to_string())
            .await
            .expect_err("a failed checkpoint commit must abort the invocation");
        assert!(
            format!("{error:#}").contains(&format!("injected {stage:?} failure")),
            "{stage:?}: the original commit error must surface: {error:#}"
        );
        assert_eq!(agent.turn_count(), 1, "{stage:?}");
        assert_eq!(
            provider.requests.lock().unwrap().len(),
            1,
            "{stage:?}: a failed commit must not start another provider request"
        );
        assert!(
            !agent
                .history()
                .iter()
                .any(|item| matches!(item, ConversationItem::FunctionCallOutput { .. })),
            "{stage:?}: the pending tool must not run after the commit failure"
        );

        drop(agent);
        drop(writer);
        let loaded = Session::load(&path).expect("the session must stay loadable");
        match stage {
            WriterFault::Flush => {
                assert!(
                    !loaded
                        .records
                        .iter()
                        .any(|record| matches!(record, SessionRecord::Compaction(_))),
                    "the failed checkpoint must not survive"
                );
            },
            WriterFault::Sync => {
                let checkpoints: Vec<_> = loaded
                    .records
                    .iter()
                    .filter_map(|record| match record {
                        SessionRecord::Compaction(data) => Some(data),
                        _ => None,
                    })
                    .collect();
                assert_eq!(checkpoints.len(), 1, "the flushed checkpoint is complete");
                assert_eq!(checkpoints[0].checkpoint_id, "checkpoint-1");
            },
            WriterFault::Serialize => unreachable!("only write stages are exercised"),
        }
    }
}

/// A failed session append ends the invocation before the next provider
/// request.
///
/// A checkpoint commit reports failure through this same persist callback, so
/// no pending tool may run and no follow-up request may start after it.
#[tokio::test]
async fn failed_append_stops_before_the_next_provider_request() {
    let server = MockServer::start().await;
    let fixture_dir = tempfile::TempDir::new_in(std::env::current_dir().unwrap()).unwrap();
    let fixture_path = fixture_dir.path().join("append-failure-input.txt");
    std::fs::write(&fixture_path, "alpha\n").unwrap();
    let read_path = fixture_path.display().to_string();

    // Only one response is scripted; the provider panics on a second request.
    let provider = ScriptedProvider::new();
    provider
        .mount(
            &server,
            "/responses",
            vec![
                ResponseTemplate::new(200).set_body_json(responses_tool_call("call-1", &read_path)),
            ],
        )
        .await;

    let appends = Arc::new(Mutex::new(0usize));
    let appends_clone = Arc::clone(&appends);
    let mut agent = test_agent(ApiType::Responses, &server.uri())
        .with_persist_callback(move |record| {
            *appends_clone.lock().unwrap() += 1;
            if matches!(record, SessionRecord::FunctionCall(_)) {
                anyhow::bail!("forced session append failure");
            }
            Ok(())
        })
        .with_tools(crate::clients::tools::read_tool_registry());

    let error = agent
        .send("read the fixture".to_string())
        .await
        .expect_err("a failed append must abort the invocation");

    assert!(
        format!("{error:#}").contains("forced session append failure"),
        "unexpected error: {error:#}"
    );
    assert_eq!(agent.turn_count(), 1);
    assert_eq!(
        provider.requests.lock().unwrap().len(),
        1,
        "a failed append must not start another provider request"
    );
    assert!(*appends.lock().unwrap() > 0);
    assert!(
        !agent
            .history()
            .iter()
            .any(|item| matches!(item, ConversationItem::FunctionCallOutput { .. })),
        "the pending tool must not run after the append failure"
    );
}
