use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::clients::agent_runner::AgentRunner;
use crate::clients::backend::Backend;
use crate::clients::compaction::MeasuredInput;
use crate::config::OutputSchema;
use crate::config::model::{ApiType, ModelConfig, ReasoningEffort, ResolvedModelConfig};
use crate::config::settings::CompactionPolicy;
use crate::types::compaction::HISTORICAL_SUMMARY_MARKER;
use crate::types::session::MessageData;
use crate::types::{ConversationItem, Role, SessionRecord, Usage, UsagePresence};

const SESSION_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
const TASK_ID: &str = "550e8400-e29b-41d4-a716-446655440001";

fn test_config(base_url: &str) -> ResolvedModelConfig {
    ResolvedModelConfig {
        model_config: ModelConfig {
            supports_images: false,
            model: "test-model".to_string(),
            api_type: ApiType::ChatCompletions,
            base_url: base_url.to_string(),
            api_key_env: "TEST_API_KEY".to_string(),
            provider: None,
            provider_headers: None,
            temperature: None,
            top_p: None,
            top_k: None,
            max_output_tokens: Some(16_384),
            context_window: Some(500_000),
            reasoning_effort: None,
            reasoning_summary: None,
            reasoning_max_tokens: None,
            providers: vec![],
        },
        api_key: "test-key".to_string(),
    }
}

fn test_agent(base_url: &str) -> Agent {
    test_agent_with_system(base_url, "test system prompt")
}

fn test_agent_with_system(base_url: &str, system: &str) -> Agent {
    Agent::new(test_config(base_url), &[(Role::System, system.to_string())])
        .with_session_id(uuid::uuid!(SESSION_ID))
        .with_task_id(uuid::uuid!(TASK_ID))
        .with_tools(crate::clients::tools::ToolRegistry::empty())
}

fn test_agent_with_window(base_url: &str, context_window: u32) -> Agent {
    let mut config = test_config(base_url);
    config.model_config.context_window = Some(context_window);
    Agent::new(config, &[(Role::System, "test system prompt".to_string())])
        .with_session_id(uuid::uuid!(SESSION_ID))
        .with_task_id(uuid::uuid!(TASK_ID))
        .with_tools(crate::clients::tools::ToolRegistry::empty())
}

fn responses_agent_with_window(base_url: &str, context_window: u32) -> Agent {
    let mut config = test_config(base_url);
    config.model_config.api_type = ApiType::Responses;
    config.model_config.context_window = Some(context_window);
    Agent::new(config, &[(Role::System, "test system prompt".to_string())])
        .with_session_id(uuid::uuid!(SESSION_ID))
        .with_task_id(uuid::uuid!(TASK_ID))
        .with_tools(crate::clients::tools::ToolRegistry::empty())
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

fn enabled_policy() -> CompactionPolicy {
    CompactionPolicy {
        enabled: true,
        threshold_percent: 10,
        trigger_tokens: None,
        retained_tokens: 20_000,
        summary_tokens: 4_096,
        summary_timeout_secs: 180,
    }
}

/// Keep the proactive trigger out of the way so a test exercises only the
/// reactive overflow path.
fn reactive_only_policy() -> CompactionPolicy {
    CompactionPolicy {
        threshold_percent: 80,
        ..enabled_policy()
    }
}

fn message(role: Role, content: &str) -> ConversationItem {
    ConversationItem::Message {
        role,
        content: content.to_string(),
        id: None,
        status: None,
        timestamp: None,
        images: Vec::new(),
    }
}

/// A long, complete tool history with the real current input at the end.
fn long_history() -> Vec<ConversationItem> {
    long_history_with(40)
}

fn long_history_with(turns: usize) -> Vec<ConversationItem> {
    let mut items = vec![message(Role::User, "start the task")];
    for index in 0..turns {
        let call_id = format!("call-{index}");
        items.push(ConversationItem::FunctionCall {
            id: format!("fc-{index}"),
            call_id: call_id.clone(),
            name: "Read".to_string(),
            arguments: "{}".to_string(),
            timestamp: None,
        });
        items.push(ConversationItem::FunctionCallOutput {
            call_id,
            output: "x".repeat(8_000),
            timestamp: None,
            images: Vec::new(),
        });
    }
    items.push(message(Role::User, "finish the task"));
    items
}

#[derive(Clone, Default)]
struct RecordSink {
    records: Arc<Mutex<Vec<SessionRecord>>>,
}

impl RecordSink {
    fn callback(&self) -> impl FnMut(&SessionRecord) -> anyhow::Result<()> + Send + Sync + 'static {
        let records = Arc::clone(&self.records);
        move |record| {
            records.lock().unwrap().push(record.clone());
            Ok(())
        }
    }

    fn records(&self) -> Vec<SessionRecord> {
        self.records.lock().unwrap().clone()
    }

    fn kinds(&self) -> Vec<&'static str> {
        self.records()
            .iter()
            .filter_map(|record| match record {
                SessionRecord::CompactionStart(_) => Some("compaction_start"),
                SessionRecord::Compaction(_) => Some("compaction"),
                SessionRecord::CompactionEnd(_) => Some("compaction_end"),
                _ => None,
            })
            .collect()
    }

    fn checkpoint(&self) -> Option<crate::types::CompactionData> {
        self.records().into_iter().find_map(|record| match record {
            SessionRecord::Compaction(checkpoint) => Some(checkpoint),
            _ => None,
        })
    }
}

