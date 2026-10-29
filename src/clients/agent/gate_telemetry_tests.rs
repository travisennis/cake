//! Gate telemetry wiring at session startup.

use std::sync::Arc;

use super::*;
use crate::clients::tools::bash_gate_fixture as gate_fixture;
use crate::config::model::{ApiType, ModelConfig};
use crate::gates::composer::GatePipeline;
use crate::gates::protocol::GateDecision;

fn resolved_model() -> ResolvedModelConfig {
    ResolvedModelConfig {
        model_config: ModelConfig {
            supports_images: false,
            model: "judge-model".to_string(),
            api_type: ApiType::ChatCompletions,
            base_url: "http://127.0.0.1:9".to_string(),
            api_key_env: "AGENT_GATE_TEST_KEY".to_string(),
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

fn pipeline() -> Arc<GatePipeline> {
    gate_fixture::decision(GateDecision::Allow, None, "Safe").pipeline()
}

fn agent_with_pipeline(
    pipeline: &Arc<GatePipeline>,
) -> (Agent, tempfile::TempDir, std::path::PathBuf) {
    let tool_context =
        Arc::new(ToolContext::from_current_process().with_gates(Some(Arc::clone(pipeline))));
    let agent = Agent::new(resolved_model(), &[]).with_tool_context(tool_context);
    let dir = tempfile::TempDir::new().expect("telemetry dir");
    let path = dir.path().join("sidecar.ndjson");
    (
        agent.with_session_telemetry(
            SessionTelemetryWriter::open(&path).expect("open sidecar"),
            uuid::Uuid::new_v4(),
        ),
        dir,
        path,
    )
}

fn records(path: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .expect("read sidecar")
        .lines()
        .map(|line| serde_json::from_str(line).expect("record parses"))
        .collect()
}

#[test]
fn session_startup_emits_a_ready_gate_record() {
    let (agent, _dir, path) = agent_with_pipeline(&pipeline());
    drop(agent);
    let records = records(&path);
    let startup = records
        .iter()
        .find(|record| record["type"] == "gate_startup")
        .expect("gate startup");
    assert_eq!(startup["gate"], "fake-safety");
    assert_eq!(startup["version"], "1.0.0-test");
    assert_eq!(startup["mode"], "enforce");
    assert_eq!(startup["outcome"], "ready");
    assert_eq!(startup["phases"], serde_json::json!(["pre_tool"]));
    assert_eq!(startup["actions"], serde_json::json!(["bash.exec"]));
    assert!(startup["failure_class"].is_null());
}

#[test]
fn session_startup_emits_no_gate_record_for_the_explicit_bypass() {
    let (agent, _dir, path) = agent_with_pipeline(&Arc::new(GatePipeline::bypass()));
    drop(agent);
    let records = records(&path);
    assert!(
        records
            .iter()
            .all(|record| record["type"] != "gate_startup"),
        "a bypass has no gate to start"
    );
}
