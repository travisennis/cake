use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::clients::typesafe::TypeSafeObservation;
use crate::config::model::ResolvedModelConfig;
use crate::config::model::{ApiType, ModelConfig};
use crate::gates::composer::{FailMode, GateMode};
use crate::gates::protocol::{
    BashInput, CheckOptions, GateAction, GatePhase, GateTransport, PROTOCOL_VERSION, ScriptEvidence,
};

fn model_config() -> ModelConfig {
    ModelConfig {
        supports_images: false,
        model: "judge-model".to_string(),
        api_type: ApiType::ChatCompletions,
        base_url: "http://127.0.0.1:9".to_string(),
        api_key_env: "JUDGE_TEST_KEY".to_string(),
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
    }
}

fn judge_context() -> Arc<JudgeContext> {
    Arc::new(JudgeContext {
        settings: JudgeSettings::default(),
        bypass_env: None,
        agent_model: ResolvedModelConfig {
            model_config: model_config(),
            api_key: String::new(),
        },
        models: HashMap::new(),
        client: std::sync::OnceLock::new(),
        record_attempt: None,
    })
}

#[test]
fn builtin_describe_reports_the_reserved_name_and_package_version() {
    let describe = builtin_describe();
    describe.validate().expect("builtin describe validates");
    assert_eq!(describe.protocol, PROTOCOL_VERSION);
    assert_eq!(describe.name, BUILTIN_GATE_NAME);
    assert_eq!(describe.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(describe.phases, vec![GatePhase::PreTool]);
    assert_eq!(describe.actions, vec![GateAction::BashExec]);
    assert_eq!(describe.transports, vec![GateTransport::Stdio]);
    assert!(describe.capabilities.is_empty());
}

#[test]
fn builtin_stage_config_is_an_enforce_gate_with_the_legacy_allowlist() {
    let settings = JudgeSettings {
        allowlist: vec!["git status".to_string()],
        ..JudgeSettings::default()
    };
    let config = builtin_stage_config(&settings);
    assert_eq!(config.name, BUILTIN_GATE_NAME);
    assert_eq!(config.mode, GateMode::Enforce);
    assert_eq!(config.fail, FailMode::Closed);
    assert!(!config.short_circuit_on_allow);
    assert_eq!(config.allowlist, vec!["git status".to_string()]);
}

#[test]
fn builtin_pipeline_emits_a_ready_startup_record() {
    let pipeline = builtin_pipeline(judge_context()).expect("builtin pipeline starts");
    let records = pipeline.startup_records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].gate, BUILTIN_GATE_NAME);
    assert_eq!(
        records[0].version.as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(records[0].mode, "enforce");
    assert_eq!(records[0].outcome, "ready");
    assert_eq!(records[0].phases, vec!["pre_tool"]);
    assert_eq!(records[0].actions, vec!["bash.exec"]);
    assert_eq!(records[0].failure_class, None);
    assert!(pipeline.judge_context().is_some());
}

#[test]
fn stage_observation_matches_the_bash_check_vocabulary() {
    let observation = TypeSafeObservation {
        elapsed: Duration::from_millis(120),
        model: Some("jev-1.13.0".to_string()),
        probability: Some(0.91),
        usage_input_tokens: None,
        usage_output_tokens: None,
        failure_class: None,
    };
    let approved = stage_observation(Some(&observation), TypeSafeMode::Cascade, true);
    assert_eq!(approved.outcome, StageObservationOutcome::Approved);
    assert_eq!(approved.probability, Some(0.91));
    assert_eq!(approved.elapsed_ms, Some(120));

    let below = stage_observation(Some(&observation), TypeSafeMode::Cascade, false);
    assert_eq!(below.outcome, StageObservationOutcome::BelowCutoff);

    let shadow = stage_observation(Some(&observation), TypeSafeMode::Shadow, false);
    assert_eq!(shadow.outcome, StageObservationOutcome::Shadow);

    let off = stage_observation(Some(&observation), TypeSafeMode::Off, true);
    assert_eq!(off.outcome, StageObservationOutcome::Absent);

    let failed = TypeSafeObservation {
        failure_class: Some("timeout"),
        ..observation
    };
    let failed = stage_observation(Some(&failed), TypeSafeMode::Cascade, false);
    assert_eq!(failed.outcome, StageObservationOutcome::Failed);
    assert_eq!(failed.failure_class, Some("timeout"));

    let absent = stage_observation(None, TypeSafeMode::Cascade, false);
    assert_eq!(absent, StageObservation::absent());
}

