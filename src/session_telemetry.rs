use std::{
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
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

#[derive(Debug, Clone, Copy, Serialize)]
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

/// Shared session-telemetry writer with a fail-stop disabled flag.
///
/// The agent loop and the judge-attempt sink share one instance, so a write
/// failure on either path disables telemetry for both: once any append fails,
/// no further records are written and a partial NDJSON line is never followed
/// by more records (see the recovery contract in the judge-attempt-diagnostics
/// exec plan). The disabled check and the failure transition happen while the
/// writer lock is held, so concurrent appends cannot race past the flag.
pub struct SharedSessionTelemetryWriter {
    writer: Mutex<SessionTelemetryWriter>,
    disabled: Arc<AtomicBool>,
}

impl SharedSessionTelemetryWriter {
    pub fn new(writer: SessionTelemetryWriter) -> Self {
        Self {
            writer: Mutex::new(writer),
            disabled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Append one record, fail-stop: after the first failure no further
    /// records are written.
    pub fn append(&self, record: &SessionTelemetryRecord) -> TelemetryAppend {
        let mut writer = match self.writer.lock() {
            Ok(writer) => writer,
            Err(poisoned) => {
                return TelemetryAppend::Failed(anyhow::anyhow!(
                    "session telemetry writer poisoned: {poisoned:?}"
                ));
            },
        };
        if self.disabled.load(Ordering::Relaxed) {
            return TelemetryAppend::Disabled;
        }
        match writer.append(record) {
            Ok(()) => TelemetryAppend::Written,
            Err(error) => {
                self.disabled.store(true, Ordering::Relaxed);
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
}
