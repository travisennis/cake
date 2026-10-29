//! Ordered composition for command-policy gates.
//!
//! The composer is the pure part of the seam: it turns ordered stage results
//! into one authorization, the model-visible annotations, and the stage records
//! telemetry needs. [`GatePipeline`] adds execution, telemetry, and the
//! no-enforce-gate rule around the same decisions.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::gates::protocol::{
    BashInput, CheckFrame, CheckSession, DecisionFrame, Describe, GateAction, GateAnnotation,
    GateDecision, GateErrorClass, GateEvidence, GatePhase, MAX_ANNOTATIONS,
    MAX_EVIDENCE_ITEM_BYTES, MAX_EVIDENCE_ITEMS, MAX_EVIDENCE_TOTAL_BYTES, bound_error_message,
    is_valid_evidence_kind, json_byte_len, sanitize_model_text,
};
use crate::session_telemetry::{
    GateDecisionTelemetry, GateEvidenceTelemetry, GateStartupTelemetry, GateTelemetrySink,
};

/// Whether a gate can change authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GateMode {
    Enforce,
    Shadow,
}

impl GateMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enforce => "enforce",
            Self::Shadow => "shadow",
        }
    }
}

/// How a per-check failure is treated. Startup failures always fail closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailMode {
    Closed,
    Open,
}

/// One gate's resolved composition settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateStageConfig {
    /// The local instance name. Composition and telemetry use this name, which
    /// may differ from `describe.name`.
    pub name: String,
    pub mode: GateMode,
    pub fail: FailMode,
    pub short_circuit_on_allow: bool,
    /// Exact `input.command` strings whose block this gate overrides.
    pub allowlist: Vec<String>,
}

impl GateStageConfig {
    pub fn enforce(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            mode: GateMode::Enforce,
            fail: FailMode::Closed,
            short_circuit_on_allow: false,
            allowlist: Vec::new(),
        }
    }

    pub fn shadow(name: impl Into<String>) -> Self {
        Self {
            mode: GateMode::Shadow,
            ..Self::enforce(name)
        }
    }
}

/// A startup failure or a gate that is ready to serve checks.
#[derive(Debug)]
pub struct GateStartup {
    pub config: GateStageConfig,
    pub gate: Result<Arc<dyn Gate>, GateFailure>,
    pub latency: Duration,
}

impl GateStartup {
    pub fn ready(config: GateStageConfig, gate: Arc<dyn Gate>, latency: Duration) -> Self {
        Self {
            config,
            gate: Ok(gate),
            latency,
        }
    }

    pub fn failed(config: GateStageConfig, failure: GateFailure, latency: Duration) -> Self {
        Self {
            config,
            gate: Err(failure),
            latency,
        }
    }
}

/// A bounded failure from a gate or from Cake's process management.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateFailure {
    pub class: GateErrorClass,
    pub message: String,
}

impl GateFailure {
    pub fn new(class: GateErrorClass, message: impl Into<String>) -> Self {
        Self {
            class,
            message: bound_error_message(&message.into()),
        }
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::new(GateErrorClass::Config, message)
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self::new(GateErrorClass::Protocol, message)
    }

    pub fn crash(message: impl Into<String>) -> Self {
        Self::new(GateErrorClass::Crash, message)
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(GateErrorClass::Timeout, message)
    }
}

impl std::fmt::Display for GateFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.class, self.message)
    }
}

impl std::error::Error for GateFailure {}

/// What a gate answered for one check.
#[derive(Debug, Clone)]
pub struct GateAnswer {
    pub kind: GateAnswerKind,
    pub latency: Duration,
}

impl GateAnswer {
    pub const fn decision(frame: DecisionFrame, latency: Duration) -> Self {
        Self {
            kind: GateAnswerKind::Decision(frame),
            latency,
        }
    }

    pub const fn bypassed(latency: Duration) -> Self {
        Self {
            kind: GateAnswerKind::Bypassed,
            latency,
        }
    }

