use std::sync::Arc;
use std::time::Duration;

use clap::CommandFactory;

use super::*;
use crate::config::gates::GateConfiguration;
use crate::config::settings::{
    JudgeSettings, LoadedSettings, ResolvedLimits, SandboxSettings, SkillSettings,
};
use crate::gates::composer::{
    FailMode, Gate, GateAnswer, GateFuture, GateMode, GatePipeline, GateStageConfig, GateStartup,
};
use crate::gates::protocol::{
    CheckFrame, DecisionFrame, Describe, GateAction, GateDecision, GatePhase, GateTransport,
    PROTOCOL_VERSION,
};

#[derive(Debug)]
struct FixedGate {
    describe: Describe,
    decision: GateDecision,
    code: Option<String>,
    message: String,
}

impl FixedGate {
    fn new(decision: GateDecision) -> Self {
        Self {
            describe: Describe {
                protocol: PROTOCOL_VERSION.to_string(),
                name: "fixed-gate".to_string(),
                version: "0.1.0".to_string(),
                phases: vec![GatePhase::PreTool],
                actions: vec![GateAction::BashExec],
                transports: vec![GateTransport::Stdio],
                capabilities: Vec::new(),
            },
            decision,
            code: Some("fixed-code".to_string()),
            message: "fixed message".to_string(),
        }
    }
}

impl Gate for FixedGate {
    fn describe(&self) -> &Describe {
        &self.describe
    }

    fn check(&self, frame: CheckFrame) -> GateFuture<'_> {
        Box::pin(async move {
            let mut decision =
                DecisionFrame::new(frame.id, self.decision).with_message(self.message.clone());
            if let Some(code) = &self.code {
                decision = decision.with_code(code.clone());
            }
            GateAnswer::decision(decision, Duration::from_millis(12))
        })
    }
}

fn pipeline(decision: GateDecision, mode: GateMode) -> GatePipeline {
    let gate = Arc::new(FixedGate::new(decision));
    let config = GateStageConfig {
        name: "safety".to_string(),
        mode,
        fail: FailMode::Closed,
        short_circuit_on_allow: false,
        allowlist: Vec::new(),
    };
    GatePipeline::single(GateStartup::ready(config, gate, Duration::from_millis(4)))
        .expect("valid fake pipeline")
}

fn pipeline_with_allowlist(decision: GateDecision, command: &str) -> GatePipeline {
    let gate = Arc::new(FixedGate::new(decision));
    let config = GateStageConfig {
        name: "safety".to_string(),
        mode: GateMode::Enforce,
        fail: FailMode::Closed,
        short_circuit_on_allow: false,
        allowlist: vec![command.to_string()],
    };
    GatePipeline::single(GateStartup::ready(config, gate, Duration::from_millis(4)))
        .expect("valid fake pipeline")
}

#[test]
fn parses_bash_check_with_or_without_double_dash() {
    for args in [
        vec!["cake", "bash", "check", "--", "git status"],
        vec!["cake", "bash", "check", "git status"],
    ] {
        let parsed = crate::CodingAssistant::parse_from(args);
        let Some(crate::cli::Commands::Bash(command)) = parsed.command else {
            panic!("expected bash command");
        };
        let BashSubcommand::Check(check) = command.command;
        assert_eq!(check.command, "git status");
    }
}

