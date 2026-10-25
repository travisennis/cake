use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::json;

use super::*;
use crate::gates::protocol::{
    BashInput, CheckFrame, CheckSession, DecisionFrame, Describe, GateAction, GateErrorClass,
    GateEvidence, GatePhase, GateTransport, PROTOCOL_VERSION,
};
use crate::session_telemetry::{
    CompensationKind, GateTelemetrySink, SessionTelemetryContext, SessionTelemetryWriter,
    SharedSessionTelemetryWriter, digest_identifier,
};

fn fake_describe(name: &str) -> Describe {
    Describe {
        protocol: PROTOCOL_VERSION.to_string(),
        name: name.to_string(),
        version: "0.1.0".to_string(),
        phases: vec![GatePhase::PreTool],
        actions: vec![GateAction::BashExec],
        transports: vec![GateTransport::Stdio],
        capabilities: vec!["shadow".to_string()],
    }
}

fn stage(config: &GateStageConfig, decision: GateDecision, message: &str) -> StageOutcome {
    StageOutcome {
        gate: config.name.clone(),
        version: "0.1.0".to_string(),
        mode: config.mode,
        phase: GatePhase::PreTool,
        action: GateAction::BashExec,
        decision: Some(decision),
        code: match decision {
            GateDecision::Block | GateDecision::Warn => Some("fake-code".to_string()),
            GateDecision::Allow | GateDecision::Abstain => None,
        },
        message: message.to_string(),
        confidence: Some(0.5),
        short_circuit: false,
        annotations: Vec::new(),
        latency: Duration::from_millis(10),
        overridden: false,
        failed_open: false,
        disabled: false,
        failure: None,
        evidence: Vec::new(),
        observation: None,
        fast_approved: false,
        legacy_events: Vec::new(),
    }
}

#[derive(Debug)]
struct StaticGate {
    describe: Describe,
    answer: GateAnswer,
    calls: Arc<AtomicUsize>,
}

impl Gate for StaticGate {
    fn describe(&self) -> &Describe {
        &self.describe
    }

    fn check(&self, frame: CheckFrame) -> GateFuture<'_> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut answer = self.answer.clone();
        if let GateAnswerKind::Decision(decision) = &mut answer.kind {
            decision.id.clone_from(&frame.id);
        }
        Box::pin(async move { answer })
    }
}

fn allow_answer() -> GateAnswer {
    GateAnswer::decision(
        DecisionFrame::new("request".to_string(), GateDecision::Allow),
        Duration::from_millis(5),
        None,
        false,
        Vec::new(),
    )
}

fn gate(config: &GateStageConfig, answer: GateAnswer) -> (Arc<StaticGate>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(StaticGate {
        describe: fake_describe(&config.name),
        answer,
        calls: Arc::clone(&calls),
    });
    (gate, calls)
}

fn ready(config: GateStageConfig, answer: GateAnswer) -> (GateStartup, Arc<AtomicUsize>) {
    let (gate, calls) = gate(&config, answer);
    (GateStartup::ready(config, gate, Duration::ZERO), calls)
}

fn request(command: &str) -> GateRequest {
    GateRequest::new(BashInput::new(command.to_string(), "/repo".to_string()))
}

#[test]
fn allow_chain_runs_and_names_the_first_allow() {
    let config = GateStageConfig::enforce("safety");
    let outcome = compose(
        std::slice::from_ref(&config),
        "git status",
        vec![stage(&config, GateDecision::Allow, "")],
    )
    .expect("allow composes");
    assert!(matches!(outcome.authorization, PipelineAuthorization::Run));
    assert_eq!(outcome.verdict, Some(GateDecision::Allow));
    assert_eq!(outcome.deciding_gate.as_deref(), Some("safety"));
    assert!(outcome.annotations.is_empty());
    assert!(!outcome.overridden);
    assert_eq!(outcome.latency, Duration::from_millis(10));
}