fn summary_response() -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-summary",
        "model": "test-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "The earlier work read files with the Read tool."
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 10,
            "total_tokens": 110
        }
    })
}

async fn mount_summary(mock_server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(summary_response()))
        .mount(mock_server)
        .await;
}

async fn mount_summary_failure(mock_server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(400).set_body_string("summary rejected"))
        .mount(mock_server)
        .await;
}

/// Mount a main-agent response queue on the Chat Completions endpoint.
///
/// The summary server is separate in these tests, so every request here is a
/// logical main-agent attempt, including the one retry after pressure recovery.
async fn mount_chat_sequence(mock_server: &MockServer, responses: Vec<serde_json::Value>) {
    let responses = Arc::new(responses);
    let next = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(move |_request: &wiremock::Request| {
            let index = next.fetch_add(1, Ordering::SeqCst);
            let body = responses
                .get(index)
                .unwrap_or_else(|| panic!("main provider received unexpected request {index}"));
            ResponseTemplate::new(200).set_body_json(body.clone())
        })
        .mount(mock_server)
        .await;
}

fn over_window_chat_response(input_tokens: u64) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-over-window",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "partial answer",
                "tool_calls": [{
                    "id": "call-over-window",
                    "type": "function",
                    "function": {"name": "Read", "arguments": "{\"path\":\"/tmp/x\"}"}
                }]
            },
            "finish_reason": "length"
        }],
        "usage": {
            "prompt_tokens": input_tokens,
            "completion_tokens": 5,
            "total_tokens": input_tokens + 5
        }
    })
}

fn chat_message_response(text: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-final",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": text},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15
        }
    })
}

/// Mount a main-agent response queue on the Responses endpoint.
async fn mount_responses_sequence(mock_server: &MockServer, responses: Vec<serde_json::Value>) {
    let responses = Arc::new(responses);
    let next = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(move |_request: &wiremock::Request| {
            let index = next.fetch_add(1, Ordering::SeqCst);
            let body = responses
                .get(index)
                .unwrap_or_else(|| panic!("main provider received unexpected request {index}"));
            ResponseTemplate::new(200).set_body_json(body.clone())
        })
        .mount(mock_server)
        .await;
}

/// Mount prepared Responses replies so one script can mix JSON envelopes with
/// SSE streams, as the in-band `response.failed` path requires.
async fn mount_responses_template_sequence(
    mock_server: &MockServer,
    responses: Vec<ResponseTemplate>,
) {
    let responses = Arc::new(responses);
    let next = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(move |_request: &wiremock::Request| {
            let index = next.fetch_add(1, Ordering::SeqCst);
            responses
                .get(index)
                .cloned()
                .unwrap_or_else(|| panic!("main provider received unexpected request {index}"))
        })
        .mount(mock_server)
        .await;
}

fn responses_message_response(text: &str) -> serde_json::Value {
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

fn over_window_responses_response(input_tokens: u64) -> serde_json::Value {
    serde_json::json!({
        "id": "resp-over-window",
        "status": "incomplete",
        "incomplete_details": {"reason": "max_output_tokens"},
        "output": [{
            "type": "message",
            "id": "msg-partial",
            "status": "incomplete",
            "content": [{"type": "output_text", "text": "partial answer"}]
        }],
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": 5,
            "total_tokens": input_tokens + 5
        }
    })
}

/// A context window large enough that the long fixture history stays below the
/// proactive trigger, while the provider fixture reports a genuine overflow.
const RECOVERY_WINDOW: u32 = 1_000_000;
const OVER_WINDOW_INPUT: u64 = 1_500_000;

fn invocation_for(agent: &Agent) -> InvocationCompactionState {
    let current = agent.conversation.history().len().saturating_sub(1);
    InvocationCompactionState::new(current, current, TASK_ID.to_string())
}

