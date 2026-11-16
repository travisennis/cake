//! Provider-request growth invariant for one agent invocation.
//!
//! Provider prompt caches key on message-prefix identity, so within a single
//! `Agent::send` every provider request must repeat the previous request's
//! message sequence exactly and append only at the tail. A later request that
//! inserts, rewrites, or drops an earlier message invalidates the cache from
//! that point and multiplies the cost of every following turn.
//!
//! A committed context checkpoint is the one sanctioned exception. The request
//! immediately after it starts a new projection from the prompt prefix, the
//! marked summary, the retained tail, and current invocation context; every
//! later request in that same checkpoint epoch must extend that projection
//! exactly. Summary requests are lifecycle work, not part of either projection,
//! so the invariant compares only main-agent requests.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::*;
use crate::config::model::{ApiType, ResolvedModelConfig};
use crate::config::output_schema::OutputSchema;

/// One recorded provider request.
///
/// `summary` excludes lifecycle traffic from the main projection comparison.
/// `checkpoint_epoch` is the number of committed checkpoints when the request
/// was sent, so a new projection starts exactly at an epoch change.
#[derive(Clone)]
struct RecordedRequest {
    body: serde_json::Value,
    summary: bool,
    checkpoint_epoch: usize,
}

/// A scripted provider that records each request body before answering in
/// order. A request beyond the script panics so an unexpected retry cannot
/// silently weaken a test.
struct ScriptedProvider {
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    checkpoints: Arc<AtomicUsize>,
}

impl ScriptedProvider {
    fn new() -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            checkpoints: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// A handle that the persist callback increments after a checkpoint commit.
    fn checkpoint_counter(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.checkpoints)
    }

    async fn mount(&self, server: &MockServer, endpoint: &str, responses: Vec<ResponseTemplate>) {
        self.mount_split(server, endpoint, responses, Vec::new())
            .await;
    }

    /// Mount separate main-agent and summary response queues.
    ///
    /// The summary request is identified by its instruction and empty tool
    /// list, matching the production adapter's request shape. Keeping the
    /// queues separate lets a deterministic fixture drive both through the same
    /// local mock without guessing which response belongs to which projection.
    async fn mount_split(
        &self,
        server: &MockServer,
        endpoint: &str,
        main_responses: Vec<ResponseTemplate>,
        summary_responses: Vec<ResponseTemplate>,
    ) {
        let requests = Arc::clone(&self.requests);
        let checkpoints = Arc::clone(&self.checkpoints);
        let next_main = AtomicUsize::new(0);
        let next_summary = AtomicUsize::new(0);
        Mock::given(method("POST"))
            .and(path(endpoint))
            .respond_with(move |request: &Request| {
                let body = serde_json::from_slice::<serde_json::Value>(&request.body)
                    .expect("provider request body must be JSON");
                let summary = is_summary_request(&body);
                let checkpoint_epoch = checkpoints.load(Ordering::SeqCst);
                requests.lock().unwrap().push(RecordedRequest {
                    body,
                    summary,
                    checkpoint_epoch,
                });
                if summary {
                    let index = next_summary.fetch_add(1, Ordering::SeqCst);
                    return summary_responses.get(index).cloned().unwrap_or_else(|| {
                        panic!("provider received unexpected summary request {index}")
                    });
                }
                let index = next_main.fetch_add(1, Ordering::SeqCst);
                main_responses
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| panic!("provider received unexpected request {index}"))
            })
            .mount(server)
            .await;
    }

    /// The message sequence of every main-agent request, in request order.
    fn main_sequences(&self, key: &str) -> Vec<Vec<serde_json::Value>> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| !request.summary)
            .map(|request| message_sequence(&request.body, key))
            .collect()
    }

    fn recorded(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }
}

fn message_sequence(body: &serde_json::Value, key: &str) -> Vec<serde_json::Value> {
    body.get(key)
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("request body has no `{key}` array: {body}"))
        .clone()
}