#[test]
fn diagnostic_flag_is_still_parseable_and_conflicts_with_json() {
    let parsed = crate::CodingAssistant::parse_from([
        "cake",
        "bash",
        "check",
        "--diagnostic",
        "--",
        "git status",
    ]);
    let Some(crate::cli::Commands::Bash(command)) = parsed.command else {
        panic!("expected bash command");
    };
    let BashSubcommand::Check(check) = command.command;
    assert!(check.diagnostic);

    let Err(error) = crate::CodingAssistant::try_parse_from([
        "cake",
        "bash",
        "check",
        "--json",
        "--diagnostic",
        "--",
        "git status",
    ]) else {
        panic!("output modes must conflict");
    };
    assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn bash_help_says_check_does_not_execute_the_command() {
    let help = BashCommand::command().render_help().to_string();
    assert!(help.contains("without executing"), "{help}");
}

#[tokio::test]
async fn gate_check_json_preserves_fields_and_adds_gate_stages() {
    let pipeline = pipeline(GateDecision::Block, GateMode::Enforce);
    let outcome = evaluate_pipeline(&pipeline, Path::new("/repo"), "git push --force")
        .await
        .expect("check succeeds");
    let document: Value = serde_json::from_str(&outcome.render_json(&[]).unwrap()).unwrap();

    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["command"], "bash check");
    assert_eq!(document["status"], "ok");
    assert_eq!(document["summary"]["verdict"], "block");
    assert_eq!(document["checks"], json!([]));
    assert_eq!(document["data"]["verdict"], "block");
    assert_eq!(document["data"]["code"], "fixed-code");
    assert_eq!(document["data"]["confidence"], Value::Null);
    assert_eq!(document["data"]["message"], "fixed message");
    assert_eq!(document["data"]["overridden"], false);
    assert_eq!(document["data"]["bypassed"], false);
    assert_eq!(document["data"]["stage"], "gate");
    assert_eq!(document["data"]["probability"], Value::Null);
    assert_eq!(document["data"]["observation"]["outcome"], "absent");
    assert_eq!(document["data"]["gate"], "safety");
    let stages = document["data"]["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 1);
    assert_eq!(stages[0]["failed_open"], false);
    assert_eq!(stages[0]["disabled"], false);
}

#[tokio::test]
async fn allowlist_override_keeps_the_block_verdict_and_flag() {
    let pipeline = pipeline_with_allowlist(GateDecision::Block, "git status");
    let outcome = evaluate_pipeline(&pipeline, Path::new("/repo"), "git status")
        .await
        .expect("check succeeds");
    let document: Value = serde_json::from_str(&outcome.render_json(&[]).unwrap()).unwrap();

    assert_eq!(document["summary"]["verdict"], "block");
    assert_eq!(document["data"]["verdict"], "block");
    assert_eq!(document["data"]["gate"], "safety");
    assert_eq!(document["data"]["overridden"], true);
    assert_eq!(document["data"]["stages"][0]["decision"], "block");
    assert!(outcome.render_text().contains("Overridden: allowlist"));
}

#[tokio::test]
async fn gate_check_text_reports_warn_and_deciding_gate() {
    let pipeline = pipeline(GateDecision::Warn, GateMode::Enforce);
    let outcome = evaluate_pipeline(&pipeline, Path::new("/repo"), "git status")
        .await
        .expect("check succeeds");
    let output = outcome.render_text();
    assert!(output.contains("Verdict: warn"));
    assert!(output.contains("Gate: safety"));
    assert!(output.contains("Code: fixed-code"));
    assert!(output.contains("Message: fixed message"));
}

#[tokio::test]
async fn bypass_json_keeps_null_verdict_and_empty_stage_list() {
    let pipeline = GatePipeline::bypass();
    let outcome = evaluate_pipeline(&pipeline, Path::new("/repo"), "git status")
        .await
        .expect("bypass succeeds");
    let document: Value = serde_json::from_str(&outcome.render_json(&[]).unwrap()).unwrap();

    assert_eq!(document["summary"]["verdict"], "bypassed");
    assert_eq!(document["data"]["verdict"], Value::Null);
    assert_eq!(document["data"]["gate"], Value::Null);
    assert_eq!(document["data"]["bypassed"], true);
    assert_eq!(document["data"]["stages"], json!([]));
}

#[tokio::test]
async fn all_abstain_is_a_valid_generic_pipeline_result() {
    let pipeline = pipeline(GateDecision::Abstain, GateMode::Enforce);
    let outcome = evaluate_pipeline(&pipeline, Path::new("/repo"), "git status")
        .await
        .expect("abstain is a valid answer");
    let document: Value = serde_json::from_str(&outcome.render_json(&[]).unwrap()).unwrap();

    assert_eq!(document["summary"]["verdict"], "allow");
    assert_eq!(document["data"]["verdict"], Value::Null);
    assert_eq!(document["data"]["gate"], Value::Null);
    assert_eq!(document["data"]["stage"], Value::Null);
}

#[tokio::test]
async fn explicit_no_gate_bypasses_before_model_resolution() {
    let loaded = LoadedSettings {
        gates: GateConfiguration::default(),
        models: std::collections::HashMap::default(),
        default_model: Some("missing-model".to_string()),
        directories: Vec::new(),
        sandbox: SandboxSettings::default(),
        skills: SkillSettings::default(),
        tools_enabled: None,
        system_prompt: None,
        judge: JudgeSettings::default(),
        limits: ResolvedLimits::default(),
        warnings: Vec::new(),
    };

    let outcome = run_bash_check(
        &loaded,
        Path::new("/repo"),
        "git status",
        Some("unknown-model"),
        true,
    )
    .await
    .expect("explicit bypass does not resolve models or start a process");
    assert!(outcome.outcome.bypassed);
}

#[test]
fn gate_errors_have_explicit_diagnostic_classes() {
    use crate::gates::composer::GateFailure;
    use crate::gates::protocol::GateErrorClass;
    use crate::session_telemetry::GateStartupTelemetry;

    let failure = GateFailure::new(GateErrorClass::Auth, "missing credential");
    let error = GatePipelineError::Startup {
        gate: "safety".to_string(),
        failure,
        startup: Box::new(GateStartupTelemetry {
            gate: "safety".to_string(),
            version: None,
            mode: "enforce".to_string(),
            phases: Vec::new(),
            actions: Vec::new(),
            outcome: "failed".to_string(),
            failure_class: Some("auth".to_string()),
            latency_ms: 0,
        }),
    };
    assert_eq!(gate_error_check_id(&error), "gates.authentication");
    assert_eq!(
        crate::exit_code::classify_to_u8(&anyhow::Error::new(error)),
        2
    );
}