#[test]
fn all_abstain_is_a_deliberate_no_opinion_allow() {
    let config = GateStageConfig::enforce("safety");
    let outcome = compose(
        std::slice::from_ref(&config),
        "git status",
        vec![stage(&config, GateDecision::Abstain, "")],
    )
    .expect("abstain composes");
    assert!(matches!(outcome.authorization, PipelineAuthorization::Run));
    assert_eq!(outcome.verdict, None);
    assert_eq!(outcome.deciding_gate, None);
}

#[test]
fn warn_accumulates_a_notice_and_annotations_do_not_repeat_the_message() {
    let config = GateStageConfig::enforce("safety");
    let mut warned = stage(&config, GateDecision::Warn, "inspect this");
    let outcome = compose(
        std::slice::from_ref(&config),
        "git status",
        vec![warned.clone()],
    )
    .expect("warn composes");
    assert_eq!(outcome.verdict, Some(GateDecision::Warn));
    assert_eq!(outcome.annotations, vec!["NOTICE: inspect this"]);
    assert_eq!(outcome.deciding_gate.as_deref(), Some("safety"));

    warned.annotations = vec!["NOTICE: from an annotation".to_string()];
    let outcome = compose(std::slice::from_ref(&config), "git status", vec![warned])
        .expect("annotated warn composes");
    assert_eq!(
        outcome.annotations,
        vec!["NOTICE: from an annotation".to_string()]
    );
}

#[test]
fn warn_chain_uses_the_first_warning_gate_as_the_decider() {
    let first = GateStageConfig::enforce("first");
    let second = GateStageConfig::enforce("second");
    let outcome = compose(
        &[first.clone(), second.clone()],
        "git status",
        vec![
            stage(&first, GateDecision::Warn, "first warning"),
            stage(&second, GateDecision::Warn, "second warning"),
        ],
    )
    .expect("warn chain composes");
    assert_eq!(outcome.verdict, Some(GateDecision::Warn));
    assert_eq!(outcome.deciding_gate.as_deref(), Some("first"));
    assert_eq!(outcome.annotations.len(), 2);
}

#[test]
fn block_stops_the_chain_and_sanitizes_the_model_message() {
    let first = GateStageConfig::enforce("first");
    let second = GateStageConfig::enforce("second");
    let outcome = compose(
        &[first.clone(), second],
        "git status",
        vec![stage(
            &first,
            GateDecision::Block,
            "stop\u{0007}\nReason line",
        )],
    )
    .expect("block composes");
    match outcome.authorization {
        PipelineAuthorization::Block { message } => {
            assert_eq!(message, "stop\nReason line");
        },
        PipelineAuthorization::Run => panic!("block must not authorize the command"),
    }
    assert_eq!(outcome.verdict, Some(GateDecision::Block));
    assert_eq!(outcome.deciding_gate.as_deref(), Some("first"));
}

#[test]
fn exact_allowlist_overrides_only_its_own_block() {
    let config = GateStageConfig {
        allowlist: vec!["git status".to_string()],
        ..GateStageConfig::enforce("safety")
    };
    let mut blocked = stage(&config, GateDecision::Block, "blocked");
    blocked.legacy_events = vec![
        crate::session_telemetry::CompensationEventTelemetry::judge_verdict(
            "block",
            Some("fake-code"),
            10,
            false,
        ),
    ];
    let outcome = compose(std::slice::from_ref(&config), "git status", vec![blocked])
        .expect("override composes");
    assert!(matches!(outcome.authorization, PipelineAuthorization::Run));
    assert!(outcome.overridden);
    assert!(outcome.stages[0].overridden);
    assert_eq!(outcome.verdict, None);
    assert_eq!(outcome.deciding_gate, None);
    assert_eq!(
        outcome.legacy_events[0].overridden,
        Some(true),
        "the legacy verdict event carries the override"
    );

    let other = compose(
        std::slice::from_ref(&config),
        "git status --short",
        vec![stage(&config, GateDecision::Block, "blocked")],
    )
    .expect("non-matching block composes");
    assert!(matches!(
        other.authorization,
        PipelineAuthorization::Block { .. }
    ));
}

