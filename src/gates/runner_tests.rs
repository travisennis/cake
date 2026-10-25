use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::gates::composer::{
    FailMode, GatePipeline, GateStageConfig, GateStartup, PipelineAuthorization,
};
use crate::gates::protocol::{BashInput, GateDecision, InitSession};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gates/fake-gate.sh")
}

fn spec(mode: &str) -> GateProcessSpec {
    let mut environment = BTreeMap::new();
    environment.insert("FAKE_GATE_MODE".to_string(), mode.to_string());
    GateProcessSpec::new(fixture())
        .with_environment(environment)
        .with_timeouts(Duration::from_secs(2), Duration::from_secs(2))
}

fn init_frame() -> InitFrame {
    InitFrame::new(InitSession {
        session_id: None,
        task_id: None,
        invocation_id: Some("invocation".to_string()),
        cwd: "/repo".to_string(),
    })
}

fn check_frame(id: &str) -> CheckFrame {
    CheckFrame::new(
        id.to_string(),
        BashInput::new("git status --short".to_string(), "/repo".to_string()),
    )
}

fn request() -> crate::gates::composer::GateRequest {
    crate::gates::composer::GateRequest::new(BashInput::new(
        "git status --short".to_string(),
        "/repo".to_string(),
    ))
}

#[tokio::test]
async fn describe_init_check_and_shutdown_round_trip() {
    let mut process = GateProcess::new(spec("allow"));
    let describe = process.describe().await.expect("describe");
    assert_eq!(describe.name, "fake-safety");
    assert_eq!(describe.version, "0.1.0");
    let ready = process.start(init_frame()).await.expect("init");
    assert_eq!(ready.name, describe.name);
    let decision = process.check(check_frame("check-1")).await.expect("check");
    assert_eq!(decision.decision, GateDecision::Allow);
    assert_eq!(decision.id, "check-1");
    process.shutdown().await;
}

#[tokio::test]
async fn warn_block_and_abstain_decisions_are_parsed() {
    for (mode, expected) in [
        ("warn", GateDecision::Warn),
        ("block", GateDecision::Block),
        ("abstain", GateDecision::Abstain),
    ] {
        let mut process = GateProcess::new(spec(mode));
        process.describe().await.expect("describe");
        process.start(init_frame()).await.expect("init");
        let decision = process.check(check_frame("check-1")).await.expect("check");
        assert_eq!(decision.decision, expected, "mode {mode}");
        if expected == GateDecision::Warn {
            assert_eq!(decision.code.as_deref(), Some("fake-warn"));
            assert!(!decision.message.is_empty());
        }
        if expected == GateDecision::Block {
            assert_eq!(decision.code.as_deref(), Some("fake-block"));
        }
        process.shutdown().await;
    }
}

#[tokio::test]
async fn malformed_frame_fails_the_check_and_two_failures_disable_the_gate() {
    let mut process = GateProcess::new(spec("malformed"));
    process.describe().await.expect("describe");
    process.start(init_frame()).await.expect("init");
    let first = process.check(check_frame("check-1")).await;
    assert!(matches!(
        first,
        Err(GateCheckFailure::Failure(GateFailure {
            class: GateErrorClass::Protocol,
            ..
        }))
    ));
    let second = process.check(check_frame("check-2")).await;
    assert!(matches!(
        second,
        Err(GateCheckFailure::Failure(GateFailure {
            class: GateErrorClass::Protocol,
            ..
        }))
    ));
    let third = process.check(check_frame("check-3")).await;
    assert!(matches!(third, Err(GateCheckFailure::Disabled { .. })));
}

#[tokio::test]
async fn crash_restarts_the_process_and_a_success_resets_the_breaker() {
    let state_dir = tempfile::tempdir().expect("state dir");
    let state = state_dir.path().join("count");
    let mut environment = BTreeMap::new();
    environment.insert("FAKE_GATE_MODE".to_string(), "crash-once".to_string());
    environment.insert("FAKE_GATE_STATE".to_string(), state.display().to_string());
    let mut process = GateProcess::new(
        GateProcessSpec::new(fixture())
            .with_environment(environment)
            .with_timeouts(Duration::from_secs(2), Duration::from_secs(2)),
    );
    process.describe().await.expect("describe");
    process.start(init_frame()).await.expect("init");
    let first = process.check(check_frame("check-1")).await;
    assert!(matches!(
        first,
        Err(GateCheckFailure::Failure(GateFailure {
            class: GateErrorClass::Crash,
            ..
        }))
    ));
    let second = process
        .check(check_frame("check-2"))
        .await
        .expect("restart");
    assert_eq!(second.decision, GateDecision::Allow);
    let third = process.check(check_frame("check-3")).await.expect("stable");
    assert_eq!(third.decision, GateDecision::Allow);
}

#[tokio::test]
async fn two_consecutive_crashes_disable_the_gate_with_the_recorded_class() {
    let mut process = GateProcess::new(spec("crash"));
    process.describe().await.expect("describe");
    process.start(init_frame()).await.expect("init");
    assert!(process.check(check_frame("check-1")).await.is_err());
    assert!(process.check(check_frame("check-2")).await.is_err());
    match process.check(check_frame("check-3")).await {
        Err(GateCheckFailure::Disabled { failure }) => {
            assert_eq!(failure.class, GateErrorClass::Crash);
        },
        other => panic!("expected a disabled gate, got {other:?}"),
    }
}

