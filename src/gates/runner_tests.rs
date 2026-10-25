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

fn spec_with_descendant(mode: &str, pid_path: &std::path::Path) -> GateProcessSpec {
    let mut spec = spec(mode);
    spec.environment.insert(
        "FAKE_GATE_DESCENDANT_PID".to_string(),
        pid_path.display().to_string(),
    );
    spec
}

#[cfg(unix)]
fn process_exists(pid: i32) -> bool {
    // SAFETY: signal 0 performs existence and permission checks without
    // sending a signal.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(unix)]
async fn wait_for_pid_file(path: &std::path::Path) -> i32 {
    for _ in 0..100 {
        if let Ok(text) = std::fs::read_to_string(path)
            && let Ok(pid) = text.trim().parse()
        {
            return pid;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("fake gate did not write its descendant pid");
}

#[cfg(unix)]
async fn wait_for_process_exit(pid: i32) {
    for _ in 0..100 {
        if !process_exists(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("gate descendant {pid} survived process-group cleanup");
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

#[cfg(unix)]
#[tokio::test]
async fn shutdown_cleans_descendants_after_the_direct_child_exits() {
    let dir = tempfile::tempdir().expect("descendant dir");
    let pid_path = dir.path().join("descendant.pid");
    let mut process = GateProcess::new(spec_with_descendant("allow", &pid_path));
    process.describe().await.expect("describe");
    process.start(init_frame()).await.expect("init");
    process.check(check_frame("check-1")).await.expect("check");
    let pid = wait_for_pid_file(&pid_path).await;
    assert!(process_exists(pid), "descendant should be running");
    process.shutdown().await;
    wait_for_process_exit(pid).await;
}

#[cfg(unix)]
#[tokio::test]
async fn describe_failure_cleans_descendants() {
    let dir = tempfile::tempdir().expect("descendant dir");
    let pid_path = dir.path().join("descendant.pid");
    let mut process = GateProcess::new(spec_with_descendant("bad-describe", &pid_path));
    let failure = process.describe().await.expect_err("bad describe");
    assert_eq!(failure.class, GateErrorClass::Config);
    let pid = wait_for_pid_file(&pid_path).await;
    wait_for_process_exit(pid).await;
}

#[tokio::test]
async fn valid_check_error_frame_is_not_a_process_failure() {
    let mut process = GateProcess::new(spec("check-error"));
    process.describe().await.expect("describe");
    process.start(init_frame()).await.expect("init");
    for attempt in 0..3 {
        let failure = process
            .check(check_frame(&format!("check-{attempt}")))
            .await
            .expect_err("gate-reported error");
        match failure {
            GateCheckFailure::Failure(failure) => {
                assert_eq!(failure.class, GateErrorClass::Config);
            },
            GateCheckFailure::Disabled { .. } => {
                panic!("a valid error frame must not trip the process breaker");
            },
        }
    }
}

#[tokio::test]
async fn bad_protocol_check_error_is_a_process_failure_and_trips_the_breaker() {
    let mut process = GateProcess::new(spec("check-error-bad-protocol"));
    process.describe().await.expect("describe");
    process.start(init_frame()).await.expect("init");
    for attempt in 0..2 {
        let failure = process
            .check(check_frame(&format!("check-{attempt}")))
            .await
            .expect_err("bad protocol error");
        match failure {
            GateCheckFailure::Failure(failure) => {
                assert_eq!(failure.class, GateErrorClass::Protocol);
            },
            GateCheckFailure::Disabled { .. } => panic!("unexpected early disable"),
        }
    }
    assert!(matches!(
        process.check(check_frame("check-3")).await,
        Err(GateCheckFailure::Disabled { .. })
    ));
}

#[tokio::test]
async fn bad_protocol_init_error_is_a_startup_protocol_failure() {
    let mut process = GateProcess::new(spec("init-error-bad-protocol"));
    process.describe().await.expect("describe");
    let failure = process
        .start(init_frame())
        .await
        .expect_err("bad init protocol");
    assert_eq!(failure.class, GateErrorClass::Protocol);
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