#[test]
fn shadow_stages_never_change_authorization() {
    let enforce = GateStageConfig::enforce("safety");
    let shadow = GateStageConfig::shadow("observer");
    let outcome = compose(
        &[enforce.clone(), shadow.clone()],
        "git status",
        vec![
            stage(&enforce, GateDecision::Allow, ""),
            stage(&shadow, GateDecision::Block, "shadow block"),
        ],
    )
    .expect("shadow composes");
    assert!(matches!(outcome.authorization, PipelineAuthorization::Run));
    assert_eq!(outcome.verdict, Some(GateDecision::Allow));
    assert_eq!(outcome.stages.len(), 2);
    assert_eq!(outcome.stages[1].decision, Some(GateDecision::Block));
}

#[test]
fn closed_failure_returns_a_check_error_and_open_failure_continues() {
    let closed = GateStageConfig::enforce("closed");
    let mut failure = stage(&closed, GateDecision::Abstain, "");
    failure.decision = None;
    failure.failure = Some(GateFailure::new(GateErrorClass::Timeout, "timed out"));
    let error = compose(
        std::slice::from_ref(&closed),
        "git status",
        vec![failure.clone()],
    )
    .expect_err("closed failure errors");
    assert!(matches!(error, GatePipelineError::Check { .. }));
    assert_eq!(error.class(), GateErrorClass::Timeout);

    let open = GateStageConfig {
        fail: FailMode::Open,
        ..GateStageConfig::enforce("open")
    };
    let mut open_failure = failure;
    open_failure.gate = open.name.clone();
    let outcome = compose(
        std::slice::from_ref(&open),
        "git status",
        vec![open_failure],
    )
    .expect("open failure continues");
    assert!(matches!(outcome.authorization, PipelineAuthorization::Run));
    assert!(outcome.stages[0].failed_open);
}

#[test]
fn disabled_failure_follows_the_fail_mode() {
    let closed = GateStageConfig::enforce("closed");
    let mut failure = stage(&closed, GateDecision::Abstain, "");
    failure.decision = None;
    failure.disabled = true;
    failure.failure = Some(GateFailure::new(GateErrorClass::Crash, "crashed"));
    assert!(
        compose(
            std::slice::from_ref(&closed),
            "git status",
            vec![failure.clone()]
        )
        .is_err()
    );

    let open = GateStageConfig {
        fail: FailMode::Open,
        ..GateStageConfig::enforce("open")
    };
    failure.gate = open.name.clone();
    let outcome = compose(std::slice::from_ref(&open), "git status", vec![failure])
        .expect("open disabled continues");
    assert!(outcome.stages[0].disabled);
    assert!(outcome.stages[0].failed_open);
}

