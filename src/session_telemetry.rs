use std::{
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::Path,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::OutputFormat;
use crate::clients::retry::{RequestOverrides, RetryReason, RetryStatus};
use crate::config::model::{ApiType, ReasoningEffort};
use crate::types::Usage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminationClassification {
    Completed,
    ToolCalls,
    TokenLimit,
    ContentFilter,
    Incomplete,
    Failed,
    Unknown,
}

impl TerminationClassification {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::ToolCalls => "tool_calls",
            Self::TokenLimit => "token_limit",
            Self::ContentFilter => "content_filter",
            Self::Incomplete => "incomplete",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderTermination {
    pub classification: TerminationClassification,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionTelemetryRunMode {
    New,
    Continue,
    Resume,
    Fork,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionTelemetrySettings {
    pub api_type: ApiType,
    pub output_format: OutputFormat,
    pub max_output_tokens: Option<u32>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub reasoning_max_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionTelemetryContext {
    pub session_id: String,
    pub invocation_id: String,
}

/// Terminal outcome of one provider attempt, independent of the semantic
/// [`TerminationClassification`]. Lets consumers tell apart a transport
/// failure, a non-2xx HTTP failure, a 2xx body/SSE decode failure, an accepted
/// 2xx request that ended in `response.failed`, and a completed turn without
/// parsing provider messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiAttemptTerminalClass {
    /// The turn completed and produced an item.
    Completed,
    /// The request phase failed (connect, timeout, stale connection).
    Transport,
    /// The provider returned a non-2xx HTTP response.
    Http,
    /// The 2xx body could not be decoded as JSON/SSE, or the stream ended
    /// before a terminal event.
    BodyParse,
    /// The provider accepted the request (HTTP 2xx) then emitted a terminal
    /// `response.failed` event.
    ResponseFailed,
}

/// Bounded, structured metadata from a terminal `response.failed` stream event
/// on the Responses API after an accepted HTTP 2xx.
///
/// Never carries raw response bodies, prompts, tool outputs, authorization
/// data, or credentials. Only the provider-supplied error identity fields and
/// the provider response ID are retained so an attempt can be diagnosed and
/// correlated without persisting the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct ResponsesFailedMetadata {
    /// Carried internally to the attempt-level field; never duplicated inside
    /// the serialized `responses_failed` object.
    #[serde(skip)]
    pub provider_request_id: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    #[serde(rename = "code", skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(rename = "param", skip_serializing_if = "Option::is_none")]
    pub error_param: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiAttemptTelemetry {
    pub turn_index: u32,
    pub attempt: u32,
    pub request_ms: u64,
    pub parse_ms: u64,
    pub total_ms: u64,
    pub history_items: usize,
    pub status_code: Option<u16>,
    pub error: Option<String>,
    pub usage: Option<Usage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub termination: Option<ProviderTermination>,
    /// How this attempt ended, so consumers can tell a transient provider
    /// failure from a request/transport or decode failure without parsing
    /// error text. `None` on legacy records.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_class: Option<ApiAttemptTerminalClass>,
    /// Provider response ID, when the provider supplied one. Retained on
    /// success and on `response.failed` so the attempt can be correlated with
    /// provider logs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_request_id: Option<String>,
    /// Bounded structured metadata when the attempt ended in `response.failed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub responses_failed: Option<Box<ResponsesFailedMetadata>>,
    pub request_overrides: RequestOverridesSnapshot,
}

/// Terminal outcome of one command-safety judge provider attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JudgeAttemptTerminalClass {
    Verdict,
    Timeout,
    Transport,
    HttpError,
    ResponseParse,
    MalformedVerdict,
    Refusal,
}

/// Metadata-only diagnostics for one command-safety judge provider attempt.
///
/// Raw prompts, command text, reason text, cwd, request and response bodies,
/// credentials, and authorization headers must never enter this type.
#[derive(Debug, Clone, Serialize)]
pub struct JudgeAttemptTelemetry {
    pub attempt: u32,
    pub retry_ordinal: u32,
    /// The classification that triggered this attempt's retry, present only on
    /// a recovery attempt (`retry_ordinal > 0`). Never carries command or
    /// prompt text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_reason: Option<RetryReasonSnapshot>,
    /// The backoff wait before this attempt, 0 for the first attempt.
    pub retry_delay_ms: u64,
    /// The complete judge operation deadline for this evaluation
    /// (`timeout_secs + retry_budget_secs`), the same on every attempt.
    pub effective_deadline_ms: u64,
    pub request_build_ms: u64,
    pub request_ms: u64,
    pub response_parse_ms: u64,
    pub verdict_parse_ms: u64,
    pub total_ms: u64,
    pub history_items: usize,
    pub system_prompt_bytes: usize,
    pub user_prompt_bytes: usize,
    pub model: String,
    pub api_type: ApiType,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub reasoning_max_tokens: Option<u32>,
    pub configured_timeout_ms: u64,
    pub tool_count: usize,
    pub tool_choice: Option<String>,
    pub status_code: Option<u16>,
    /// One-way digest of the originating tool call identifier, when the
    /// attempt came from a tool execution: concurrent Bash calls record
    /// attempts in completion order, so consumers attribute an attempt to its
    /// tool call by hashing the session's raw call identifier with the same
    /// function. The raw value is provider-controlled text and never enters
    /// telemetry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// One-way digest of the provider request identifier, when the provider
    /// supplied one. Consumers correlate the attempt with provider logs by
    /// hashing the known raw identifier with the same function; raw
    /// provider-controlled text never enters telemetry.
    pub provider_request_id: Option<String>,
    pub terminal_class: JudgeAttemptTerminalClass,
    pub usage: Option<Usage>,
    pub termination: Option<ProviderTermination>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetryScheduledTelemetry {
    pub turn_index: u32,
    pub attempt: u32,
    pub max_retries: u32,
    pub reason: RetryReasonSnapshot,
    pub delay_ms: u64,
    pub detail: String,
    pub changed_request_overrides: bool,
    pub request_overrides: RequestOverridesSnapshot,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolCallTelemetry {
    pub turn_index: u32,
    pub call_id: String,
    pub name: String,
    pub duration_ms: u64,
    pub output_bytes: usize,
    pub was_error: bool,
}

#[derive(Debug, Clone)]
pub enum AgentRunnerTelemetryEvent {
    ApiAttempt(ApiAttemptTelemetry),
    RetryScheduled(RetryScheduledTelemetry),
    Compensation(CompensationEventTelemetry),
}

#[derive(Debug, Clone, Serialize)]
pub struct RequestOverridesSnapshot {
    pub max_output_tokens: Option<u32>,
    pub reasoning_max_tokens: Option<u32>,
    pub context_overflow_retry_used: bool,
}

impl From<&RequestOverrides> for RequestOverridesSnapshot {
    fn from(overrides: &RequestOverrides) -> Self {
        Self {
            max_output_tokens: overrides.max_output_tokens,
            reasoning_max_tokens: overrides.reasoning_max_tokens,
            context_overflow_retry_used: overrides.context_overflow_retry_used,
        }
    }
}

/// A model weakness cake compensates for during a session.
///
/// Every variant maps to one compensation: hand-coded knowledge that rescues
/// the model. A flatlined counter for a given model is the signal that the
/// compensation is a deletion candidate (see the expiration-review discipline
/// documented next to the counters in `scripts/session-metrics`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompensationKind {
    /// Recoverable malformed tool-call arguments repaired before parsing.
    /// `detail` names the tool whose arguments were repaired.
    JsonRepair,
    /// LLM judge verdict on a Bash command. `detail` is `block:<code>`,
    /// `warn:<code>`, or `allow`; `latency_ms` is the judge call duration;
    /// `overridden` is set when an allowlist entry overrode the verdict.
    JudgeVerdict,
    /// The judge failed (timeout, transport, malformed, refusal) and the
    /// command was blocked. `detail` names the failure class.
    JudgeFailClosed,
    /// The judge was disabled for the call (emergency bypass: `CAKE_JUDGE=off`
    /// or `enabled = false`), so the command ran without a judge verdict.
    /// Emitted per bypassed call so the escape hatch cannot be used silently.
    JudgeBypass,
    /// A second same-path mutation reordered into serial execution.
    /// `detail` is the canonical target path.
    SamePathSerialization,
    /// Tool output exceeded its inline cap and was truncated or spilled to a
    /// temp file. `detail` names the tool.
    OutputTruncation,
    /// A request exceeded context and was retried with reduced output tokens.
    ContextOverflowRetry,
    /// A tool call's arguments failed to parse after the repair pass.
    EditInvalidArguments,
}

/// One model-compensation event. The sidecar records one of these per event;
/// the metrics suite counts them per model over a date range.
#[derive(Debug, Clone, Serialize)]
pub struct CompensationEventTelemetry {
    pub kind: CompensationKind,
    /// Kind-specific detail; see [`CompensationKind`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Judge verdict call duration, when the event is a judge verdict.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    /// Set on a judge verdict event when an allowlist entry overrode a block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overridden: Option<bool>,
}

impl CompensationEventTelemetry {
    pub const fn new(kind: CompensationKind, detail: Option<String>) -> Self {
        Self {
            kind,
            detail,
            latency_ms: None,
            overridden: None,
        }
    }

    /// Build a judge verdict event: decision and verdict code (block and warn
    /// verdicts carry a code; allow omits it), the judge call latency, and
    /// whether an allowlist entry overrode the verdict. Metadata only — never
    /// the command or reason text.
    pub fn judge_verdict(
        decision: &str,
        code: Option<&str>,
        latency_ms: u64,
        overridden: bool,
    ) -> Self {
        let detail = code.map_or_else(|| decision.to_string(), |code| format!("{decision}:{code}"));
        Self {
            kind: CompensationKind::JudgeVerdict,
            detail: Some(detail),
            latency_ms: Some(latency_ms),
            overridden: overridden.then_some(true),
        }
    }

    /// Build a fail-closed judge event: the judge error class that blocked
    /// the command instead of a verdict.
    pub fn judge_fail_closed(error_class: &str) -> Self {
        Self {
            kind: CompensationKind::JudgeFailClosed,
            detail: Some(error_class.to_string()),
            latency_ms: None,
            overridden: None,
        }
    }

    /// Build a judge-bypass event: the judge was disabled for this call, so
    /// the command ran without a verdict. One event per bypassed call.
    pub const fn judge_bypass() -> Self {
        Self {
            kind: CompensationKind::JudgeBypass,
            detail: None,
            latency_ms: None,
            overridden: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryReasonSnapshot {
    RateLimit,
    Overloaded,
    ServerError,
    RequestTimeout,
    LockTimeout,
    Network,
    ContextOverflow,
    SemanticIncomplete,
}

impl From<&RetryReason> for RetryReasonSnapshot {
    fn from(reason: &RetryReason) -> Self {
        match reason {
            RetryReason::RateLimit => Self::RateLimit,
            RetryReason::Overloaded => Self::Overloaded,
            RetryReason::ServerError => Self::ServerError,
            RetryReason::RequestTimeout => Self::RequestTimeout,
            RetryReason::LockTimeout => Self::LockTimeout,
            RetryReason::Network => Self::Network,
            RetryReason::ContextOverflow => Self::ContextOverflow,
            RetryReason::SemanticIncomplete => Self::SemanticIncomplete,
        }
    }
}

impl RetryScheduledTelemetry {
    pub fn from_status(
        status: &RetryStatus,
        turn_index: u32,
        changed_request_overrides: bool,
        request_overrides: &RequestOverrides,
    ) -> Self {
        Self {
            turn_index,
            attempt: status.attempt,
            max_retries: status.max_retries,
            reason: RetryReasonSnapshot::from(&status.reason),
            delay_ms: status.delay.as_millis().try_into().unwrap_or(u64::MAX),
            detail: status.detail.clone(),
            changed_request_overrides,
            request_overrides: RequestOverridesSnapshot::from(request_overrides),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionTelemetryRecord {
    TelemetryInit {
        session_id: String,
        invocation_id: String,
        timestamp: DateTime<Utc>,
        mode: SessionTelemetryRunMode,
        working_directory: String,
        model: String,
        api_type: ApiType,
        output_format: OutputFormat,
        tools: Vec<String>,
        settings: SessionTelemetrySettings,
    },
    ApiAttempt {
        session_id: String,
        invocation_id: String,
        timestamp: DateTime<Utc>,
        #[serde(flatten)]
        attempt: ApiAttemptTelemetry,
    },
    JudgeAttempt {
        session_id: String,
        invocation_id: String,
        timestamp: DateTime<Utc>,
        #[serde(flatten)]
        attempt: JudgeAttemptTelemetry,
    },
    RetryScheduled {
        session_id: String,
        invocation_id: String,
        timestamp: DateTime<Utc>,
        #[serde(flatten)]
        retry: RetryScheduledTelemetry,
    },
    ToolCall {
        session_id: String,
        invocation_id: String,
        timestamp: DateTime<Utc>,
        #[serde(flatten)]
        tool_call: ToolCallTelemetry,
    },
    Compensation {
        session_id: String,
        invocation_id: String,
        timestamp: DateTime<Utc>,
        #[serde(flatten)]
        event: CompensationEventTelemetry,
    },
    SessionSummary {
        session_id: String,
        invocation_id: String,
        timestamp: DateTime<Utc>,
        success: bool,
        duration_ms: u64,
        turn_count: u32,
        usage: Usage,
        error: Option<String>,
    },
}

pub struct SessionTelemetryWriter {
    writer: BufWriter<File>,
}

#[cfg(test)]
trait TelemetryRecordWriter: Send {
    fn append_record(&mut self, record: &SessionTelemetryRecord) -> anyhow::Result<()>;
}

impl SessionTelemetryWriter {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    pub fn append(&mut self, record: &SessionTelemetryRecord) -> anyhow::Result<()> {
        serde_json::to_writer(&mut self.writer, record)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }
}

/// Outcome of one shared-writer append.
#[derive(Debug)]
pub enum TelemetryAppend {
    /// The record was written.
    Written,
    /// The record was skipped because a prior append already failed and
    /// telemetry is disabled; callers stay silent.
    Disabled,
    /// The append failed and telemetry is now disabled; exactly one caller
    /// should report this transition.
    Failed(anyhow::Error),
}

/// Shared session-telemetry writer with a fail-stop disabled state.
///
/// The agent loop and the judge-attempt sink share one instance, so a write
/// failure on either path disables telemetry for both: once any append fails,
/// no further records are written and a partial NDJSON line is never followed
/// by more records (see the recovery contract in the judge-attempt-diagnostics
/// exec plan). Availability and the failure transition are one mutex-wrapped
/// state, so the pair cannot drift: exactly one caller observes `Failed`, and
/// every later caller observes `Disabled`.
pub struct SharedSessionTelemetryWriter {
    state: Mutex<TelemetryWriterState>,
}

/// Whether the shared writer is still accepting records.
///
/// `Disabled` is terminal; it is never written again once set, so every caller
/// after a failure observes `Disabled`.
enum TelemetryWriterState {
    Active(SessionTelemetryWriter),
    #[cfg(test)]
    TestActive(Box<dyn TelemetryRecordWriter>),
    Disabled,
}

impl TelemetryWriterState {
    fn append(&mut self, record: &SessionTelemetryRecord) -> Option<anyhow::Result<()>> {
        match self {
            Self::Active(writer) => Some(writer.append(record)),
            #[cfg(test)]
            Self::TestActive(writer) => Some(writer.append_record(record)),
            Self::Disabled => None,
        }
    }
}

impl SharedSessionTelemetryWriter {
    pub const fn new(writer: SessionTelemetryWriter) -> Self {
        Self {
            state: Mutex::new(TelemetryWriterState::Active(writer)),
        }
    }

    #[cfg(test)]
    fn new_for_test<W: TelemetryRecordWriter + 'static>(writer: W) -> Self {
        Self {
            state: Mutex::new(TelemetryWriterState::TestActive(Box::new(writer))),
        }
    }

    /// Append one record, fail-stop: after the first failure no further
    /// records are written.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the lock must cover the writer and the Disabled transition together, so the guard is held for the whole append rather than dropped early"
    )]
    pub fn append(&self, record: &SessionTelemetryRecord) -> TelemetryAppend {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                // A panic poisoned the lock while it held a writer that may
                // have left a partial NDJSON line. Recover the state so the
                // mutex stays the single availability authority, then fail-stop:
                // never write after a partial line. Exactly one caller observes
                // `Failed`; later callers observe `Disabled`.
                let mut state = poisoned.into_inner();
                if !matches!(&*state, TelemetryWriterState::Disabled) {
                    *state = TelemetryWriterState::Disabled;
                    return TelemetryAppend::Failed(anyhow::anyhow!(
                        "session telemetry writer poisoned: telemetry disabled"
                    ));
                }
                return TelemetryAppend::Disabled;
            },
        };
        let Some(append_result) = state.append(record) else {
            return TelemetryAppend::Disabled;
        };
        match append_result {
            Ok(()) => TelemetryAppend::Written,
            Err(error) => {
                *state = TelemetryWriterState::Disabled;
                TelemetryAppend::Failed(error)
            },
        }
    }
}

/// Shared handle that appends judge-attempt records to the session sidecar as
/// soon as judging completes.
///
/// Judge attempts used to ride tool-result compensation events, so an
/// interrupted command (for example Ctrl-C during a hung Bash call) dropped
/// them before the tool result was recorded. The Bash preflight records each
/// finalized attempt through this sink instead, so the sidecar keeps the
/// append-only durability the ADR-007 rationale requires even when the run is
/// cut short.
#[derive(Clone)]
pub struct JudgeAttemptSink {
    writer: Arc<SharedSessionTelemetryWriter>,
    context: SessionTelemetryContext,
}

impl JudgeAttemptSink {
    pub const fn new(
        writer: Arc<SharedSessionTelemetryWriter>,
        context: SessionTelemetryContext,
    ) -> Self {
        Self { writer, context }
    }

    /// Append one finalized judge attempt to the sidecar, best-effort and
    /// silent once telemetry has already been disabled by an earlier failure.
    pub fn record(&self, attempt: JudgeAttemptTelemetry) {
        let record = SessionTelemetryRecord::JudgeAttempt {
            session_id: self.context.session_id.clone(),
            invocation_id: self.context.invocation_id.clone(),
            timestamp: Utc::now(),
            attempt,
        };
        if let TelemetryAppend::Failed(error) = self.writer.append(&record) {
            tracing::warn!(target: "cake", "Failed to record judge attempt: {error}");
        }
    }
}

impl std::fmt::Debug for JudgeAttemptSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JudgeAttemptSink")
            .field("context", &self.context)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api_attempt(termination: Option<ProviderTermination>) -> ApiAttemptTelemetry {
        ApiAttemptTelemetry {
            turn_index: 1,
            attempt: 1,
            request_ms: 10,
            parse_ms: 2,
            total_ms: 12,
            history_items: 3,
            status_code: Some(200),
            error: None,
            usage: None,
            termination,
            terminal_class: None,
            provider_request_id: None,
            responses_failed: None,
            request_overrides: RequestOverridesSnapshot {
                max_output_tokens: None,
                reasoning_max_tokens: None,
                context_overflow_retry_used: false,
            },
        }
    }

    fn judge_attempt() -> JudgeAttemptTelemetry {
        JudgeAttemptTelemetry {
            attempt: 1,
            retry_ordinal: 0,
            retry_reason: None,
            retry_delay_ms: 0,
            effective_deadline_ms: 30_000,
            request_build_ms: 1,
            request_ms: 10,
            response_parse_ms: 2,
            verdict_parse_ms: 1,
            total_ms: 14,
            history_items: 2,
            system_prompt_bytes: 4000,
            user_prompt_bytes: 180,
            model: "provider/judge".to_string(),
            api_type: ApiType::Responses,
            reasoning_effort: Some(ReasoningEffort::Low),
            temperature: Some(0.0),
            top_p: None,
            max_output_tokens: Some(128),
            reasoning_max_tokens: None,
            configured_timeout_ms: 30_000,
            tool_count: 0,
            tool_choice: None,
            status_code: Some(200),
            call_id: None,
            provider_request_id: Some("req-123".to_string()),
            terminal_class: JudgeAttemptTerminalClass::Verdict,
            usage: None,
            termination: Some(ProviderTermination {
                classification: TerminationClassification::Completed,
                provider_status: Some("completed".to_string()),
                provider_reason: None,
            }),
        }
    }

    fn telemetry_summary() -> SessionTelemetryRecord {
        SessionTelemetryRecord::SessionSummary {
            session_id: "session".to_string(),
            invocation_id: "invocation".to_string(),
            timestamp: Utc::now(),
            success: true,
            duration_ms: 1,
            turn_count: 1,
            usage: Usage::default(),
            error: None,
        }
    }

    #[test]
    fn api_attempt_serializes_optional_termination_metadata() {
        let with_termination = serde_json::to_value(api_attempt(Some(ProviderTermination {
            classification: TerminationClassification::Unknown,
            provider_status: Some("future_status".to_string()),
            provider_reason: Some("future_reason".to_string()),
        })))
        .unwrap();
        assert_eq!(with_termination["termination"]["classification"], "unknown");
        assert_eq!(
            with_termination["termination"]["provider_status"],
            "future_status"
        );
        assert_eq!(
            with_termination["termination"]["provider_reason"],
            "future_reason"
        );

        let without_termination = serde_json::to_value(api_attempt(None)).unwrap();
        assert!(without_termination.get("termination").is_none());
    }

    #[test]
    fn api_attempt_serializes_terminal_class_and_response_failed_metadata() {
        let mut attempt = api_attempt(None);
        attempt.terminal_class = Some(ApiAttemptTerminalClass::ResponseFailed);
        attempt.provider_request_id = Some("req-42".to_string());
        attempt.responses_failed = Some(Box::new(ResponsesFailedMetadata {
            provider_request_id: Some("req-42".to_string()),
            error_type: Some("server_error".to_string()),
            error_code: Some("server_error".to_string()),
            error_param: None,
            message: Some("provider failed".to_string()),
        }));

        let value = serde_json::to_value(&attempt).unwrap();
        assert_eq!(value["terminal_class"], "response_failed");
        assert_eq!(value["provider_request_id"], "req-42");
        assert_eq!(value["responses_failed"]["code"], "server_error");
        assert_eq!(value["responses_failed"]["type"], "server_error");
        assert!(value["responses_failed"].get("param").is_none());
        assert!(
            value["responses_failed"]
                .get("provider_request_id")
                .is_none(),
            "provider request id belongs at the api_attempt level"
        );
        for forbidden in [
            "response_body",
            "request_body",
            "prompt",
            "api_key",
            "authorization",
        ] {
            assert!(
                value.get(forbidden).is_none(),
                "unexpected {forbidden} in {value}"
            );
            assert!(
                value["responses_failed"].get(forbidden).is_none(),
                "unexpected {forbidden} in responses_failed metadata"
            );
        }

        let bare = api_attempt(None);
        let value = serde_json::to_value(bare).unwrap();
        assert!(value.get("terminal_class").is_none());
        assert!(value.get("responses_failed").is_none());
        assert!(value.get("provider_request_id").is_none());
    }

    #[test]
    fn judge_attempt_is_first_class_metadata_only_record() {
        let record = SessionTelemetryRecord::JudgeAttempt {
            session_id: "session".to_string(),
            invocation_id: "invocation".to_string(),
            timestamp: Utc::now(),
            attempt: judge_attempt(),
        };
        let value = serde_json::to_value(record).unwrap();
        assert_eq!(value["type"], "judge_attempt");
        assert_eq!(value["terminal_class"], "verdict");
        assert_eq!(value["provider_request_id"], "req-123");
        assert_eq!(value["tool_count"], 0);
        assert!(value["usage"].is_null());
        for forbidden in [
            "command",
            "reason",
            "cwd",
            "request_json",
            "response_body",
            "api_key",
            "authorization",
        ] {
            assert!(value.get(forbidden).is_none(), "unexpected {forbidden}");
        }
    }

    #[test]
    fn judge_attempt_sink_appends_first_class_attempt_record() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("telemetry.ndjson");
        let writer = Arc::new(SharedSessionTelemetryWriter::new(
            SessionTelemetryWriter::open(&path).unwrap(),
        ));
        let sink = JudgeAttemptSink::new(
            Arc::clone(&writer),
            SessionTelemetryContext {
                session_id: "session".to_string(),
                invocation_id: "invocation".to_string(),
            },
        );
        sink.record(judge_attempt());

        let contents = std::fs::read_to_string(&path).unwrap();
        let record: serde_json::Value = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(record["type"], "judge_attempt");
        assert_eq!(record["session_id"], "session");
        assert_eq!(record["attempt"], 1);
    }

    #[test]
    fn compensation_kind_labels_match_serialized_values() {
        let cases = [
            (CompensationKind::JsonRepair, "json_repair"),
            (CompensationKind::JudgeVerdict, "judge_verdict"),
            (CompensationKind::JudgeFailClosed, "judge_fail_closed"),
            (CompensationKind::JudgeBypass, "judge_bypass"),
            (
                CompensationKind::SamePathSerialization,
                "same_path_serialization",
            ),
            (CompensationKind::OutputTruncation, "output_truncation"),
            (
                CompensationKind::ContextOverflowRetry,
                "context_overflow_retry",
            ),
            (
                CompensationKind::EditInvalidArguments,
                "edit_invalid_arguments",
            ),
        ];

        for (kind, expected) in cases {
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::json!(expected)
            );
        }
    }

    #[test]
    fn compensation_event_omits_empty_fields() {
        let event =
            CompensationEventTelemetry::new(CompensationKind::JsonRepair, Some("Edit".into()));
        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["kind"], "json_repair");
        assert_eq!(value["detail"], "Edit");
        assert!(value.get("latency_ms").is_none());

        let bare = CompensationEventTelemetry::new(CompensationKind::ContextOverflowRetry, None);
        let value = serde_json::to_value(bare).unwrap();
        assert_eq!(value["kind"], "context_overflow_retry");
        assert!(value.get("detail").is_none());
        assert!(value.get("latency_ms").is_none());
    }

    #[test]
    fn judge_verdict_event_carries_decision_code_and_latency() {
        let event = CompensationEventTelemetry::judge_verdict("block", Some("rm-rf"), 42, false);
        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["kind"], "judge_verdict");
        assert_eq!(value["detail"], "block:rm-rf");
        assert_eq!(value["latency_ms"], 42);
        assert!(value.get("overridden").is_none());

        let allow = CompensationEventTelemetry::judge_verdict("allow", None, 7, false);
        let value = serde_json::to_value(allow).unwrap();
        assert_eq!(value["detail"], "allow");

        let overridden = CompensationEventTelemetry::judge_verdict("block", Some("rm-rf"), 9, true);
        let value = serde_json::to_value(overridden).unwrap();
        assert_eq!(value["overridden"], true);

        let fail_closed = CompensationEventTelemetry::judge_fail_closed("timeout");
        let value = serde_json::to_value(fail_closed).unwrap();
        assert_eq!(value["kind"], "judge_fail_closed");
        assert_eq!(value["detail"], "timeout");
        assert!(value.get("latency_ms").is_none());

        let bypass = CompensationEventTelemetry::judge_bypass();
        let value = serde_json::to_value(bypass).unwrap();
        assert_eq!(value["kind"], "judge_bypass");
        assert!(value.get("detail").is_none());
        assert!(value.get("latency_ms").is_none());
    }

    #[test]
    fn compensation_record_serializes_flattened_event() {
        let record = SessionTelemetryRecord::Compensation {
            session_id: "session".to_string(),
            invocation_id: "invocation".to_string(),
            timestamp: Utc::now(),
            event: CompensationEventTelemetry::new(
                CompensationKind::OutputTruncation,
                Some("Read".into()),
            ),
        };
        let value = serde_json::to_value(record).unwrap();
        assert_eq!(value["type"], "compensation");
        assert_eq!(value["kind"], "output_truncation");
        assert_eq!(value["detail"], "Read");
        assert_eq!(value["session_id"], "session");
    }

    #[test]
    fn termination_classification_labels_match_serialized_values() {
        let cases = [
            (TerminationClassification::Completed, "completed"),
            (TerminationClassification::ToolCalls, "tool_calls"),
            (TerminationClassification::TokenLimit, "token_limit"),
            (TerminationClassification::ContentFilter, "content_filter"),
            (TerminationClassification::Incomplete, "incomplete"),
            (TerminationClassification::Failed, "failed"),
            (TerminationClassification::Unknown, "unknown"),
        ];

        for (classification, expected) in cases {
            assert_eq!(classification.as_str(), expected);
            assert_eq!(
                serde_json::to_value(classification).unwrap(),
                serde_json::json!(expected)
            );
        }
    }

    #[test]
    fn writer_appends_newline_delimited_json() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session-telemetry").join("test.ndjson");
        let session_id = uuid::Uuid::new_v4().to_string();
        let invocation_id = uuid::Uuid::new_v4().to_string();
        let mut writer = SessionTelemetryWriter::open(&path).unwrap();

        writer
            .append(&SessionTelemetryRecord::SessionSummary {
                session_id: session_id.clone(),
                invocation_id: invocation_id.clone(),
                timestamp: Utc::now(),
                success: true,
                duration_ms: 42,
                turn_count: 1,
                usage: Usage::default(),
                error: None,
            })
            .unwrap();
        writer
            .append(&SessionTelemetryRecord::SessionSummary {
                session_id,
                invocation_id,
                timestamp: Utc::now(),
                success: false,
                duration_ms: 99,
                turn_count: 2,
                usage: Usage::default(),
                error: Some("boom".to_string()),
            })
            .unwrap();
        drop(writer);

        let contents = std::fs::read_to_string(path).unwrap();
        let lines = contents.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(lines[0]).unwrap()["type"],
            "session_summary"
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(lines[1]).unwrap()["error"],
            "boom"
        );
        assert!(contents.ends_with('\n'));
    }

    const PARTIAL_PREFIX: &[u8] = b"{\"type\":\"session_summary\"";

    struct PrefixThenFailWriter {
        output: Arc<Mutex<Vec<u8>>>,
        failed: bool,
    }

    impl PrefixThenFailWriter {
        fn new(output: Arc<Mutex<Vec<u8>>>) -> Self {
            Self {
                output,
                failed: false,
            }
        }
    }

    impl TelemetryRecordWriter for PrefixThenFailWriter {
        fn append_record(&mut self, _record: &SessionTelemetryRecord) -> anyhow::Result<()> {
            if self.failed {
                return Err(
                    std::io::Error::other("unexpected append after deterministic failure").into(),
                );
            }

            self.failed = true;
            self.output
                .lock()
                .unwrap()
                .extend_from_slice(PARTIAL_PREFIX);
            Err(std::io::Error::other("deterministic telemetry failure").into())
        }
    }

    fn assert_partial_output(output: &Arc<Mutex<Vec<u8>>>) {
        let output = output.lock().unwrap().clone();
        assert_eq!(output.as_slice(), PARTIAL_PREFIX);
        assert!(!output.contains(&b'\n'));
        assert!(serde_json::from_slice::<serde_json::Value>(&output).is_err());
    }

    #[test]
    fn shared_writer_reports_one_failed_then_disabled_after_partial_write() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let shared = SharedSessionTelemetryWriter::new_for_test(PrefixThenFailWriter::new(
            Arc::clone(&output),
        ));
        let record = telemetry_summary();

        assert!(matches!(shared.append(&record), TelemetryAppend::Failed(_)));
        assert!(matches!(shared.append(&record), TelemetryAppend::Disabled));
        assert!(matches!(shared.append(&record), TelemetryAppend::Disabled));
        assert_partial_output(&output);
    }

    #[test]
    fn shared_writer_concurrent_failure_reports_one_failed_then_disabled() {
        const CALLERS: usize = 32;

        let output = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::new(SharedSessionTelemetryWriter::new_for_test(
            PrefixThenFailWriter::new(Arc::clone(&output)),
        ));
        let barrier = Arc::new(std::sync::Barrier::new(CALLERS + 1));
        let mut callers = Vec::with_capacity(CALLERS);
        for _ in 0..CALLERS {
            let shared = Arc::clone(&shared);
            let barrier = Arc::clone(&barrier);
            callers.push(std::thread::spawn(move || {
                barrier.wait();
                shared.append(&telemetry_summary())
            }));
        }

        barrier.wait();
        let results = callers
            .into_iter()
            .map(|caller| caller.join().unwrap())
            .collect::<Vec<_>>();

        let failed = results
            .iter()
            .filter(|result| matches!(result, TelemetryAppend::Failed(_)))
            .count();
        let disabled = results
            .iter()
            .filter(|result| matches!(result, TelemetryAppend::Disabled))
            .count();
        assert_eq!(failed, 1);
        assert_eq!(disabled, CALLERS - 1);
        assert_eq!(failed + disabled, CALLERS);
        assert_partial_output(&output);
    }

    #[test]
    fn shared_writer_poison_reports_one_failed_then_disabled() {
        let dir = tempfile::TempDir::new().unwrap();
        let writer = SessionTelemetryWriter::open(&dir.path().join("telemetry.ndjson")).unwrap();
        let shared = SharedSessionTelemetryWriter::new(writer);
        let record = telemetry_summary();

        // Poison the lock by panicking while the guard is held; the guard drops
        // during unwind and poisons the mutex.
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = shared.state.lock().unwrap();
            panic!("boom");
        }));
        assert!(poisoned.is_err());

        // A poison may mean a partial NDJSON line is already buffered, so the
        // first append reports Failed and disables the writer; no further
        // record is written after it.
        assert!(
            matches!(shared.append(&record), TelemetryAppend::Failed(_)),
            "a poisoned writer must report exactly one Failed transition"
        );
        assert!(matches!(shared.append(&record), TelemetryAppend::Disabled));
        assert!(matches!(shared.append(&record), TelemetryAppend::Disabled));
    }

    #[test]
    fn shared_writer_disabled_state_writes_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("telemetry.ndjson");
        let shared =
            SharedSessionTelemetryWriter::new(SessionTelemetryWriter::open(&path).unwrap());
        {
            let mut state = shared.state.lock().unwrap();
            *state = TelemetryWriterState::Disabled;
        }
        let record = telemetry_summary();

        assert!(matches!(shared.append(&record), TelemetryAppend::Disabled));
        assert!(matches!(shared.append(&record), TelemetryAppend::Disabled));

        let contents = std::fs::read_to_string(path).unwrap();
        assert!(
            contents.is_empty(),
            "no record may be written once disabled"
        );
    }

    #[test]
    fn shared_writer_concurrent_appends_are_interleaved_safely() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("telemetry.ndjson");
        let shared = std::sync::Arc::new(SharedSessionTelemetryWriter::new(
            SessionTelemetryWriter::open(&path).unwrap(),
        ));

        let writers = (0..8)
            .map(|thread| {
                let shared = std::sync::Arc::clone(&shared);
                std::thread::spawn(move || {
                    for record in 0..25 {
                        let result = shared.append(&telemetry_summary());
                        assert!(
                            matches!(result, TelemetryAppend::Written),
                            "thread {thread} record {record} failed: {result:?}"
                        );
                    }
                })
            })
            .collect::<Vec<_>>();
        for writer in writers {
            writer.join().unwrap();
        }

        let contents = std::fs::read_to_string(path).unwrap();
        let lines = contents.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 8 * 25);
        for line in lines {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(value["type"], "session_summary");
        }
    }
}