#[tokio::test]
async fn omitted_policy_does_not_query_writer_health() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let mut agent = test_agent("https://api.example.com")
        .with_history(vec![message(Role::User, "hello")])
        .unwrap()
        .with_health_callback(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("writer failed")
        });
    let mut invocation = InvocationCompactionState::new(0, 0, TASK_ID.to_string());

    agent
        .maybe_compact_before_request(TurnMode::Normal, &mut invocation)
        .await
        .unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn disabled_policy_checks_writer_health_before_returning() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let mut agent = test_agent("https://api.example.com")
        .with_compaction_policy(CompactionPolicy::default())
        .unwrap()
        .with_history(vec![message(Role::User, "hello")])
        .unwrap()
        .with_health_callback(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("writer failed")
        });
    let mut invocation = InvocationCompactionState::new(0, 0, TASK_ID.to_string());

    let error = agent
        .maybe_compact_before_request(TurnMode::Normal, &mut invocation)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("writer failed"), "{error}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn enabled_compaction_validates_the_selected_summarizer_backend() {
    let mut summary = test_config("https://chatgpt.com/backend-api/codex");
    summary.model_config.api_type = ApiType::Responses;
    let Err(error) =
        test_agent("https://api.example.com").with_compaction(enabled_policy(), Some(summary))
    else {
        panic!("a Codex summarizer cannot create bounded checkpoints");
    };
    assert!(
        error.to_string().contains("bounded summary generation cap"),
        "{error}"
    );
}

#[tokio::test]
async fn summary_uses_separate_compaction_model() {
    let main_server = MockServer::start().await;
    let summary_server = MockServer::start().await;
    mount_summary(&summary_server).await;
    let mut summary_config = test_config(&summary_server.uri());
    summary_config.model_config.model = "summary-model".to_string();
    summary_config.model_config.reasoning_effort = Some(ReasoningEffort::Low);
    let sink = RecordSink::default();
    let mut agent = test_agent(&main_server.uri())
        .with_history(long_history())
        .unwrap()
        .with_compaction(enabled_policy(), Some(summary_config))
        .unwrap()
        .with_persist_callback(sink.callback())
        .with_last_usage(Some(Usage {
            input_tokens: 200_000,
            ..Usage::default()
        }));
    let mut invocation = invocation_for(&agent);
    agent.last_measurement = Some(MeasuredInput {
        input_tokens: 200_000,
        covered_item_count: agent.conversation.history().len(),
        model: "test-model".to_string(),
        fixed_tokens: 0,
        presence: UsagePresence::Complete,
    });

    agent
        .maybe_compact_before_request(TurnMode::Normal, &mut invocation)
        .await
        .unwrap();

    assert_eq!(
        sink.kinds(),
        vec!["compaction_start", "compaction", "compaction_end"]
    );
    let requests = summary_server
        .received_requests()
        .await
        .expect("summary requests");
    assert_eq!(requests.len(), 1, "one summary call expected");
    let body: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("summary request body should be JSON");
    assert_eq!(body["model"], "summary-model");
    assert_eq!(body["reasoning_effort"], "low");
    assert!(
        main_server
            .received_requests()
            .await
            .expect("main requests")
            .is_empty(),
        "the main provider must not receive the summary call"
    );
}

#[tokio::test]
async fn healthy_checkpoint_emits_start_checkpoint_end_and_activates() {
    let mock_server = MockServer::start().await;
    mount_summary(&mock_server).await;
    let sink = RecordSink::default();
    let history = long_history();
    let mut agent = test_agent(&mock_server.uri())
        .with_history(history.clone())
        .unwrap()
        .with_compaction_policy(enabled_policy())
        .unwrap()
        .with_persist_callback(sink.callback())
        .with_last_usage(Some(Usage {
            input_tokens: 200_000,
            ..Usage::default()
        }));
    let mut invocation = invocation_for(&agent);
    agent.last_measurement = Some(MeasuredInput {
        input_tokens: 200_000,
        covered_item_count: agent.conversation.history().len(),
        model: "test-model".to_string(),
        fixed_tokens: 0,
        presence: UsagePresence::Complete,
    });

    agent
        .maybe_compact_before_request(TurnMode::Normal, &mut invocation)
        .await
        .unwrap();

    assert_eq!(
        sink.kinds(),
        vec!["compaction_start", "compaction", "compaction_end"]
    );
    assert!(agent.conversation.has_checkpoint());
    assert!(agent.last_usage().is_none());
    assert!(agent.last_measurement().is_none());
    let checkpoint = sink.checkpoint().expect("checkpoint record");
    assert_eq!(checkpoint.session_id, SESSION_ID);
    assert_eq!(checkpoint.trigger, CompactionTrigger::Threshold);
    assert!(checkpoint.retained.iter().any(|record| {
        matches!(
            &record.record,
            crate::types::compaction::RetainedConversationRecord::Message(MessageData {
                content,
                ..
            }) if content == "finish the task"
        )
    }));
    let context = agent.conversation.request_context();
    assert!(context.iter().any(|item| {
        matches!(
            item,
            ConversationItem::Message { content, .. } if content.contains(HISTORICAL_SUMMARY_MARKER)
        )
    }));
    let end = sink
        .records()
        .into_iter()
        .find_map(|record| match record {
            SessionRecord::CompactionEnd(data) => Some(data),
            _ => None,
        })
        .expect("end record");
    assert_eq!(end.status, CompactionStatus::Committed);
    assert_eq!(
        end.estimated_before_tokens,
        Some(checkpoint.estimated_input.before_tokens)
    );
    assert_eq!(
        end.estimated_after_tokens,
        Some(checkpoint.estimated_input.after_tokens)
    );
    assert_eq!(end.retained_count, Some(checkpoint.retained.len()));
}