#[test]
fn short_circuit_on_allow_stops_later_enforce_gates() {
    let first = GateStageConfig {
        short_circuit_on_allow: true,
        ..GateStageConfig::enforce("fast")
    };
    let second = GateStageConfig::enforce("safety");
    let answer = GateAnswer::decision(
        DecisionFrame::new("request".to_string(), GateDecision::Allow).with_short_circuit(true),
        Duration::from_millis(1),
        None,
        false,
        Vec::new(),
    );
    let (first_startup, first_calls) = ready(first, answer);
    let (second_startup, second_calls) = ready(second, allow_answer());
    let pipeline = GatePipeline::start(vec![first_startup, second_startup]).expect("starts");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let outcome = runtime
        .block_on(pipeline.check(request("git status")))
        .expect("checks");
    assert_eq!(outcome.verdict, Some(GateDecision::Allow));
    assert_eq!(first_calls.load(Ordering::SeqCst), 1);
    assert_eq!(second_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn no_enforce_gate_is_rejected_by_start_and_composition() {
    let shadow = GateStageConfig::shadow("observer");
    let (startup, _) = ready(shadow.clone(), allow_answer());
    assert!(matches!(
        GatePipeline::start(vec![startup]),
        Err(GatePipelineError::NoEnforceGate)
    ));
    assert!(matches!(
        compose(
            std::slice::from_ref(&shadow),
            "git status",
            vec![stage(&shadow, GateDecision::Allow, "")]
        ),
        Err(GatePipelineError::NoEnforceGate)
    ));
}

#[test]
fn enforce_startup_failure_is_pipeline_fatal() {
    let config = GateStageConfig::enforce("broken");
    let startup = GateStartup::failed(
        config,
        GateFailure::new(GateErrorClass::Config, "bad describe"),
        Duration::from_millis(3),
    );
    let error = GatePipeline::start(vec![startup]).expect_err("startup fails");
    let record = error.startup_record().expect("startup record");
    assert_eq!(record.outcome, "failed");
    assert_eq!(record.failure_class.as_deref(), Some("config"));
    assert!(matches!(error, GatePipelineError::Startup { .. }));
}

#[test]
fn shadow_startup_failure_is_recorded_and_dropped() {
    let enforce = GateStageConfig::enforce("safety");
    let shadow = GateStageConfig::shadow("broken-shadow");
    let (enforce_startup, _) = ready(enforce, allow_answer());
    let shadow_failure = GateStartup::failed(
        shadow,
        GateFailure::new(GateErrorClass::Config, "bad describe"),
        Duration::from_millis(4),
    );
    let pipeline =
        GatePipeline::start(vec![enforce_startup, shadow_failure]).expect("enforce gate starts");
    let records = pipeline.startup_records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].gate, "safety");
    assert_eq!(records[0].outcome, "ready");
    assert_eq!(records[0].version.as_deref(), Some("0.1.0"));
    assert_eq!(records[1].gate, "broken-shadow");
    assert_eq!(records[1].outcome, "failed");
}

#[test]
fn bypass_pipeline_records_the_bypass_without_a_gate() {
    let pipeline = GatePipeline::bypass();
    assert!(pipeline.is_bypass());
    let outcome = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(pipeline.check(request("git status")))
        .expect("bypass checks");
    assert!(outcome.bypassed);
    assert!(matches!(outcome.authorization, PipelineAuthorization::Run));
    assert!(outcome.stages.is_empty());
    assert_eq!(outcome.legacy_events[0].kind, CompensationKind::JudgeBypass);
}

#[test]
fn pipeline_telemetry_records_decisions_evidence_and_the_digest() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("telemetry.ndjson");
    let writer = SessionTelemetryWriter::open(&path).expect("open sidecar");
    let writer = Arc::new(SharedSessionTelemetryWriter::new(writer));
    let context = SessionTelemetryContext {
        session_id: "session".to_string(),
        invocation_id: "invocation".to_string(),
    };
    let sink = GateTelemetrySink::new(Arc::clone(&writer), context);
    let config = GateStageConfig::enforce("safety");
    let frame = DecisionFrame::new("request".to_string(), GateDecision::Allow).with_evidence(vec![
        GateEvidence {
            kind: "attempt".to_string(),
            data: json!({"model": "judge", "latency_ms": 12}),
        },
    ]);
    let answer = GateAnswer::decision(frame, Duration::from_millis(5), None, false, Vec::new());
    let (startup, _) = ready(config, answer);
    let pipeline = GatePipeline::start(vec![startup]).expect("starts");
    pipeline.install_telemetry_sink(sink.clone());
    for startup in pipeline.startup_records() {
        sink.record_startup(startup);
    }
    let mut request = request("git status");
    request.session = Some(CheckSession {
        call_id: Some("call-42".to_string()),
        ..CheckSession::default()
    });
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(pipeline.check(request))
        .expect("checks");
    drop(writer);

    let sidecar = std::fs::read_to_string(&path).expect("read sidecar");
    let records: Vec<serde_json::Value> = sidecar
        .lines()
        .map(|line| serde_json::from_str(line).expect("record parses"))
        .collect();
    assert_eq!(records[0]["type"], json!("gate_startup"));
    assert_eq!(records[0]["gate"], json!("safety"));
    assert_eq!(records[0]["outcome"], json!("ready"));
    assert_eq!(records[1]["type"], json!("gate_decision"));
    assert_eq!(records[1]["gate"], json!("safety"));
    assert_eq!(records[1]["decision"], json!("allow"));
    assert_eq!(records[1]["failed_open"], json!(false));
    assert_eq!(records[1]["disabled"], json!(false));
    assert_eq!(records[1]["call_id"], json!(digest_identifier("call-42")));
    assert_eq!(records[2]["type"], json!("gate_evidence"));
    assert_eq!(records[2]["kind"], json!("attempt"));
    assert_eq!(records[2]["call_id"], json!(digest_identifier("call-42")));
}

