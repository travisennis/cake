//! The in-process safety judge behind the gate seam.
//!
//! `BuiltinJudgeGate` is the only gate in slice 1. It translates the existing
//! `JudgeRequest`/`JudgeOutcome` path into protocol decisions and keeps every
//! current behavior: `NOTICE:`/`BLOCKED` framing through the composer, the
//! 1,000-character model-text cap, the exact-command allowlist (applied by the
//! composer), the emergency bypass, the `TypeSafe` cascade, and the judge's own
//! `timeout_secs + retry_budget_secs` operation deadline. It reports as the
//! reserved gate `builtin-safety` with the Cake package version.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crate::clients::judge::{
    JudgeClient, JudgeClientError, JudgeContext, JudgeDecision, JudgeError, JudgeOutcome,
    JudgeRequest, JudgeVerdict, ScriptEvidence, evaluate_command_observed, judge_is_enabled,
};
use crate::clients::typesafe::TypeSafeObservation;
use crate::config::settings::{JudgeSettings, TypeSafeMode};
use crate::gates::composer::{
    Gate, GateAnswer, GateFuture, GatePipeline, GateStageConfig, GateStartup, StageObservation,
    StageObservationOutcome,
};
use crate::gates::protocol::{
    BashInput, CheckFrame, DecisionFrame, Describe, GateAction, GateDecision, GateErrorClass,
    GatePhase, GateTransport, PROTOCOL_VERSION,
};
use crate::session_telemetry::{CompensationEventTelemetry, JudgeAttemptSink};

/// The reserved local name for the in-process judge during the dual-path
/// release. It is removed in slice 5.
pub const BUILTIN_GATE_NAME: &str = "builtin-safety";

/// The built-in judge's protocol description.
pub fn builtin_describe() -> Describe {
    Describe {
        protocol: PROTOCOL_VERSION.to_string(),
        name: BUILTIN_GATE_NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        phases: vec![GatePhase::PreTool],
        actions: vec![GateAction::BashExec],
        transports: vec![GateTransport::Stdio],
        capabilities: Vec::new(),
    }
}

/// The composition settings the built-in judge always uses.
pub fn builtin_stage_config(settings: &JudgeSettings) -> GateStageConfig {
    GateStageConfig {
        allowlist: settings.allowlist.clone(),
        ..GateStageConfig::enforce(BUILTIN_GATE_NAME)
    }
}

enum JudgeSource {
    Context(Arc<JudgeContext>),
    Client(Box<JudgeClient>),
}

/// The in-process judge adapter.
pub struct BuiltinJudgeGate {
    describe: Describe,
    source: JudgeSource,
    settings: JudgeSettings,
    bypass_env: Option<String>,
    sink: OnceLock<JudgeAttemptSink>,
}