#[tokio::test]
async fn no_session_compaction_validates_and_activates_in_memory() {
    let mock_server = MockServer::start().await;
    mount_summary(&mock_server).await;
    let mut agent = test_agent(&mock_server.uri())
        .with_history(long_history())
        .unwrap()
        .with_compaction_policy(enabled_policy())
        .unwrap();
    let mut invocation = invocation_for(&agent);

    agent
        .maybe_compact_before_request(TurnMode::Normal, &mut invocation)
        .await
        .unwrap();

    assert!(agent.conversation.has_checkpoint());
    assert!(agent.last_measurement().is_none());
}

#[tokio::test]
async fn disabled_policy_with_a_healthy_writer_is_a_no_op() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let mut agent = test_agent("https://api.example.com")
        .with_compaction_policy(CompactionPolicy::default())
        .unwrap()
        .with_history(vec![message(Role::User, "hello")])
        .unwrap()
        .with_health_callback(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
    let mut invocation = InvocationCompactionState::new(1, 1, TASK_ID.to_string());

    agent
        .maybe_compact_before_request(TurnMode::Normal, &mut invocation)
        .await
        .unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!agent.conversation.has_checkpoint());
    assert!(!invocation.failed());
}

#[tokio::test]
async fn terminal_writer_failure_between_summary_batches_stops_spending() {
    let mock_server = MockServer::start().await;
    mount_summary(&mock_server).await;
    let sink = RecordSink::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let mut agent = test_agent_with_window(&mock_server.uri(), 1_000_000)
        .with_history(long_history_with(160))
        .unwrap()
        .with_compaction_policy(enabled_policy())
        .unwrap()
        .with_persist_callback(sink.callback())
        .with_health_callback(move || {
            if counter.fetch_add(1, Ordering::SeqCst) >= 2 {
                anyhow::bail!("writer failed between summary batches")
            }
            Ok(())
        });
    let mut invocation = invocation_for(&agent);

    agent
        .maybe_compact_before_request(TurnMode::Normal, &mut invocation)
        .await
        .unwrap();

    assert_eq!(
        mock_server
            .received_requests()
            .await
            .expect("received requests")
            .len(),
        1,
        "health is checked before the second summary batch"
    );
    let end = sink
        .records()
        .into_iter()
        .find_map(|record| match record {
            SessionRecord::CompactionEnd(data) => Some(data),
            _ => None,
        })
        .expect("failed end record");
    assert_eq!(end.status, CompactionStatus::Failed);
    assert!(invocation.failed());
}

#[tokio::test]
async fn summary_failure_waits_for_growth_before_retrying() {
    let mock_server = MockServer::start().await;
    mount_summary_failure(&mock_server).await;
    let sink = RecordSink::default();
    let history = long_history();
    let mut agent = test_agent(&mock_server.uri())
        .with_history(history.clone())
        .unwrap()
        .with_compaction_policy(enabled_policy())
        .unwrap()
        .with_persist_callback(sink.callback());
    let mut invocation = invocation_for(&agent);

    agent
        .maybe_compact_before_request(TurnMode::Normal, &mut invocation)
        .await
        .unwrap();

    assert!(invocation.failed());
    assert!(!agent.conversation.has_checkpoint());
    assert_eq!(sink.kinds(), vec!["compaction_start", "compaction_end"]);
    let end = sink
        .records()
        .into_iter()
        .find_map(|record| match record {
            SessionRecord::CompactionEnd(data) => Some(data),
            _ => None,
        })
        .unwrap();
    assert_eq!(end.status, CompactionStatus::Failed);
    assert_eq!(end.reason.as_deref(), Some("summary generation failed"));
    let records_after_failure = sink.records().len();
    let requests_after_failure = mock_server
        .received_requests()
        .await
        .expect("received requests")
        .len();
    assert!(requests_after_failure >= 1);

    agent
        .maybe_compact_before_request(TurnMode::Normal, &mut invocation)
        .await
        .unwrap();

    assert_eq!(sink.records().len(), records_after_failure);
    assert_eq!(
        mock_server
            .received_requests()
            .await
            .expect("received requests")
            .len(),
        requests_after_failure
    );
}