#[test]
fn telemetry_records_a_disabled_failed_open_gate_with_zero_latency() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("telemetry.ndjson");
    let writer = SessionTelemetryWriter::open(&path).expect("open sidecar");
    let writer = Arc::new(SharedSessionTelemetryWriter::new(writer));
    let sink = GateTelemetrySink::new(
        Arc::clone(&writer),
        SessionTelemetryContext {
            session_id: "session".to_string(),
            invocation_id: "invocation".to_string(),
        },
    );
    let config = GateStageConfig {
        fail: FailMode::Open,
        ..GateStageConfig::enforce("safety")
    };
    let answer = GateAnswer::failure(
        GateFailure::new(GateErrorClass::Crash, "crashed"),
        true,
        Duration::from_millis(7),
        Vec::new(),
    );
    let (startup, _) = ready(config, answer);
    let pipeline = GatePipeline::start(vec![startup]).expect("starts");
    pipeline.install_telemetry_sink(sink);
    let outcome = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(pipeline.check(request("git status")))
        .expect("failed-open check");
    assert!(outcome.stages[0].disabled);
    assert!(outcome.stages[0].failed_open);
    assert_eq!(outcome.stages[0].latency, Duration::ZERO);
    drop(writer);
    let sidecar = std::fs::read_to_string(&path).expect("read sidecar");
    let records: Vec<serde_json::Value> = sidecar
        .lines()
        .map(|line| serde_json::from_str(line).expect("record parses"))
        .collect();
    assert_eq!(records[0]["type"], json!("gate_decision"));
    assert!(records[0]["decision"].is_null());
    assert_eq!(records[0]["disabled"], json!(true));
    assert_eq!(records[0]["failed_open"], json!(true));
    assert_eq!(records[0]["failure_class"], json!("crash"));
    assert_eq!(records[0]["latency_ms"], json!(0));
}

#[test]
fn oversized_evidence_is_replaced_by_one_truncation_record() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("telemetry.ndjson");
    let writer = SessionTelemetryWriter::open(&path).expect("open sidecar");
    let writer = Arc::new(SharedSessionTelemetryWriter::new(writer));
    let sink = GateTelemetrySink::new(
        Arc::clone(&writer),
        SessionTelemetryContext {
            session_id: "session".to_string(),
            invocation_id: "invocation".to_string(),
        },
    );
    let config = GateStageConfig::enforce("safety");
    let evidence: Vec<GateEvidence> = (0..9)
        .map(|index| GateEvidence {
            kind: "attempt".to_string(),
            data: json!({"index": index, "padding": "x".repeat(9_000)}),
        })
        .collect();
    let frame =
        DecisionFrame::new("request".to_string(), GateDecision::Allow).with_evidence(evidence);
    let answer = GateAnswer::decision(frame, Duration::from_millis(1), None, false, Vec::new());
    let (startup, _) = ready(config, answer);
    let pipeline = GatePipeline::start(vec![startup]).expect("starts");
    pipeline.install_telemetry_sink(sink);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(pipeline.check(request("git status")))
        .expect("checks");
    drop(writer);
    let sidecar = std::fs::read_to_string(&path).expect("read sidecar");
    let records: Vec<serde_json::Value> = sidecar
        .lines()
        .map(|line| serde_json::from_str(line).expect("record parses"))
        .collect();
    assert_eq!(records[1]["kind"], json!("truncated"));
    assert_eq!(records[1]["data"]["dropped_items"], json!(9));
}