impl std::fmt::Debug for BuiltinJudgeGate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BuiltinJudgeGate")
            .field("describe", &self.describe)
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl BuiltinJudgeGate {
    /// Build the adapter over the run's shared judge configuration.
    pub fn from_context(judge: Arc<JudgeContext>) -> Self {
        let settings = judge.settings.clone();
        let bypass_env = judge.bypass_env.clone();
        Self {
            describe: builtin_describe(),
            source: JudgeSource::Context(judge),
            settings,
            bypass_env,
            sink: OnceLock::new(),
        }
    }

    /// Build the adapter over an already-constructed client. Used by the
    /// `bash check` stub tests so they drive the composer without settings or
    /// process spawning.
    pub fn from_client(
        client: JudgeClient,
        settings: JudgeSettings,
        bypass_env: Option<String>,
    ) -> Self {
        Self {
            describe: builtin_describe(),
            source: JudgeSource::Client(Box::new(client)),
            settings,
            bypass_env,
            sink: OnceLock::new(),
        }
    }

    fn client(&self) -> Result<&JudgeClient, JudgeClientError> {
        match &self.source {
            JudgeSource::Context(context) => context.judge_client().map_err(Clone::clone),
            JudgeSource::Client(client) => Ok(client),
        }
    }

    fn attempt_sink(&self) -> Option<&JudgeAttemptSink> {
        self.sink.get().or_else(|| match &self.source {
            JudgeSource::Context(context) => context.record_attempt.as_ref(),
            JudgeSource::Client(_) => None,
        })
    }

    fn record_attempts(&self, attempts: &[crate::session_telemetry::JudgeAttemptTelemetry]) {
        let Some(sink) = self.attempt_sink() else {
            return;
        };
        for attempt in attempts {
            sink.record(attempt.clone());
        }
    }

    fn record_observation(
        &self,
        observation: Option<&TypeSafeObservation>,
        raw_call_id: Option<&str>,
    ) {
        let (Some(observation), Some(sink)) = (observation, self.attempt_sink()) else {
            return;
        };
        sink.record_typesafe(
            crate::session_telemetry::TypeSafeShadowTelemetry {
                elapsed_ms: u64::try_from(observation.elapsed.as_millis()).unwrap_or(u64::MAX),
                model: observation.model.clone(),
                probability: observation.probability,
                call_id: None,
                usage_input_tokens: observation.usage_input_tokens,
                usage_output_tokens: observation.usage_output_tokens,
                failure_class: observation.failure_class.map(str::to_string),
            },
            raw_call_id,
        );
    }

    async fn evaluate(&self, frame: CheckFrame) -> GateAnswer {
        let started = Instant::now();
        let raw_call_id = frame
            .session
            .as_ref()
            .and_then(|session| session.call_id.as_deref())
            .map(str::to_string);
        let call_id = raw_call_id.as_deref();
        if !judge_is_enabled(&self.settings, self.bypass_env.as_deref()) {
            return GateAnswer::bypassed(
                started.elapsed(),
                vec![CompensationEventTelemetry::judge_bypass().with_call_id(call_id)],
            );
        }
        let Some(request) = judge_request(&frame) else {
            return failure_answer(
                GateErrorClass::Protocol,
                "protocol",
                "check frame has no Bash input",
                started.elapsed(),
                call_id,
            );
        };
        let client = match self.client() {
            Ok(client) => client,
            Err(error) => {
                return failure_answer(
                    GateErrorClass::Config,
                    error.class,
                    error.message,
                    started.elapsed(),
                    call_id,
                );
            },
        };
        // The composer owns the exact-command allowlist for every gate, so the
        // built-in path evaluates without one and returns the raw decision.
        let mut settings = self.settings.clone();
        settings.allowlist.clear();
        let evaluation = evaluate_command_observed(
            client,
            &settings,
            request,
            self.bypass_env.as_deref(),
            frame.options.diagnostic,
        )
        .await;
        self.record_attempts(&evaluation.attempts);
        self.record_observation(evaluation.shadow.as_ref(), call_id);
        let fast_approved = evaluation
            .outcome
            .as_ref()
            .is_ok_and(|outcome| matches!(outcome, JudgeOutcome::FastApproved { .. }));
        let observation = stage_observation(
            evaluation.shadow.as_ref(),
            self.settings.typesafe.mode,
            fast_approved,
        );
        outcome_answer(
            frame.id,
            evaluation,
            observation,
            started.elapsed(),
            call_id,
        )
    }
}

fn failure_answer(
    class: GateErrorClass,
    legacy_class: &'static str,
    message: impl Into<String>,
    latency: Duration,
    call_id: Option<&str>,
) -> GateAnswer {
    let failure = crate::gates::composer::GateFailure::new(class, message);
    GateAnswer::failure(
        failure,
        false,
        latency,
        vec![CompensationEventTelemetry::judge_fail_closed(legacy_class).with_call_id(call_id)],
    )
}