#[tokio::test]
async fn failed_summary_retries_once_the_projection_grows() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(400).set_body_string("summary rejected"))
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;
    mount_summary(&mock_server).await;
    let sink = RecordSink::default();
    let mut agent = test_agent(&mock_server.uri())
        .with_history(long_history())
        .unwrap()
        .with_compaction_policy(enabled_policy())
        .unwrap()
        .with_persist_callback(sink.callback());
    let mut invocation = invocation_for(&agent);

    agent
        .maybe_compact_before_request(TurnMode::Normal, &mut invocation)
        .await
        .unwrap();

    assert!(invocation.failed());
    assert!(!agent.conversation.has_checkpoint());
    let requests_after_failure = mock_server
        .received_requests()
        .await
        .expect("received requests")
        .len();

    // A little new content stays below the growth margin and makes no attempt.
    agent
        .conversation
        .push_user_message("not enough growth".to_string(), None);
    agent
        .maybe_compact_before_request(TurnMode::Normal, &mut invocation)
        .await
        .unwrap();
    assert!(invocation.failed());
    assert_eq!(
        mock_server
            .received_requests()
            .await
            .expect("received requests")
            .len(),
        requests_after_failure
    );

    // More than a tenth of the trigger lets the retry run, and the summary
    // succeeds on the next request.
    agent
        .conversation
        .push_user_message("y".repeat(40_000), None);
    agent
        .maybe_compact_before_request(TurnMode::Normal, &mut invocation)
        .await
        .unwrap();

    assert!(!invocation.failed());
    assert!(agent.conversation.has_checkpoint());
    assert!(
        mock_server
            .received_requests()
            .await
            .expect("received requests")
            .len()
            > requests_after_failure
    );
}

#[tokio::test]
async fn checkpoint_commit_failure_emits_no_end_or_activation() {
    let mock_server = MockServer::start().await;
    mount_summary(&mock_server).await;
    let records = Arc::new(Mutex::new(Vec::new()));
    let sink = RecordSink {
        records: Arc::clone(&records),
    };
    let history = long_history();
    let mut agent = test_agent(&mock_server.uri())
        .with_history(history.clone())
        .unwrap()
        .with_compaction_policy(enabled_policy())
        .unwrap()
        .with_persist_callback(move |record| {
            if matches!(record, SessionRecord::Compaction(_)) {
                anyhow::bail!("checkpoint write failed");
            }
            sink.records.lock().unwrap().push(record.clone());
            Ok(())
        });
    let mut invocation = invocation_for(&agent);

    let error = agent
        .maybe_compact_before_request(TurnMode::Normal, &mut invocation)
        .await
        .unwrap_err();

    assert!(
        error.to_string().contains("checkpoint write failed"),
        "{error}"
    );
    assert!(!agent.conversation.has_checkpoint());
    assert!(
        records
            .lock()
            .unwrap()
            .iter()
            .all(|record| !matches!(record, SessionRecord::CompactionEnd(_)))
    );
}

#[tokio::test]
async fn runner_health_failure_before_a_retry_sends_only_the_first_attempt() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("server error"))
        .expect(1)
        .mount(&mock_server)
        .await;
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let health: crate::clients::agent_observer::HealthCallback = Arc::new(move || {
        if counter.fetch_add(1, Ordering::SeqCst) >= 1 {
            anyhow::bail!("writer failed between retries");
        }
        Ok(())
    });
    let config = test_config(&mock_server.uri());
    let runner = AgentRunner::new(Backend::from_api_type(ApiType::ChatCompletions));
    let mut next_attempt = 1;

    let error = runner
        .complete_turn(
            &config,
            uuid::uuid!(SESSION_ID),
            uuid::uuid!(TASK_ID),
            1,
            &[],
            &[],
            None,
            None,
            Some(&health),
            &mut next_attempt,
            |_| {},
            |_| {},
        )
        .await
        .unwrap_err();

    assert!(
        error.to_string().contains("writer failed between retries"),
        "{error}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        mock_server
            .received_requests()
            .await
            .expect("received requests")
            .len(),
        1
    );
}