#[test]
fn judge_error_classes_preserve_the_existing_exit_classes() {
    for (status, expected) in [
        (401, GateErrorClass::Auth),
        (403, GateErrorClass::Auth),
        (408, GateErrorClass::Timeout),
        (429, GateErrorClass::RateLimit),
        (500, GateErrorClass::Transport),
    ] {
        let error = JudgeError::Transport {
            status: Some(status),
            detail: "failed".to_string(),
        };
        assert_eq!(gate_error_class(&error), expected, "status {status}");
    }
    let network = JudgeError::Transport {
        status: None,
        detail: "connection refused".to_string(),
    };
    assert_eq!(gate_error_class(&network), GateErrorClass::Network);
    let transport = JudgeError::Transport {
        status: None,
        detail: "other".to_string(),
    };
    assert_eq!(gate_error_class(&transport), GateErrorClass::Transport);
    assert_eq!(
        gate_error_class(&JudgeError::Timeout(Duration::from_secs(1))),
        GateErrorClass::Timeout
    );
    assert_eq!(
        gate_error_class(&JudgeError::Malformed("bad".to_string())),
        GateErrorClass::Malformed
    );
    assert_eq!(
        gate_error_class(&JudgeError::Refusal),
        GateErrorClass::Refusal
    );
}

#[test]
fn judge_request_maps_the_check_frame() {
    let mut frame = CheckFrame::new(
        "check-1".to_string(),
        BashInput {
            command: "bash run.sh".to_string(),
            cwd: "/repo".to_string(),
            reason: Some("run the suite".to_string()),
            repo_digest: Some("git repo, branch main".to_string()),
            script_evidence: vec![ScriptEvidence {
                path: "/repo/run.sh".to_string(),
                contents: "echo hi".to_string(),
            }],
        },
    );
    frame.session = Some(crate::gates::protocol::CheckSession {
        call_id: Some("call-7".to_string()),
        ..crate::gates::protocol::CheckSession::default()
    });
    frame.options = CheckOptions { diagnostic: true };
    let request = judge_request(&frame).expect("request maps");
    assert_eq!(request.command, "bash run.sh");
    assert_eq!(request.cwd, std::path::PathBuf::from("/repo"));
    assert_eq!(request.reason.as_deref(), Some("run the suite"));
    assert_eq!(
        request.repo_digest.as_deref(),
        Some("git repo, branch main")
    );
    assert_eq!(request.call_id.as_deref(), Some("call-7"));
    let evidence = request.script_evidence.expect("script evidence");
    assert_eq!(evidence.path, "/repo/run.sh");
    assert_eq!(evidence.contents, "echo hi");
}

#[test]
fn compatibility_verdict_keeps_the_gate_vocabulary() {
    let verdict = compatibility_verdict(
        GateDecision::Warn,
        Some("fake-warn".to_string()),
        "look".to_string(),
        Some(0.5),
    );
    assert_eq!(verdict.decision, JudgeDecision::Warn);
    assert_eq!(verdict.code.as_deref(), Some("fake-warn"));
    assert_eq!(verdict.message, "look");
    assert_eq!(verdict.confidence, Some(0.5));
    assert_eq!(
        compatibility_verdict(GateDecision::Block, None, "stop".to_string(), None).decision,
        JudgeDecision::Block
    );
}