    pub const fn failure(failure: GateFailure, disabled: bool, latency: Duration) -> Self {
        Self {
            kind: GateAnswerKind::Failure { failure, disabled },
            latency,
        }
    }
}

/// A gate answer before composition applies its policy.
#[derive(Debug, Clone)]
pub enum GateAnswerKind {
    Decision(DecisionFrame),
    Bypassed,
    Failure {
        failure: GateFailure,
        disabled: bool,
    },
}

/// The interface the pipeline uses for both the in-process adapter and a
/// process-backed gate.
pub trait Gate: Send + Sync + std::fmt::Debug {
    fn describe(&self) -> &Describe;

    fn check(&self, frame: CheckFrame) -> GateFuture<'_>;

    /// Whether this gate is the global bypass rather than a checking gate.
    fn is_bypassed(&self) -> bool {
        false
    }

    fn shutdown(&self) -> GateShutdownFuture<'_> {
        Box::pin(async {})
    }
}

pub type GateFuture<'a> = Pin<Box<dyn Future<Output = GateAnswer> + Send + 'a>>;
pub type GateShutdownFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// One gate in pipeline order.
struct GateStage {
    config: GateStageConfig,
    gate: Arc<dyn Gate>,
    startup_latency: Duration,
}

impl std::fmt::Debug for GateStage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GateStage")
            .field("config", &self.config)
            .field("describe", self.gate.describe())
            .finish_non_exhaustive()
    }
}

/// The ordered gate pipeline for one run.
pub struct GatePipeline {
    bypass: bool,
    stages: Vec<GateStage>,
    startup_failures: Vec<GateStartupTelemetry>,
    telemetry: OnceLock<GateTelemetrySink>,
}

impl std::fmt::Debug for GatePipeline {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GatePipeline")
            .field("bypass", &self.bypass)
            .field("stages", &self.stages)
            .field("startup_failures", &self.startup_failures)
            .finish_non_exhaustive()
    }
}

/// One `pre_tool` `bash.exec` check request.
#[derive(Debug, Clone)]
pub struct GateRequest {
    pub input: BashInput,
    pub session: Option<CheckSession>,
    pub diagnostic: bool,
}

impl GateRequest {
    pub const fn new(input: BashInput) -> Self {
        Self {
            input,
            session: None,
            diagnostic: false,
        }
    }

    pub fn with_session(mut self, session: CheckSession) -> Self {
        self.session = Some(session);
        self
    }

    pub const fn with_diagnostic(mut self, diagnostic: bool) -> Self {
        self.diagnostic = diagnostic;
        self
    }
}

/// The final authorization from a successful pipeline run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineAuthorization {
    Run,
    Block { message: String },
}

/// One executed stage, as composition saw it.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the flags mirror the independent telemetry fields; each can be true or false on
              its own"
)]
#[derive(Debug, Clone)]
pub struct StageOutcome {
    pub gate: String,
    pub version: String,
    pub mode: GateMode,
    pub phase: GatePhase,
    pub action: GateAction,
    pub decision: Option<GateDecision>,
    pub code: Option<String>,
    pub message: String,
    pub confidence: Option<f32>,
    pub short_circuit: bool,
    pub annotations: Vec<String>,
    pub latency: Duration,
    pub overridden: bool,
    pub failed_open: bool,
    pub disabled: bool,
    pub failure: Option<GateFailure>,
    pub evidence: Vec<GateEvidence>,
}

impl StageOutcome {
    fn decision_record(
        &self,
        phase: GatePhase,
        action: GateAction,
        raw_call_id: Option<&str>,
    ) -> GateDecisionTelemetry {
        GateDecisionTelemetry {
            gate: Some(self.gate.clone()),
            version: Some(self.version.clone()),
            mode: self.mode.as_str().to_string(),
            phase: phase.as_str().to_string(),
            action: action.as_str().to_string(),
            decision: self.decision.map(|decision| decision.as_str().to_string()),
            code: self.code.clone(),
            latency_ms: duration_millis(self.latency),
            overridden: self.overridden,
            bypassed: false,
            failed_open: self.failed_open,
            disabled: self.disabled,
            failure_class: self
                .failure
                .as_ref()
                .map(|failure| failure.class.to_string()),
            call_id: raw_call_id.map(str::to_string),
        }
    }
}