#[tokio::test]
async fn no_fit_at_trigger_emits_skip_and_suppresses_later_attempts() {
    let mock_server = MockServer::start().await;
    let sink = RecordSink::default();
    let progress = Arc::new(Mutex::new(Vec::<String>::new()));
    let progress_sink = Arc::clone(&progress);
    let system = "large system context ".repeat(20_000);
    let mut agent = test_agent_with_system(&mock_server.uri(), &system)
        .with_history(vec![message(Role::User, "finish the task")])
        .unwrap()
        .with_compaction_policy(enabled_policy())
        .unwrap()
        .with_persist_callback(sink.callback())
        .with_progress_callback(move |message| {
            progress_sink.lock().unwrap().push(message.to_string());
        });
    let mut invocation = invocation_for(&agent);
    let measured = agent
        .measure_for_request(TurnMode::Normal, &invocation)
        .unwrap();
    let trigger_tokens = agent
        .resolved_compaction_policy()
        .and_then(|policy| policy.trigger_tokens)
        .expect("a percentage policy resolves a trigger line");
    assert!(measured.trigger_reached);
    assert!(measured.before_tokens >= trigger_tokens);

    agent
        .maybe_compact_before_request(TurnMode::Normal, &mut invocation)
        .await
        .unwrap();

    assert!(invocation.failed());
    assert!(!agent.conversation.has_checkpoint());
    assert_eq!(sink.kinds(), vec!["compaction_end"]);
    assert!(
        mock_server
            .received_requests()
            .await
            .expect("received requests")
            .is_empty(),
        "a no-fit must not spend on a summary"
    );
    let end = sink
        .records()
        .into_iter()
        .find_map(|record| match record {
            SessionRecord::CompactionEnd(data) => Some(data),
            _ => None,
        })
        .expect("no-fit skip record");
    assert_eq!(end.status, CompactionStatus::NoFit);
    assert_eq!(end.estimated_before_tokens, Some(measured.before_tokens));
    assert_eq!(end.estimated_after_tokens, None);
    assert_eq!(end.retired_count, None);
    assert_eq!(end.retained_count, None);
    let reason = end.reason.expect("no-fit reason");
    assert!(!reason.is_empty());
    let messages = progress.lock().unwrap().clone();
    assert_eq!(
        messages,
        vec![format!("Context compaction skipped: {reason}")]
    );

    let records_after_skip = sink.records();
    let requests_after_skip = mock_server
        .received_requests()
        .await
        .expect("received requests")
        .len();
    let remeasured = agent
        .measure_for_request(TurnMode::Normal, &invocation)
        .unwrap();
    assert!(
        remeasured.trigger_reached,
        "the later proactive attempt is still above the trigger"
    );

    agent
        .maybe_compact_before_request(TurnMode::Normal, &mut invocation)
        .await
        .unwrap();

    assert_eq!(sink.records().len(), records_after_skip.len());
    assert_eq!(
        mock_server
            .received_requests()
            .await
            .expect("received requests")
            .len(),
        requests_after_skip
    );
    assert_eq!(progress.lock().unwrap().len(), 1);
    assert!(invocation.failed());
    assert!(!agent.conversation.has_checkpoint());
}

#[tokio::test]
async fn pressure_recovery_commits_one_overflow_checkpoint_and_retries_once() {
    let main_server = MockServer::start().await;
    let summary_server = MockServer::start().await;
    mount_summary(&summary_server).await;
    mount_chat_sequence(
        &main_server,
        vec![
            over_window_chat_response(OVER_WINDOW_INPUT),
            chat_message_response("finished"),
        ],
    )
    .await;
    let sink = RecordSink::default();
    let summary_config = test_config(&summary_server.uri());
    let mut agent = test_agent_with_window(&main_server.uri(), RECOVERY_WINDOW)
        .with_history(long_history_with(20))
        .unwrap()
        .with_compaction(reactive_only_policy(), Some(summary_config))
        .unwrap()
        .with_persist_callback(sink.callback());

    let result = agent.send("finish now".to_string()).await.unwrap();

    assert_eq!(result, "finished");
    assert_eq!(
        main_server
            .received_requests()
            .await
            .expect("main requests")
            .len(),
        2,
        "the discarded request gets exactly one logical retry"
    );
    assert_eq!(
        summary_server
            .received_requests()
            .await
            .expect("summary requests")
            .len(),
        1,
        "one reactive checkpoint means one summary call"
    );
    assert_eq!(
        sink.kinds(),
        vec!["compaction_start", "compaction", "compaction_end"]
    );
    let checkpoint = sink.checkpoint().expect("reactive checkpoint");
    assert_eq!(checkpoint.trigger, CompactionTrigger::Overflow);
    assert!(agent.conversation.has_checkpoint());
    assert_eq!(
        agent.tool_call_count, 0,
        "the discarded tool call must not run"
    );
    assert!(
        !agent.history().iter().any(|item| matches!(
            item,
            ConversationItem::FunctionCall { call_id, .. } if call_id == "call-over-window"
        )),
        "the discarded response must not persist its function call"
    );
    assert!(
        agent.total_usage().input_tokens >= OVER_WINDOW_INPUT,
        "the discarded attempt's reported usage is retained"
    );
}

