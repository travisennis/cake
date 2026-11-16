use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::clients::agent_runner::AgentRunner;
use crate::clients::backend::Backend;
use crate::clients::compaction::MeasuredInput;
use crate::config::model::{ApiType, ModelConfig, ResolvedModelConfig};
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

fn enabled_policy() -> CompactionPolicy {
    CompactionPolicy {
        enabled: true,
        threshold_percent: 10,
        trigger_tokens: None,
        retained_tokens: 20_000,
        summary_tokens: 4_096,
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
    assert!(!invocation.failed);
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
    assert!(invocation.failed);
}

#[tokio::test]
async fn summary_failure_emits_failed_end_and_suppresses_later_attempts() {
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

    assert!(invocation.failed);
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

    assert!(invocation.failed);
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
    assert!(invocation.failed);
    assert!(!agent.conversation.has_checkpoint());
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