/// The pipeline's successful result.
#[derive(Debug, Clone)]
pub struct PipelineOutcome {
    pub bypassed: bool,
    pub authorization: PipelineAuthorization,
    /// The effective pipeline decision: block, warn, the first allow, or none
    /// when every enforce gate abstained or only an overridden block remained.
    pub verdict: Option<GateDecision>,
    pub annotations: Vec<String>,
    pub overridden: bool,
    pub latency: Duration,
    pub stages: Vec<StageOutcome>,
    pub deciding_gate: Option<String>,
}

/// Why a pipeline could not produce a decision.
#[derive(Debug, thiserror::Error)]
pub enum GatePipelineError {
    #[error("no enforce gate covers pre_tool + bash.exec")]
    NoEnforceGate,
    #[error("gate '{gate}' failed to start: {failure}")]
    Startup {
        gate: String,
        failure: GateFailure,
        startup: Box<GateStartupTelemetry>,
    },
    #[error("gate '{gate}' failed: {failure}")]
    Check {
        gate: String,
        failure: GateFailure,
        stages: Vec<StageOutcome>,
    },
}

impl GatePipelineError {
    pub fn class(&self) -> GateErrorClass {
        match self {
            Self::NoEnforceGate => GateErrorClass::Config,
            Self::Startup { failure, .. } | Self::Check { failure, .. } => failure.class.clone(),
        }
    }

    /// The startup record this failure carries, when it is a startup failure.
    pub const fn startup_record(&self) -> Option<&GateStartupTelemetry> {
        match self {
            Self::Startup { startup, .. } => Some(startup),
            Self::NoEnforceGate | Self::Check { .. } => None,
        }
    }
}

impl GatePipeline {
    /// Build a pipeline from gate startup results.
    ///
    /// An enforce startup failure is pipeline-fatal. A shadow startup failure is
    /// recorded as a failed `gate_startup` record and the gate is dropped, which
    /// leaves authorization untouched.
    pub fn start(startups: Vec<GateStartup>) -> Result<Self, GatePipelineError> {
        let mut stages = Vec::new();
        let mut startup_failures = Vec::new();
        for startup in startups {
            let latency_ms = duration_millis(startup.latency);
            match startup.gate {
                Ok(gate) => {
                    let describe = gate.describe();
                    let validation = validate_stage(&startup.config, describe);
                    match validation {
                        Ok(()) => stages.push(GateStage {
                            config: startup.config,
                            gate,
                            startup_latency: startup.latency,
                        }),
                        Err(failure) => {
                            let record = startup_record(
                                &startup.config,
                                Some(describe),
                                "failed",
                                Some(&failure),
                                latency_ms,
                            );
                            if startup.config.mode == GateMode::Enforce {
                                return Err(GatePipelineError::Startup {
                                    gate: startup.config.name,
                                    failure,
                                    startup: Box::new(record),
                                });
                            }
                            startup_failures.push(record);
                        },
                    }
                },
                Err(failure) => {
                    let record =
                        startup_record(&startup.config, None, "failed", Some(&failure), latency_ms);
                    if startup.config.mode == GateMode::Enforce {
                        return Err(GatePipelineError::Startup {
                            gate: startup.config.name,
                            failure,
                            startup: Box::new(record),
                        });
                    }
                    startup_failures.push(record);
                },
            }
        }
        if !stages
            .iter()
            .any(|stage| stage.config.mode == GateMode::Enforce)
        {
            return Err(GatePipelineError::NoEnforceGate);
        }
        Ok(Self {
            bypass: false,
            stages,
            startup_failures,
            telemetry: OnceLock::new(),
        })
    }