#[tokio::test]
async fn per_check_timeout_kills_the_process_and_fails_closed() {
    let mut environment = BTreeMap::new();
    environment.insert("FAKE_GATE_MODE".to_string(), "delayed".to_string());
    environment.insert("FAKE_GATE_DELAY".to_string(), "5".to_string());
    let mut process = GateProcess::new(
        GateProcessSpec::new(fixture())
            .with_environment(environment)
            .with_timeouts(Duration::from_secs(2), Duration::from_millis(100)),
    );
    process.describe().await.expect("describe");
    process.start(init_frame()).await.expect("init");
    let failure = process.check(check_frame("check-1")).await;
    assert!(matches!(
        failure,
        Err(GateCheckFailure::Failure(GateFailure {
            class: GateErrorClass::Timeout,
            ..
        }))
    ));
}

#[tokio::test]
async fn oversized_frame_is_a_protocol_failure() {
    let mut process = GateProcess::new(spec("oversized"));
    process.describe().await.expect("describe");
    process.start(init_frame()).await.expect("init");
    let failure = process.check(check_frame("check-1")).await;
    assert!(matches!(
        failure,
        Err(GateCheckFailure::Failure(GateFailure {
            class: GateErrorClass::Protocol,
            ..
        }))
    ));
}

#[tokio::test]
async fn oversized_init_config_is_rejected_before_spawn() {
    let mut process = GateProcess::new(spec("allow"));
    process.describe().await.expect("describe");
    let init = init_frame().with_config(serde_json::json!({
        "padding": "x".repeat(65 * 1024)
    }));
    let failure = process.start(init).await.expect_err("config too large");
    assert_eq!(failure.class, GateErrorClass::Config);
}

#[tokio::test]
async fn startup_describe_and_init_failures_are_reported_before_checks() {
    let mut process = GateProcess::new(spec("bad-describe"));
    let failure = process.describe().await.expect_err("bad describe");
    assert_eq!(failure.class, GateErrorClass::Config);

    let mut process = GateProcess::new(spec("init-error"));
    process.describe().await.expect("describe");
    let failure = process.start(init_frame()).await.expect_err("init error");
    assert_eq!(failure.class, GateErrorClass::Config);
}

#[tokio::test]
async fn process_gate_runs_through_the_composer_with_fail_open() {
    let process = GateProcess::new(spec("crash"));
    let gate = ProcessGate::start(init_frame(), process)
        .await
        .expect("process gate starts");
    let config = GateStageConfig {
        fail: FailMode::Open,
        ..GateStageConfig::enforce("fake-safety")
    };
    let pipeline = GatePipeline::start(vec![GateStartup::ready(
        config,
        Arc::new(gate),
        Duration::ZERO,
    )])
    .expect("pipeline starts");
    let first = pipeline.check(request()).await.expect("fail-open check");
    assert!(matches!(first.authorization, PipelineAuthorization::Run));
    assert!(first.stages[0].failed_open);
    let second = pipeline.check(request()).await.expect("second fail-open");
    assert!(second.stages[0].failed_open);
    let third = pipeline.check(request()).await.expect("disabled fail-open");
    assert!(third.stages[0].disabled);
    assert!(third.stages[0].failed_open);
    assert_eq!(third.stages[0].latency, Duration::ZERO);
}

#[tokio::test]
async fn process_gate_closed_failure_is_a_pipeline_error() {
    let process = GateProcess::new(spec("crash"));
    let gate = ProcessGate::start(init_frame(), process)
        .await
        .expect("process gate starts");
    let pipeline = GatePipeline::start(vec![GateStartup::ready(
        GateStageConfig::enforce("fake-safety"),
        Arc::new(gate),
        Duration::ZERO,
    )])
    .expect("pipeline starts");
    let error = pipeline.check(request()).await.expect_err("closed failure");
    assert_eq!(error.class(), GateErrorClass::Crash);
}

#[tokio::test]
async fn shadow_process_gate_observes_without_authority() {
    let enforce_process = GateProcess::new(spec("allow"));
    let enforce = ProcessGate::start(init_frame(), enforce_process)
        .await
        .expect("enforce starts");
    let shadow_process = GateProcess::new(spec("block"));
    let shadow = ProcessGate::start(init_frame(), shadow_process)
        .await
        .expect("shadow starts");
    let pipeline = GatePipeline::start(vec![
        GateStartup::ready(
            GateStageConfig::enforce("safety"),
            Arc::new(enforce),
            Duration::ZERO,
        ),
        GateStartup::ready(
            GateStageConfig::shadow("observer"),
            Arc::new(shadow),
            Duration::ZERO,
        ),
    ])
    .expect("pipeline starts");
    let outcome = pipeline.check(request()).await.expect("shadow check");
    assert!(matches!(outcome.authorization, PipelineAuthorization::Run));
    assert_eq!(outcome.stages.len(), 2);
    assert_eq!(outcome.stages[0].decision, Some(GateDecision::Allow));
    assert_eq!(outcome.stages[1].decision, Some(GateDecision::Block));
}