#[tokio::test]
async fn in_band_responses_context_failure_recovers_once() {
    let main_server = MockServer::start().await;
    let summary_server = MockServer::start().await;
    mount_summary(&summary_server).await;
    let failed = ResponseTemplate::new(200)
        .insert_header("Content-Type", "text/event-stream")
        .set_body_string(
            "data: {\"type\":\"response.failed\",\"response\":{\"id\":\"resp-overflow\",\"status\":\"failed\",\"error\":{\"message\":\"input too long\",\"type\":\"invalid_request_error\",\"code\":\"context_length_exceeded\"},\"usage\":{\"input_tokens\":1500000,\"output_tokens\":0,\"total_tokens\":1500000}}}\n\n",
        );
    mount_responses_template_sequence(
        &main_server,
        vec![
            failed,
            ResponseTemplate::new(200).set_body_json(responses_message_response("recovered")),
        ],
    )
    .await;
    let sink = RecordSink::default();
    let summary_config = test_config(&summary_server.uri());
    let mut agent = responses_agent_with_window(&main_server.uri(), RECOVERY_WINDOW)
        .with_history(long_history_with(20))
        .unwrap()
        .with_compaction(reactive_only_policy(), Some(summary_config))
        .unwrap()
        .with_persist_callback(sink.callback());

    let result = agent.send("finish now".to_string()).await.unwrap();

    assert_eq!(result, "recovered");
    assert_eq!(
        main_server
            .received_requests()
            .await
            .expect("main requests")
            .len(),
        2,
        "the in-band failure is retried once after recovery"
    );
    assert_eq!(
        summary_server
            .received_requests()
            .await
            .expect("summary requests")
            .len(),
        1,
        "one in-band pressure verdict means one summary call"
    );
    assert_eq!(
        sink.checkpoint().expect("reactive checkpoint").trigger,
        CompactionTrigger::Overflow
    );
    assert!(
        agent.total_usage().input_tokens >= OVER_WINDOW_INPUT,
        "the failed stream's reported usage is retained"
    );
}

#[tokio::test]
async fn pressure_recovery_when_nothing_can_fit_ends_neutrally() {
    let main_server = MockServer::start().await;
    let summary_server = MockServer::start().await;
    mount_summary(&summary_server).await;
    mount_chat_sequence(
        &main_server,
        vec![over_window_chat_response(OVER_WINDOW_INPUT)],
    )
    .await;
    let sink = RecordSink::default();
    let summary_config = test_config(&summary_server.uri());
    let system = "large pinned context ".repeat(20_000);
    let mut agent = test_agent_with_system(&main_server.uri(), &system)
        .with_compaction(reactive_only_policy(), Some(summary_config))
        .unwrap()
        .with_persist_callback(sink.callback());

    let error = agent.send("go".to_string()).await.unwrap_err();

    assert!(
        error.downcast_ref::<crate::types::CutOffError>().is_some(),
        "no-fit recovery ends with the neutral pressure error: {error:#}"
    );
    assert_eq!(
        main_server
            .received_requests()
            .await
            .expect("main requests")
            .len(),
        1,
        "a no-fit must not retry the same request"
    );
    assert!(
        summary_server
            .received_requests()
            .await
            .expect("summary requests")
            .is_empty(),
        "a no-fit must not spend on a summary"
    );
    assert_eq!(sink.kinds(), vec!["compaction_end"]);
    let end = sink
        .records()
        .into_iter()
        .find_map(|record| match record {
            SessionRecord::CompactionEnd(data) => Some(data),
            _ => None,
        })
        .expect("skip record");
    assert_eq!(end.trigger, CompactionTrigger::Overflow);
    assert_eq!(end.status, CompactionStatus::NoFit);
    assert!(!agent.conversation.has_checkpoint());
}

#[tokio::test]
async fn a_second_pressure_failure_does_not_compact_again() {
    let main_server = MockServer::start().await;
    let summary_server = MockServer::start().await;
    mount_summary(&summary_server).await;
    mount_chat_sequence(
        &main_server,
        vec![
            over_window_chat_response(OVER_WINDOW_INPUT),
            over_window_chat_response(OVER_WINDOW_INPUT),
        ],
    )
    .await;
    let sink = RecordSink::default();
    let summary_config = test_config(&summary_server.uri());
    let mut agent = test_agent_with_window(&main_server.uri(), RECOVERY_WINDOW)
        .with_history(long_history_with(20))
        .unwrap()
        .with_compaction(reactive_only_policy(), Some(summary_config))
        .unwrap()
        .with_persist_callback(sink.callback());

    let error = agent.send("finish now".to_string()).await.unwrap_err();

    assert!(
        error.downcast_ref::<crate::types::CutOffError>().is_some(),
        "the second pressure failure ends neutrally: {error:#}"
    );
    assert_eq!(
        main_server
            .received_requests()
            .await
            .expect("main requests")
            .len(),
        2,
        "the intended turn is retried exactly once"
    );
    assert_eq!(
        summary_server
            .received_requests()
            .await
            .expect("summary requests")
            .len(),
        1,
        "the guard prevents a second compaction"
    );
    assert_eq!(
        sink.kinds(),
        vec!["compaction_start", "compaction", "compaction_end"]
    );
    assert_eq!(
        sink.checkpoint().expect("checkpoint").trigger,
        CompactionTrigger::Overflow
    );
}