    /// Build a single-gate pipeline.
    pub fn single(startup: GateStartup) -> Result<Self, GatePipelineError> {
        Self::start(vec![startup])
    }

    /// An explicit global bypass: no gate runs, and every check records one
    /// bypass decision.
    pub const fn bypass() -> Self {
        Self {
            bypass: true,
            stages: Vec::new(),
            startup_failures: Vec::new(),
            telemetry: OnceLock::new(),
        }
    }

    pub const fn is_bypass(&self) -> bool {
        self.bypass
    }

    /// Whether every check is the explicit global bypass, so Cake can skip
    /// script collection before calling the pipeline.
    pub fn is_bypassed(&self) -> bool {
        self.bypass || self.stages.iter().any(|stage| stage.gate.is_bypassed())
    }

    /// Install the session telemetry sink. Only the first installation wins.
    pub fn install_telemetry_sink(&self, sink: GateTelemetrySink) {
        drop(self.telemetry.set(sink));
    }

    /// One `gate_startup` record per gate, ready or failed.
    pub fn startup_records(&self) -> Vec<GateStartupTelemetry> {
        let mut records: Vec<_> = self
            .stages
            .iter()
            .map(|stage| {
                startup_record(
                    &stage.config,
                    Some(stage.gate.describe()),
                    "ready",
                    None,
                    duration_millis(stage.startup_latency),
                )
            })
            .collect();
        records.extend(self.startup_failures.iter().cloned());
        records
    }

    /// Run one check through the enforce chain and every shadow gate.
    pub async fn check(&self, request: GateRequest) -> Result<PipelineOutcome, GatePipelineError> {
        if self.bypass {
            return Ok(self.bypass_outcome(&request));
        }
        let raw_call_id = request
            .session
            .as_ref()
            .and_then(|session| session.call_id.as_deref());
        let mut composer = Composer::new(
            self.stages
                .iter()
                .map(|stage| stage.config.clone())
                .collect(),
            request.input.command.clone(),
        );
        for stage in &self.stages {
            if stage.config.mode == GateMode::Enforce && composer.enforce_stopped() {
                continue;
            }
            let request_id = uuid::Uuid::new_v4().to_string();
            let mut frame = CheckFrame::new(request_id.clone(), request.input.clone())
                .with_diagnostic(request.diagnostic);
            if let Some(session) = request.session.clone() {
                frame = frame.with_session(session);
            }
            let answer = stage.gate.check(frame).await;
            if matches!(answer.kind, GateAnswerKind::Bypassed) {
                return Ok(self.bypass_outcome(&request));
            }
            let outcome = stage_outcome(&stage.config, stage.gate.describe(), &request_id, answer);
            composer.record(&stage.config, outcome);
            if let Some(recorded) = composer.last_stage() {
                self.record_stage_telemetry(recorded, raw_call_id);
            }
        }
        composer.finish()
    }

    /// Best-effort shutdown of every gate process.
    pub async fn shutdown(&self) {
        for stage in &self.stages {
            stage.gate.shutdown().await;
        }
    }

    fn bypass_outcome(&self, request: &GateRequest) -> PipelineOutcome {
        let raw_call_id = request
            .session
            .as_ref()
            .and_then(|session| session.call_id.as_deref());
        if let Some(sink) = self.telemetry.get() {
            sink.record_decision(base_decision_record(
                None,
                None,
                false,
                true,
                None,
                raw_call_id,
            ));
        }
        PipelineOutcome {
            bypassed: true,
            authorization: PipelineAuthorization::Run,
            verdict: None,
            annotations: Vec::new(),
            overridden: false,
            latency: Duration::ZERO,
            stages: Vec::new(),
            deciding_gate: None,
        }
    }

    fn record_stage_telemetry(&self, stage: &StageOutcome, raw_call_id: Option<&str>) {
        let Some(sink) = self.telemetry.get() else {
            return;
        };
        sink.record_decision(stage.decision_record(stage.phase, stage.action, raw_call_id));
        for evidence in bounded_evidence(&stage.gate, &stage.evidence, raw_call_id) {
            sink.record_evidence(evidence);
        }
    }
}