fn outcome_answer(
    request_id: String,
    evaluation: crate::clients::judge::JudgeEvaluation,
    observation: StageObservation,
    latency: Duration,
    call_id: Option<&str>,
) -> GateAnswer {
    let attempt_latency = judge_latency_ms(&evaluation.attempts);
    match evaluation.outcome {
        Ok(JudgeOutcome::Bypassed) => GateAnswer::bypassed(
            latency,
            vec![CompensationEventTelemetry::judge_bypass().with_call_id(call_id)],
        ),
        Ok(JudgeOutcome::FastApproved { elapsed, .. }) => {
            let decision = DecisionFrame::new(request_id, GateDecision::Allow);
            let event = CompensationEventTelemetry::judge_verdict(
                GateDecision::Allow.as_str(),
                None,
                duration_millis(elapsed),
                false,
            )
            .with_call_id(call_id);
            GateAnswer::decision(decision, latency, Some(observation), true, vec![event])
        },
        Ok(JudgeOutcome::Verdict {
            verdict,
            overridden,
        }) => verdict_answer(
            request_id,
            &verdict,
            overridden,
            observation,
            latency,
            attempt_latency,
            call_id,
        ),
        Err(error) => {
            let class = gate_error_class(&error);
            let legacy_class = error.error_class();
            failure_answer(class, legacy_class, error.to_string(), latency, call_id)
        },
    }
}

fn verdict_answer(
    request_id: String,
    verdict: &JudgeVerdict,
    overridden: bool,
    observation: StageObservation,
    latency: Duration,
    attempt_latency: u64,
    call_id: Option<&str>,
) -> GateAnswer {
    let decision = gate_decision(verdict.decision);
    let mut frame = DecisionFrame::new(request_id, decision).with_message(verdict.message.clone());
    if let Some(code) = verdict.code.clone() {
        frame = frame.with_code(code);
    }
    if let Some(confidence) = verdict.confidence {
        frame = frame.with_confidence(confidence);
    }
    let event = CompensationEventTelemetry::judge_verdict(
        decision.as_str(),
        verdict.code.as_deref(),
        attempt_latency,
        overridden,
    )
    .with_call_id(call_id);
    GateAnswer::decision(frame, latency, Some(observation), false, vec![event])
}

impl Gate for BuiltinJudgeGate {
    fn describe(&self) -> &Describe {
        &self.describe
    }

    fn check(&self, frame: CheckFrame) -> GateFuture<'_> {
        Box::pin(self.evaluate(frame))
    }

    fn install_judge_attempt_sink(&self, sink: &JudgeAttemptSink) {
        drop(self.sink.set(sink.clone()));
    }

    fn is_bypassed(&self) -> bool {
        !judge_is_enabled(&self.settings, self.bypass_env.as_deref())
    }

    fn judge_context(&self) -> Option<&Arc<JudgeContext>> {
        match &self.source {
            JudgeSource::Context(context) => Some(context),
            JudgeSource::Client(_) => None,
        }
    }
}

/// Build the one-stage built-in pipeline.
pub fn builtin_pipeline(
    judge: Arc<JudgeContext>,
) -> Result<GatePipeline, crate::gates::composer::GatePipelineError> {
    let config = builtin_stage_config(&judge.settings);
    let gate = Arc::new(BuiltinJudgeGate::from_context(judge));
    GatePipeline::single(GateStartup::ready(config, gate, Duration::ZERO))
}

/// Build a one-stage pipeline around an already-constructed judge client. The
/// `bash check` stub tests use this to drive the exact production composition.
pub fn pipeline_from_client(
    client: JudgeClient,
    settings: JudgeSettings,
    bypass_env: Option<String>,
    startup_latency: Duration,
) -> Result<GatePipeline, crate::gates::composer::GatePipelineError> {
    let config = builtin_stage_config(&settings);
    let gate = Arc::new(BuiltinJudgeGate::from_client(client, settings, bypass_env));
    GatePipeline::single(GateStartup::ready(config, gate, startup_latency))
}