#[tokio::test]
async fn a_prior_proactive_checkpoint_still_allows_only_one_reactive_recovery() {
    let main_server = MockServer::start().await;
    let summary_server = MockServer::start().await;
    mount_summary(&summary_server).await;
    mount_chat_sequence(
        &main_server,
        vec![
            over_window_chat_response(OVER_WINDOW_INPUT),
            over_window_chat_response(OVER_WINDOW_INPUT),
        ],
    )
    .await;
    let sink = RecordSink::default();
    let summary_config = test_config(&summary_server.uri());
    let mut agent = test_agent_with_window(&main_server.uri(), RECOVERY_WINDOW)
        .with_history(long_history_with(60))
        .unwrap()
        .with_compaction(enabled_policy(), Some(summary_config))
        .unwrap()
        .with_persist_callback(sink.callback());

    let error = agent.send("finish now".to_string()).await.unwrap_err();

    assert!(error.downcast_ref::<crate::types::CutOffError>().is_some());
    let checkpoints: Vec<_> = sink
        .records()
        .into_iter()
        .filter_map(|record| match record {
            SessionRecord::Compaction(checkpoint) => Some(checkpoint),
            _ => None,
        })
        .collect();
    assert_eq!(
        checkpoints.len(),
        2,
        "one proactive and one reactive checkpoint"
    );
    assert_eq!(checkpoints[0].trigger, CompactionTrigger::Threshold);
    assert_eq!(checkpoints[1].trigger, CompactionTrigger::Overflow);
    assert_eq!(
        main_server
            .received_requests()
            .await
            .expect("main requests")
            .len(),
        2
    );
    assert_eq!(
        summary_server
            .received_requests()
            .await
            .expect("summary requests")
            .len(),
        2,
        "one summary per committed checkpoint, never a second reactive attempt"
    );
}

#[tokio::test]
async fn pressure_recovery_keeps_schema_correction_constrained_and_tools_disabled() {
    let main_server = MockServer::start().await;
    let summary_server = MockServer::start().await;
    mount_summary(&summary_server).await;
    mount_responses_sequence(
        &main_server,
        vec![
            responses_message_response("not json"),
            over_window_responses_response(OVER_WINDOW_INPUT),
            responses_message_response(r#"{"summary":"recovered"}"#),
        ],
    )
    .await;

    let mut agent = responses_agent_with_window(&main_server.uri(), RECOVERY_WINDOW)
        .with_history(long_history_with(20))
        .unwrap()
        .with_output_schema(test_schema())
        .with_compaction(
            reactive_only_policy(),
            Some(test_config(&summary_server.uri())),
        )
        .unwrap();

    let result = agent.send("finish now".to_string()).await.unwrap();

    assert!(result.contains("recovered"), "{result}");
    assert_eq!(
        summary_server
            .received_requests()
            .await
            .expect("summary requests")
            .len(),
        1,
        "the pressure recovery committed one checkpoint"
    );
    let requests = main_server
        .received_requests()
        .await
        .expect("main requests");
    assert_eq!(
        requests.len(),
        3,
        "initial response, discarded pressure response, retried correction"
    );
    for (index, request) in requests.iter().enumerate().skip(1) {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert!(
            body.get("tools").is_none(),
            "request {index} offered tools during correction: {body}"
        );
        assert_eq!(
            body["text"]["format"]["type"], "json_schema",
            "request {index} lost the native constraint"
        );
        let has_corrective = body["input"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item["content"][0]["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("failed output schema validation"))
            })
        });
        assert!(
            has_corrective,
            "request {index} lost correction mode: {body}"
        );
    }
}

#[test]
fn fixed_surface_matches_correction_turns() {
    let agent = test_agent("https://api.example.com");
    let normal = agent.fixed_request_surface(TurnMode::Normal);
    assert!(normal.tool_schemas.is_empty());
    assert!(normal.output_schema.is_none());

    let correction = agent.fixed_request_surface(TurnMode::SchemaCorrection);
    assert!(correction.tool_schemas.is_empty());
    assert!(correction.output_schema.is_none());
}

#[test]
fn lifecycle_reason_is_bounded_on_a_character_boundary() {
    let long = "é".repeat(crate::types::compaction::MAX_COMPACTION_REASON_BYTES);
    let bounded = crate::types::compaction::bounded_compaction_reason(&long);
    assert!(bounded.len() <= crate::types::compaction::MAX_COMPACTION_REASON_BYTES);
    assert!(bounded.ends_with("..."));
    assert!(bounded.is_char_boundary(bounded.len()));
}