fn validate_stage(config: &GateStageConfig, describe: &Describe) -> Result<(), GateFailure> {
    describe.validate().map_err(|message| {
        GateFailure::new(
            GateErrorClass::Config,
            format!("invalid describe: {message}"),
        )
    })?;
    if !describe.covers(GatePhase::PreTool, GateAction::BashExec) {
        return Err(GateFailure::new(
            GateErrorClass::Config,
            "describe does not declare pre_tool + bash.exec",
        ));
    }
    if config.mode == GateMode::Shadow && !describe.capabilities.iter().any(|cap| cap == "shadow") {
        return Err(GateFailure::new(
            GateErrorClass::Config,
            "a shadow gate must declare the 'shadow' capability",
        ));
    }
    Ok(())
}

fn startup_record(
    config: &GateStageConfig,
    describe: Option<&Describe>,
    outcome: &str,
    failure: Option<&GateFailure>,
    latency_ms: u64,
) -> GateStartupTelemetry {
    GateStartupTelemetry {
        gate: config.name.clone(),
        version: describe.map(|describe| describe.version.clone()),
        mode: config.mode.as_str().to_string(),
        phases: describe
            .map(|describe| {
                describe
                    .phases
                    .iter()
                    .map(|phase| phase.as_str().to_string())
                    .collect()
            })
            .unwrap_or_default(),
        actions: describe
            .map(|describe| {
                describe
                    .actions
                    .iter()
                    .map(|action| action.as_str().to_string())
                    .collect()
            })
            .unwrap_or_default(),
        outcome: outcome.to_string(),
        failure_class: failure.map(|failure| failure.class.to_string()),
        latency_ms,
    }
}

fn stage_outcome(
    config: &GateStageConfig,
    describe: &Describe,
    request_id: &str,
    answer: GateAnswer,
) -> StageOutcome {
    let disabled = matches!(&answer.kind, GateAnswerKind::Failure { disabled: true, .. });
    let base = StageOutcome {
        gate: config.name.clone(),
        version: describe.version.clone(),
        mode: config.mode,
        phase: GatePhase::PreTool,
        action: GateAction::BashExec,
        decision: None,
        code: None,
        message: String::new(),
        confidence: None,
        short_circuit: false,
        annotations: Vec::new(),
        latency: if disabled {
            Duration::ZERO
        } else {
            answer.latency
        },
        overridden: false,
        failed_open: false,
        disabled: false,
        failure: None,
        evidence: Vec::new(),
    };
    match answer.kind {
        GateAnswerKind::Decision(frame) => {
            let invalid = frame.validate(request_id).err();
            if let Some(message) = invalid {
                return StageOutcome {
                    failure: Some(GateFailure::new(GateErrorClass::Protocol, message)),
                    ..base
                };
            }
            StageOutcome {
                decision: Some(frame.decision),
                code: frame.code,
                message: frame.message,
                confidence: frame.confidence,
                short_circuit: frame.short_circuit,
                annotations: bounded_annotations(&frame.annotations),
                evidence: frame.evidence,
                ..base
            }
        },
        GateAnswerKind::Failure { failure, disabled } => StageOutcome {
            failure: Some(failure),
            disabled,
            ..base
        },
        GateAnswerKind::Bypassed => StageOutcome {
            failure: Some(GateFailure::new(
                GateErrorClass::Protocol,
                "a gate answered a bypass outside the pipeline bypass",
            )),
            ..base
        },
    }
}

fn bounded_annotations(annotations: &[GateAnnotation]) -> Vec<String> {
    annotations
        .iter()
        .take(MAX_ANNOTATIONS)
        .map(|annotation| sanitize_model_text(&annotation.text))
        .filter(|text| !text.is_empty())
        .collect()
}