fn judge_request(frame: &CheckFrame) -> Option<JudgeRequest> {
    let BashInput {
        command,
        cwd,
        reason,
        repo_digest,
        script_evidence,
    } = &frame.input;
    if command.is_empty() || cwd.is_empty() {
        return None;
    }
    let call_id = frame
        .session
        .as_ref()
        .and_then(|session| session.call_id.clone());
    let mut request = JudgeRequest::new(
        command.clone(),
        std::path::PathBuf::from(cwd),
        reason.clone(),
    )
    .with_repo_digest(repo_digest.clone())
    .with_call_id(call_id);
    request.script_evidence = script_evidence.first().map(|evidence| ScriptEvidence {
        path: evidence.path.clone(),
        contents: evidence.contents.clone(),
    });
    Some(request)
}

const fn gate_decision(decision: JudgeDecision) -> GateDecision {
    match decision {
        JudgeDecision::Allow => GateDecision::Allow,
        JudgeDecision::Warn => GateDecision::Warn,
        JudgeDecision::Block => GateDecision::Block,
    }
}

fn stage_observation(
    observation: Option<&TypeSafeObservation>,
    mode: TypeSafeMode,
    fast_approved: bool,
) -> StageObservation {
    let Some(observation) = observation else {
        return StageObservation::absent();
    };
    let outcome = match mode {
        TypeSafeMode::Off => return StageObservation::absent(),
        TypeSafeMode::Shadow => StageObservationOutcome::Shadow,
        TypeSafeMode::Cascade if fast_approved => StageObservationOutcome::Approved,
        TypeSafeMode::Cascade if observation.failure_class.is_some() => {
            StageObservationOutcome::Failed
        },
        TypeSafeMode::Cascade => StageObservationOutcome::BelowCutoff,
    };
    StageObservation {
        outcome,
        probability: observation.probability,
        failure_class: observation.failure_class,
        elapsed_ms: Some(duration_millis(observation.elapsed)),
    }
}

fn judge_latency_ms(attempts: &[crate::session_telemetry::JudgeAttemptTelemetry]) -> u64 {
    attempts.iter().fold(0_u64, |acc, attempt| {
        acc.saturating_add(attempt.total_ms)
            .saturating_add(attempt.retry_delay_ms)
    })
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn gate_error_class(error: &JudgeError) -> GateErrorClass {
    match error {
        JudgeError::Timeout(_) => GateErrorClass::Timeout,
        JudgeError::Transport {
            status: Some(status),
            ..
        } => status_class(*status),
        JudgeError::Transport {
            status: None,
            detail,
        } => transport_class(detail),
        JudgeError::Malformed(_) => GateErrorClass::Malformed,
        JudgeError::Refusal => GateErrorClass::Refusal,
    }
}

const fn status_class(status: u16) -> GateErrorClass {
    match status {
        401 | 403 => GateErrorClass::Auth,
        408 => GateErrorClass::Timeout,
        429 => GateErrorClass::RateLimit,
        _ => GateErrorClass::Transport,
    }
}

fn transport_class(detail: &str) -> GateErrorClass {
    if crate::exit_code::is_api_network_error(detail) {
        GateErrorClass::Network
    } else {
        GateErrorClass::Transport
    }
}

/// A judge verdict used by the compatibility projection in `cake bash check`.
pub const fn compatibility_verdict(
    decision: GateDecision,
    code: Option<String>,
    message: String,
    confidence: Option<f32>,
) -> JudgeVerdict {
    JudgeVerdict {
        decision: match decision {
            GateDecision::Allow | GateDecision::Abstain => JudgeDecision::Allow,
            GateDecision::Warn => JudgeDecision::Warn,
            GateDecision::Block => JudgeDecision::Block,
        },
        code,
        message,
        confidence,
    }
}

#[cfg(test)]
#[path = "judge_gate_tests.rs"]
mod tests;