/// Whether a request is the production summary adapter's bounded call.
fn is_summary_request(body: &serde_json::Value) -> bool {
    let instruction_present = body
        .get("instructions")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|text| text.contains("producing a checkpoint summary"))
        || body
            .get("messages")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|messages| {
                messages.iter().any(|message| {
                    message
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|text| text.contains("producing a checkpoint summary"))
                })
            });
    let tools_empty = body
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .is_none_or(Vec::is_empty);
    instruction_present && tools_empty
}

/// The first main-agent request that does not extend its predecessor within the
/// same checkpoint epoch, or `None` when every request only appends.
///
/// An equal pair is fine (a retry resends the same history). A changed epoch is
/// the sanctioned checkpoint boundary: the next request starts a new
/// projection, and later requests must extend that projection exactly.
fn tail_only_violation(requests: &[RecordedRequest], key: &str) -> Option<String> {
    let main: Vec<_> = requests.iter().filter(|request| !request.summary).collect();
    for (index, pair) in main.windows(2).enumerate() {
        let (previous, next) = (&pair[0], &pair[1]);
        if next.checkpoint_epoch != previous.checkpoint_epoch {
            continue;
        }
        let previous = message_sequence(&previous.body, key);
        let next = message_sequence(&next.body, key);
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

fn assert_tail_only_growth(requests: &[RecordedRequest], key: &str) {
    if let Some(violation) = tail_only_violation(requests, key) {
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
            top_k: None,
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

fn test_agent_with_window(api_type: ApiType, base_url: &str, context_window: u32) -> Agent {
    let mut config = test_resolved_model_config(api_type, base_url);
    config.model_config.context_window = Some(context_window);
    Agent::new(config, &[(Role::System, "test system prompt".to_string())])
        .with_session_id(uuid::uuid!("550e8400-e29b-41d4-a716-446655440000"))
        .with_task_id(uuid::uuid!("550e8400-e29b-41d4-a716-446655440001"))
}

fn compaction_policy() -> crate::config::settings::CompactionPolicy {
    crate::config::settings::CompactionPolicy {
        enabled: true,
        threshold_percent: 60,
        trigger_tokens: None,
        retained_tokens: 1_000,
        summary_tokens: 512,
    }
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

fn responses_function_calls(calls: &[(&str, &str, serde_json::Value)]) -> serde_json::Value {
    let output = calls
        .iter()
        .map(|(call_id, name, arguments)| {
            serde_json::json!({
                "type": "function_call",
                "id": format!("fc-{call_id}"),
                "call_id": call_id,
                "name": name,
                "arguments": arguments.to_string()
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "id": "resp-tools",
        "output": output,
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
    let first = request_with_input(&[serde_json::json!({ "role": "user", "content": "one" })]);
    let second = request_with_input(&[
        first_message(&first),
        serde_json::json!({ "role": "assistant", "content": "two" }),
    ]);

    assert_eq!(tail_only_violation(&[first, second], "input"), None);
}

#[test]
fn prefix_check_rejects_an_insertion_before_the_tail() {
    let first = request_with_input(&[serde_json::json!({ "role": "user", "content": "one" })]);
    let second = request_with_input(&[
        serde_json::json!({ "role": "developer", "content": "injected" }),
        first_message(&first),
    ]);

    let violation = tail_only_violation(&[first, second], "input")
        .expect("an insertion must fail the invariant");
    assert!(violation.contains("rewrote message 0"), "{violation}");
}

#[test]
fn prefix_check_rejects_truncation_and_rewrite() {
    let first = request_with_input(&[
        serde_json::json!({ "role": "user", "content": "one" }),
        serde_json::json!({ "role": "assistant", "content": "two" }),
    ]);
    let truncated = request_with_input(&[first_message(&first)]);
    let rewritten = request_with_input(&[
        first_message(&first),
        serde_json::json!({ "role": "assistant", "content": "rewritten" }),
    ]);

    assert!(tail_only_violation(&[first.clone(), truncated], "input").is_some());
    assert!(tail_only_violation(&[first, rewritten], "input").is_some());
}

#[test]
fn prefix_check_allows_one_new_projection_per_checkpoint() {
    let first = request_with_input(&[serde_json::json!({ "role": "user", "content": "one" })]);
    let before = request_with_input(&[
        first_message(&first),
        serde_json::json!({ "role": "assistant", "content": "two" }),
    ]);
    let mut after_checkpoint =
        request_with_input(&[serde_json::json!({ "role": "user", "content": "summary" })]);
    after_checkpoint.checkpoint_epoch = 1;
    let mut extended = request_with_input(&[
        first_message(&after_checkpoint),
        serde_json::json!({ "role": "assistant", "content": "three" }),
    ]);
    extended.checkpoint_epoch = 1;

    assert_eq!(
        tail_only_violation(&[first, before, after_checkpoint, extended], "input"),
        None
    );
}

fn request_with_input(input: &[serde_json::Value]) -> RecordedRequest {
    RecordedRequest {
        body: serde_json::json!({ "input": input.to_vec() }),
        summary: false,
        checkpoint_epoch: 0,
    }
}

fn first_message(request: &RecordedRequest) -> serde_json::Value {
    message_sequence(&request.body, "input")
        .into_iter()
        .next()
        .expect("request has a first message")
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
    let requests = provider.main_sequences("input");
    assert_eq!(requests.len(), 3);
    assert_tail_only_growth(&provider.recorded(), "input");
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
    let requests = provider.main_sequences("messages");
    assert_eq!(requests.len(), 2);
    assert_tail_only_growth(&provider.recorded(), "messages");
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
    let requests = provider.main_sequences("input");
    assert_eq!(requests.len(), 2);
    assert_tail_only_growth(&provider.recorded(), "input");
    let last = requests[1].last().expect("second request has input");
    assert!(
        last["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("failed output schema validation")),
        "the corrective prompt must be the new tail: {last}"
    );
}

/// A committed checkpoint starts one new projection; the rest of the
/// invocation must grow only at that projection's tail and must not replay
/// retired raw records or repeat a tool side effect.
#[tokio::test]
async fn compaction_continues_from_the_activated_checkpoint_projection() {
    let server = MockServer::start().await;
    let fixture_dir = tempfile::TempDir::new_in(std::env::current_dir().unwrap()).unwrap();
    let large_path = fixture_dir.path().join("pre-boundary.txt");
    let small_path = fixture_dir.path().join("post-boundary.txt");
    let marker_path = fixture_dir.path().join("side-effect.txt");
    std::fs::write(
        &large_path,
        format!(
            "PRE_BOUNDARY_SENTINEL\n{}\n",
            (0..100)
                .map(|_| "alpha ".repeat(80))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    )
    .unwrap();
    std::fs::write(&small_path, "POST_BOUNDARY_SENTINEL\n").unwrap();
    let user_input = "CURRENT_INVOCATION_SENTINEL: write the marker once";
    let write_content = "written exactly once\n";

    let provider = ScriptedProvider::new();
    provider
        .mount_split(
            &server,
            "/responses",
            vec![
                ResponseTemplate::new(200).set_body_json(responses_function_calls(&[
                    (
                        "call-read",
                        "Read",
                        serde_json::json!({ "path": large_path.display().to_string() }),
                    ),
                    (
                        "call-write",
                        "Write",
                        serde_json::json!({
                            "path": marker_path.display().to_string(),
                            "content": write_content
                        }),
                    ),
                ])),
                ResponseTemplate::new(200).set_body_json(responses_tool_call(
                    "call-after",
                    &small_path.display().to_string(),
                )),
                ResponseTemplate::new(200).set_body_json(responses_text("done")),
            ],
            vec![ResponseTemplate::new(200).set_body_json(responses_text(
                "Earlier work read the large fixture and wrote the marker once.",
            ))],
        )
        .await;

    let checkpoints = provider.checkpoint_counter();
    let persisted = Arc::new(Mutex::new(Vec::<SessionRecord>::new()));
    let sink = Arc::clone(&persisted);
    let mut agent = test_agent_with_window(ApiType::Responses, &server.uri(), 10_000)
        .with_compaction_policy(compaction_policy())
        .unwrap()
        .with_persist_callback(move |record| {
            if matches!(record, SessionRecord::Compaction(_)) {
                checkpoints.fetch_add(1, Ordering::SeqCst);
            }
            sink.lock().unwrap().push(record.clone());
            Ok(())
        })
        .with_tools(crate::clients::tools::read_write_tool_registry());

    let result = agent.send(user_input.to_string()).await.unwrap();

    assert_eq!(result, "done");
    assert_eq!(
        std::fs::read_to_string(&marker_path).unwrap(),
        write_content,
        "the Write side effect must run exactly once"
    );

    assert_continuation_requests(&provider.recorded(), user_input);
    assert_continuation_records(&persisted.lock().unwrap());
}

fn assert_continuation_requests(recorded: &[RecordedRequest], user_input: &str) {
    assert_eq!(recorded.len(), 4, "main, summary, main, main");
    assert_eq!(recorded.iter().filter(|request| request.summary).count(), 1);
    let main: Vec<_> = recorded.iter().filter(|request| !request.summary).collect();
    assert_eq!(main.len(), 3);
    assert_eq!(main[0].checkpoint_epoch, 0);
    assert_eq!(main[1].checkpoint_epoch, 1);
    assert_eq!(main[2].checkpoint_epoch, 1);
    assert_tail_only_growth(recorded, "input");

    let summary_request = recorded
        .iter()
        .find(|request| request.summary)
        .expect("summary request");
    let summary_body = summary_request.body.to_string();
    assert!(
        summary_body.contains("PRE_BOUNDARY_SENTINEL"),
        "the summary must receive the retired exchange: {summary_body}"
    );
    assert!(
        summary_request
            .body
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .is_none_or(Vec::is_empty),
        "a summary request never carries tools"
    );

    let post_checkpoint = main[1].body.to_string();
    assert!(
        !post_checkpoint.contains("PRE_BOUNDARY_SENTINEL"),
        "the post-checkpoint projection must omit retired raw records: {post_checkpoint}"
    );
    assert!(post_checkpoint.contains(crate::types::compaction::HISTORICAL_SUMMARY_MARKER));
    assert!(post_checkpoint.contains("Earlier work read the large fixture"));
    assert_eq!(
        post_checkpoint.matches(user_input).count(),
        1,
        "the current user input must appear exactly once: {post_checkpoint}"
    );
    assert!(main[2].body.to_string().contains("POST_BOUNDARY_SENTINEL"));
}

fn assert_continuation_records(records: &[SessionRecord]) {
    let lifecycle: Vec<_> = records
        .iter()
        .filter_map(|record| match record {
            SessionRecord::CompactionStart(_) => Some("start"),
            SessionRecord::Compaction(_) => Some("checkpoint"),
            SessionRecord::CompactionEnd(_) => Some("end"),
            _ => None,
        })
        .collect();
    assert_eq!(lifecycle, vec!["start", "checkpoint", "end"]);
    let outputs = |call_id: &str| {
        records
            .iter()
            .filter(|record| {
                matches!(
                    record,
                    SessionRecord::FunctionCallOutput(data) if data.call_id == call_id
                )
            })
            .count()
    };
    assert_eq!(outputs("call-read"), 1);
    assert_eq!(outputs("call-write"), 1);
    assert_eq!(outputs("call-after"), 1);
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
        let writer = SessionWriter::new(
            Session::open_for_append(&path).unwrap(),
            Arc::new(crate::config::BlobStore::new(dir.path().join("blobs"))),
        );
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

#[cfg(test)]
#[path = "tail_only_growth_target_tests.rs"]
mod target_tests;