fn bounded_evidence(
    gate: &str,
    evidence: &[GateEvidence],
    raw_call_id: Option<&str>,
) -> Vec<GateEvidenceTelemetry> {
    let mut records = Vec::new();
    let mut kept_bytes = 0_usize;
    let mut dropped_items = 0_usize;
    let mut dropped_bytes = 0_usize;
    for item in evidence {
        let bytes = json_byte_len(&item.data);
        let keep = records.len() < MAX_EVIDENCE_ITEMS
            && is_valid_evidence_kind(&item.kind)
            && bytes <= MAX_EVIDENCE_ITEM_BYTES
            && kept_bytes.saturating_add(bytes) <= MAX_EVIDENCE_TOTAL_BYTES;
        if keep {
            kept_bytes = kept_bytes.saturating_add(bytes);
            records.push(GateEvidenceTelemetry {
                gate: gate.to_string(),
                call_id: raw_call_id.map(str::to_string),
                kind: item.kind.clone(),
                data: item.data.clone(),
            });
        } else {
            dropped_items = dropped_items.saturating_add(1);
            dropped_bytes = dropped_bytes.saturating_add(bytes);
        }
    }
    if dropped_items > 0 {
        records.push(GateEvidenceTelemetry {
            gate: gate.to_string(),
            call_id: raw_call_id.map(str::to_string),
            kind: "truncated".to_string(),
            data: serde_json::json!({
                "dropped_items": dropped_items,
                "dropped_bytes": dropped_bytes,
            }),
        });
    }
    records
}

fn base_decision_record(
    gate: Option<String>,
    version: Option<String>,
    failed_open: bool,
    bypassed: bool,
    failure: Option<&GateFailure>,
    raw_call_id: Option<&str>,
) -> GateDecisionTelemetry {
    GateDecisionTelemetry {
        gate,
        version,
        mode: GateMode::Enforce.as_str().to_string(),
        phase: GatePhase::PreTool.as_str().to_string(),
        action: GateAction::BashExec.as_str().to_string(),
        decision: None,
        code: None,
        latency_ms: 0,
        overridden: false,
        bypassed,
        failed_open,
        disabled: false,
        failure_class: failure.map(|failure| failure.class.to_string()),
        call_id: raw_call_id.map(str::to_string),
    }
}

/// Internal composer state. It is intentionally free of I/O so the composition
/// matrix can be tested without spawning a gate.
struct Composer {
    configs: Vec<GateStageConfig>,
    command: String,
    stages: Vec<StageOutcome>,
    annotations: Vec<String>,
    first_warn: Option<usize>,
    first_allow: Option<usize>,
    blocking: Option<usize>,
    closed_failure: Option<(usize, GateFailure)>,
    enforce_stopped: bool,
}

impl Composer {
    const fn new(configs: Vec<GateStageConfig>, command: String) -> Self {
        Self {
            configs,
            command,
            stages: Vec::new(),
            annotations: Vec::new(),
            first_warn: None,
            first_allow: None,
            blocking: None,
            closed_failure: None,
            enforce_stopped: false,
        }
    }

    const fn enforce_stopped(&self) -> bool {
        self.enforce_stopped
    }

    fn last_stage(&self) -> Option<&StageOutcome> {
        self.stages.last()
    }

    fn record(&mut self, config: &GateStageConfig, mut stage: StageOutcome) {
        if config.mode == GateMode::Enforce && self.enforce_stopped {
            return;
        }
        let index = self.stages.len();
        self.apply_override(config, &mut stage);
        if config.mode == GateMode::Enforce {
            if self.record_failure(config, &mut stage, index) {
                self.push_stage(stage);
                return;
            }
            if let Some(decision) = stage.decision {
                self.apply_decision(config, &stage, index, decision);
            }
        }
        self.push_stage(stage);
    }

    fn apply_override(&self, config: &GateStageConfig, stage: &mut StageOutcome) {
        if stage.decision == Some(GateDecision::Block)
            && config.allowlist.iter().any(|entry| entry == &self.command)
        {
            stage.overridden = true;
        }
    }

    /// Record a failure. Returns true when the failure is closed and the chain
    /// must stop; an open failure stays on the stage and the chain continues.
    fn record_failure(
        &mut self,
        config: &GateStageConfig,
        stage: &mut StageOutcome,
        index: usize,
    ) -> bool {
        let Some(failure) = stage.failure.clone().filter(|_| stage.decision.is_none()) else {
            return false;
        };
        if config.fail == FailMode::Open {
            stage.failed_open = true;
            return false;
        }
        self.closed_failure = Some((index, failure));
        self.enforce_stopped = true;
        true
    }

    fn apply_decision(
        &mut self,
        config: &GateStageConfig,
        stage: &StageOutcome,
        index: usize,
        decision: GateDecision,
    ) {
        match decision {
            GateDecision::Abstain => {},
            GateDecision::Allow => {
                self.first_allow.get_or_insert(index);
                self.annotations.extend(stage.annotations.iter().cloned());
                if config.short_circuit_on_allow && stage.short_circuit {
                    self.enforce_stopped = true;
                }
            },
            GateDecision::Warn => {
                self.first_warn.get_or_insert(index);
                if stage.annotations.is_empty() {
                    self.annotations
                        .push(format!("NOTICE: {}", sanitize_model_text(&stage.message)));
                } else {
                    self.annotations.extend(stage.annotations.iter().cloned());
                }
            },
            GateDecision::Block if stage.overridden => {},
            GateDecision::Block => {
                self.blocking = Some(index);
                self.enforce_stopped = true;
            },
        }
    }

    fn push_stage(&mut self, stage: StageOutcome) {
        self.stages.push(stage);
    }

    fn finish(self) -> Result<PipelineOutcome, GatePipelineError> {
        if !self
            .configs
            .iter()
            .any(|config| config.mode == GateMode::Enforce)
        {
            return Err(GatePipelineError::NoEnforceGate);
        }
        if let Some((index, failure)) = self.closed_failure {
            let gate = self
                .configs
                .get(index)
                .map_or_else(|| "unknown".to_string(), |config| config.name.clone());
            return Err(GatePipelineError::Check {
                gate,
                failure,
                stages: self.stages,
            });
        }
        let (authorization, verdict, deciding_index) = if let Some(index) = self.blocking {
            let message = self
                .stages
                .get(index)
                .map_or_else(String::new, |stage| sanitize_model_text(&stage.message));
            (
                PipelineAuthorization::Block { message },
                Some(GateDecision::Block),
                Some(index),
            )
        } else if let Some(index) = self.first_warn {
            (
                PipelineAuthorization::Run,
                Some(GateDecision::Warn),
                Some(index),
            )
        } else {
            (
                PipelineAuthorization::Run,
                self.first_allow.map(|_| GateDecision::Allow),
                self.first_allow,
            )
        };
        let deciding_gate = deciding_index
            .and_then(|index| self.stages.get(index))
            .map(|stage| stage.gate.clone());
        let latency = self.stages.iter().fold(Duration::ZERO, |total, stage| {
            total.saturating_add(stage.latency)
        });
        let overridden = self.stages.iter().any(|stage| stage.overridden);
        Ok(PipelineOutcome {
            bypassed: false,
            authorization,
            verdict,
            annotations: self.annotations,
            overridden,
            latency,
            stages: self.stages,
            deciding_gate,
        })
    }
}

/// Pure composition over already-executed stage results, in execution order.
///
/// The pipeline uses the same composer incrementally so it can stop the enforce
/// chain at the right gate; tests use this entry point directly.
pub fn compose(
    configs: &[GateStageConfig],
    command: &str,
    stages: Vec<StageOutcome>,
) -> Result<PipelineOutcome, GatePipelineError> {
    let mut composer = Composer::new(configs.to_vec(), command.to_string());
    for (index, stage) in stages.into_iter().enumerate() {
        match configs.get(index) {
            Some(config) => composer.record(config, stage),
            None => {
                composer.stages.push(stage);
            },
        }
    }
    composer.finish()
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "composer_tests.rs"]
mod tests;
