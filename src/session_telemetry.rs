use std::{
    fs::{self, File, OpenOptions},
    io::{BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::OutputFormat;
use crate::clients::retry::{RequestOverrides, RetryReason, RetryStatus};
use crate::config::model::{ApiType, ModelProvider, ReasoningEffort};
use crate::types::ProviderIdentity;
use crate::types::provider_identity::ProviderRequestShape;
use crate::types::{ApiAttemptTerminalClass, TurnUsagePurpose, Usage, UsagePresence};

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
    /// The `[[models]]` entry name the invocation resolved to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_config: Option<String>,
    /// The provider endpoint the invocation targets (non-secret).
    /// Distinguishes same-ID entries on different backends.
    ///
    /// Serialized through [`sanitize_endpoint`]: userinfo, query, and fragment
    /// are removed before the metadata byte bound is applied, and a value that
    /// is not a parsable URL serializes as `null` rather than persisting the
    /// raw string.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_endpoint_optional"
    )]
    pub base_url: Option<String>,
}

/// Sanitized provider identity for one invocation.
///
/// The session snapshot stores the environment variable name; telemetry
/// intentionally drops it and keeps only the credential kind, so a sidecar can
/// attribute provider drift without carrying a local variable name.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderIdentityTelemetry {
    pub model: String,
    pub model_config: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_endpoint_optional"
    )]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url_query_digest: Option<String>,
    pub api_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routing: Vec<String>,
    pub credential_kind: String,
    #[serde(default, skip_serializing_if = "ProviderRequestShape::is_empty")]
    pub request_shape: ProviderRequestShape,
}

impl From<&ProviderIdentity> for ProviderIdentityTelemetry {
    fn from(identity: &ProviderIdentity) -> Self {
        Self {
            model: identity.model.clone(),
            model_config: identity.model_config.clone(),
            base_url: Some(identity.base_url.clone()),
            base_url_query_digest: identity.base_url_query_digest.clone(),
            api_type: identity.api_type.clone(),
            provider: identity.provider.clone(),
            routing: identity.routing.clone(),
            credential_kind: identity.credential.kind().to_string(),
            request_shape: identity.request_shape.clone(),
        }
    }
}

/// Path-free project trust metadata for one invocation.
///
/// Records the effective mode, a one-way project key, and the number of
/// authority-bearing files. It never carries the project root, a file path, a
/// settings key, or file content.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectTrustTelemetry {
    pub mode: &'static str,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_key: Option<String>,
    pub authority_files: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionTelemetryContext {
    pub session_id: String,
    pub invocation_id: String,
}

/// One explicit working-directory rebind for this invocation.
///
/// The sidecar records one-way digests rather than the working directories, so
/// it remains path-free.
#[derive(Debug, Clone, Serialize)]
pub struct ContextRebindTelemetry {
    pub stored_working_directory_digest: String,
    pub current_working_directory_digest: String,
    pub reason: String,
}

/// Bounded, structured metadata from a terminal `response.failed` stream event
/// on the Responses API after an accepted HTTP 2xx.
///
/// Never carries raw response bodies, prompts, tool outputs, authorization
/// data, or credentials. Every provider-supplied string serializes through
/// [`bound_metadata`], and the response ID has exactly one owner: it is carried
/// in memory here, skipped from the nested object, copied to
/// [`ApiAttemptTelemetry::provider_request_id`], and serialized there only as a
/// one-way digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct ResponsesFailedMetadata {
    /// Carried internally to the attempt-level field; never serialized inside
    /// the `responses_failed` object. The attempt-level
    /// [`ApiAttemptTelemetry::provider_request_id`] serializes the value as a
    /// one-way digest, so the raw provider-controlled identifier reaches no
    /// record through either representation.
    #[serde(skip)]
    pub provider_request_id: Option<String>,
    #[serde(
        rename = "type",
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_bounded_optional"
    )]
    pub error_type: Option<String>,
    #[serde(
        rename = "code",
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_bounded_optional"
    )]
    pub error_code: Option<String>,
    #[serde(
        rename = "param",
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_bounded_optional"
    )]
    pub error_param: Option<String>,
    /// Provider error text. The in-memory value keeps its full length for the
    /// user-facing error on `crate::clients::responses::ResponsesStreamFailed`; the
    /// serialized copy is bounded.
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_bounded_optional"
    )]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiAttemptPhase {
    AwaitingHeaders,
    ReadingBody,
}

impl ApiAttemptPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AwaitingHeaders => "awaiting_headers",
            Self::ReadingBody => "reading_body",
        }
    }
}

/// Bounded context for a provider attempt that has started but has not yet
/// produced a terminal [`ApiAttemptTelemetry`] record.
///
/// The first record identifies an attempt before the HTTP future is awaited;
/// an optional second record changes `phase` to `reading_body` once headers
/// arrive. This keeps the sidecar useful during a stalled request without
/// buffering request or response bodies.
#[derive(Debug, Clone, Serialize)]
pub struct ApiAttemptInFlightTelemetry {
    pub task_id: String,
    pub turn_index: u32,
    pub attempt: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<ModelProvider>,
    pub model: String,
    pub started_at: DateTime<Utc>,
    pub phase: ApiAttemptPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
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
    /// Provider or transport error text, bounded at serialization so an
    /// oversized error body cannot grow the record.
    #[serde(serialize_with = "serialize_bounded_optional")]
    pub error: Option<String>,
    pub usage: Option<Usage>,
    /// Whether the provider reported every required token counter for this
    /// attempt. `partial` means the provider sent a usage object with at least
    /// one counter absent, so a zero in `usage` is not a measured zero;
    /// `unreported` means no usage object arrived.
    pub usage_presence: UsagePresence,
    /// The resolved provider for this attempt, when the endpoint matched a
    /// known provider strategy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<ModelProvider>,
    /// The configured model ID for this attempt.
    pub model: String,
    /// The model the provider reported serving the response, when its wire
    /// format supplied one and the attempt produced a parsed response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub termination: Option<ProviderTermination>,
    /// How this attempt ended, so consumers can tell a transient provider
    /// failure from a request/transport or decode failure without parsing
    /// error text. `None` on legacy records.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_class: Option<ApiAttemptTerminalClass>,
    /// Provider response ID, when the provider supplied one. Retained on
    /// success and on `response.failed` so the attempt can be correlated with
    /// provider logs, and serialized only as the one-way digest
    /// [`digest_identifier`] produces: a consumer correlates by hashing the
    /// known raw identifier the same way.
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_digested_optional"
    )]
    pub provider_request_id: Option<String>,
    /// Bounded structured metadata when the attempt ended in `response.failed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub responses_failed: Option<Box<ResponsesFailedMetadata>>,
    /// The last known phase when this attempt completed. This is optional so
    /// historical sidecars retain their original shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<ApiAttemptPhase>,
    pub request_overrides: RequestOverridesSnapshot,
    /// Why this attempt ran. Absent on historical and main-agent records.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purpose: Option<TurnUsagePurpose>,
    /// The checkpoint a compaction-purpose attempt belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction_id: Option<String>,
    /// The logical summary request within a compaction operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_step: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetryScheduledTelemetry {
    pub turn_index: u32,
    pub attempt: u32,
    pub max_retries: u32,
    pub reason: RetryReasonSnapshot,
    pub delay_ms: u64,
    /// The retry classification's detail, which may embed provider-supplied
    /// text; bounded at serialization.
    #[serde(serialize_with = "serialize_bounded_required")]
    pub detail: String,
    pub changed_request_overrides: bool,
    pub request_overrides: RequestOverridesSnapshot,
}

/// Elapsed backoff actually awaited after a scheduled main-provider retry.
#[derive(Debug, Clone, Serialize)]
pub struct RetryWaitTelemetry {
    pub turn_index: u32,
    pub attempt: u32,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolCallTelemetry {
    pub turn_index: u32,
    /// The provider-assigned tool call identifier, held raw in memory so hooks,
    /// transcripts, and permission labels keep the identifier the provider
    /// sent. Serialization writes only the one-way digest
    /// [`digest_identifier`] produces, and an empty identifier writes no field,
    /// matching the other telemetry identifier fields.
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_digested_optional"
    )]
    pub call_id: Option<String>,
    pub name: String,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    pub output_bytes: usize,
    pub was_error: bool,
}

#[derive(Debug, Clone)]
#[expect(
    clippy::large_enum_variant,
    reason = "the attempt variant is one completed provider-attempt record, built once per \
              HTTP request and moved once, so boxing it would add a heap allocation to a \
              network-bound path for no measurable benefit"
)]
pub enum AgentRunnerTelemetryEvent {
    ApiAttemptInFlight(ApiAttemptInFlightTelemetry),
    ApiAttempt(ApiAttemptTelemetry),
    Retry(RetryTelemetryEvent),
    Compensation(CompensationEventTelemetry),
}

#[derive(Debug, Clone)]
pub enum RetryTelemetryEvent {
    Scheduled(RetryScheduledTelemetry),
    Wait(RetryWaitTelemetry),
}

impl AgentRunnerTelemetryEvent {
    pub const fn retry_scheduled(retry: RetryScheduledTelemetry) -> Self {
        Self::Retry(RetryTelemetryEvent::Scheduled(retry))
    }

    pub const fn retry_wait(wait: RetryWaitTelemetry) -> Self {
        Self::Retry(RetryTelemetryEvent::Wait(wait))
    }
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

/// Metadata-only record for one gate at startup.
///
/// `version`, `phases`, and `actions` are null or empty when `describe`
/// failed. The record carries no gate text.
#[derive(Debug, Clone, Serialize)]
pub struct GateStartupTelemetry {
    pub gate: String,
    pub version: Option<String>,
    pub mode: String,
    pub phases: Vec<String>,
    pub actions: Vec<String>,
    /// `ready` or `failed`.
    pub outcome: String,
    pub failure_class: Option<String>,
    pub latency_ms: u64,
}

/// Metadata-only record for one gate's participation in one check.
///
/// A bypass has no gate: `gate` and `decision` are null, `bypassed` is true,
/// and `disabled` is false. A gate failure records no decision and carries the
/// bounded class. A gate disabled by the circuit breaker records `disabled`
/// and `failed_open` according to its `fail` mode.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the flags are independent telemetry facts from the protocol record; a bypass, a
              failed-open check, an override, and a disabled gate are separate states"
)]
#[derive(Debug, Clone, Serialize)]
pub struct GateDecisionTelemetry {
    pub gate: Option<String>,
    pub version: Option<String>,
    pub mode: String,
    pub phase: String,
    pub action: String,
    pub decision: Option<String>,
    pub code: Option<String>,
    pub latency_ms: u64,
    pub overridden: bool,
    pub bypassed: bool,
    pub failed_open: bool,
    pub disabled: bool,
    pub failure_class: Option<String>,
    /// The originating tool-call identifier, held raw in memory and serialized
    /// only as the one-way digest, matching `judge_attempt.call_id`.
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_digested_optional"
    )]
    pub call_id: Option<String>,
}

/// One gate-declared evidence payload. Evidence is the only gate-controlled
/// data that reaches telemetry.
#[derive(Debug, Clone, Serialize)]
pub struct GateEvidenceTelemetry {
    pub gate: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_digested_optional"
    )]
    pub call_id: Option<String>,
    pub kind: String,
    pub data: serde_json::Value,
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
    /// Compatibility projection of a current generic gate verdict on a Bash
    /// command. `detail` is `block:<code>`, `warn:<code>`, or `allow`;
    /// `latency_ms` is the pipeline latency; `overridden` is set when an
    /// allowlist entry overrode the decision.
    JudgeVerdict,
    /// Compatibility projection of a current gate failure that blocked the
    /// command. `detail` names the failure class.
    JudgeFailClosed,
    /// Compatibility projection of an explicit gate bypass (`CAKE_GATE=off`,
    /// `--no-gate`, or a legacy disablement), so the command ran ungated.
    /// Emitted per bypassed call so the escape hatch cannot be used silently.
    JudgeBypass,
    /// Emitted when a call's start was delayed behind an earlier call
    /// targeting the same canonical path; reads and mutations both qualify.
    /// `detail` is that path.
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
    /// Kind-specific detail; see [`CompensationKind`]. Bounded at
    /// serialization: the same-path and truncation kinds carry paths and tool
    /// names, which are configuration- or tool-controlled rather than
    /// provider-controlled, but the record's size stays bounded regardless.
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_bounded_optional"
    )]
    pub detail: Option<String>,
    /// Gate verdict duration, when the event is a compatibility verdict.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    /// Set on a compatibility verdict event when an allowlist entry overrode a
    /// block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overridden: Option<bool>,
    /// One-way digest of the transcript tool call this event belongs to.
    ///
    /// The field holds the transcript's raw `call_id` in memory and serializes
    /// only its SHA-256 hex digest, so pairing a call with its gate outcome
    /// cannot pull provider-controlled text into telemetry. A compatibility
    /// event carries the same digest as the `gate_decision.call_id` for the
    /// call, which is what makes the session-metrics join deterministic
    /// (#404). Compensations that are not tied to one tool call leave it unset.
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_digested_optional"
    )]
    pub call_id: Option<String>,
}

impl CompensationEventTelemetry {
    pub const fn new(kind: CompensationKind, detail: Option<String>) -> Self {
        Self {
            kind,
            detail,
            latency_ms: None,
            overridden: None,
            call_id: None,
        }
    }

    /// Build the compatibility verdict event for a generic gate decision:
    /// decision and code (block and warn carry a code; allow omits it), the
    /// pipeline latency, and whether an allowlist entry overrode the decision.
    /// Metadata only — never the command or reason text.
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
            call_id: None,
        }
    }

    /// Build the compatibility fail-closed event for a gate failure: the
    /// failure class that blocked the command instead of a verdict.
    pub fn judge_fail_closed(error_class: &str) -> Self {
        Self {
            kind: CompensationKind::JudgeFailClosed,
            detail: Some(error_class.to_string()),
            latency_ms: None,
            overridden: None,
            call_id: None,
        }
    }

    /// Build the compatibility bypass event for an explicit gate bypass. One
    /// event per bypassed call.
    pub const fn judge_bypass() -> Self {
        Self {
            kind: CompensationKind::JudgeBypass,
            detail: None,
            latency_ms: None,
            overridden: None,
            call_id: None,
        }
    }

    /// Attach the transcript call this event belongs to.
    ///
    /// `raw_call_id` is the transcript's raw tool-call identifier, retained
    /// here so a live operation can correlate with it; serialization writes it
    /// only as the digest [`digest_identifier`] produces, so the raw identifier
    /// is never persisted. An absent or empty identifier attaches nothing,
    /// which is the case for a call with no provider-assigned id.
    #[must_use]
    pub fn with_call_id(mut self, raw_call_id: Option<&str>) -> Self {
        self.call_id = match raw_call_id {
            Some(call_id) if !call_id.is_empty() => Some(call_id.to_string()),
            _ => None,
        };
        self
    }
}

/// Persist a telemetry identifier only as a one-way digest.
///
/// Provider- and tool-controlled identifiers (a provider request id, an
/// originating tool-call id) reach the sidecar only through this function, so
/// the raw value never enters telemetry; a consumer correlates a record with
/// its source by hashing the known raw value the same way. A gate decision and
/// its compatibility compensation event for one Bash call therefore carry the
/// same digest, which is what makes the session-metrics join deterministic
/// (#404).
#[must_use]
pub fn digest_identifier(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}

/// Maximum bytes retained for one provider-supplied metadata string.
///
/// The bound applies at serialization, so an oversized provider error, message,
/// or detail value cannot make a telemetry record arbitrarily large; the
/// in-memory value keeps its full length for user-facing error text.
pub const METADATA_MAX_BYTES: usize = 1_024;

/// The marker appended to a metadata string cut by [`bound_metadata`].
///
/// ASCII, so the byte budget and downstream `jq`/`grep` handling stay trivial,
/// and counted inside [`METADATA_MAX_BYTES`].
const METADATA_TRUNCATION_MARKER: &str = "...[truncated]";

/// Bound one provider-supplied metadata string for serialization.
///
/// A value at or under [`METADATA_MAX_BYTES`] is returned unchanged; a longer
/// value is cut on the last UTF-8 boundary that leaves room for
/// [`METADATA_TRUNCATION_MARKER`]. The result is therefore always at most
/// [`METADATA_MAX_BYTES`] bytes and never splits a code point.
#[must_use]
pub fn bound_metadata(value: &str) -> String {
    if value.len() <= METADATA_MAX_BYTES {
        return value.to_string();
    }
    let budget = METADATA_MAX_BYTES - METADATA_TRUNCATION_MARKER.len();
    let mut bounded = String::with_capacity(METADATA_MAX_BYTES);
    for character in value.chars() {
        if bounded.len() + character.len_utf8() > budget {
            break;
        }
        bounded.push(character);
    }
    bounded.push_str(METADATA_TRUNCATION_MARKER);
    bounded
}

/// Reduce a configured provider endpoint to a bounded, credential-free form
/// for serialization.
///
/// Userinfo, query, and fragment are removed before the byte bound is applied,
/// so a credential embedded in any of those components never reaches a
/// telemetry record. A value that is not a parsable URL, or whose userinfo
/// cannot be cleared, yields `None` and serializes as `null` rather than
/// persisting the raw string: an unparsable value cannot be proven
/// credential-free. The retained path is endpoint identity (for example
/// `https://host/v1`), not a credential surface.
#[must_use]
pub fn sanitize_endpoint(value: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(value).ok()?;
    url.set_username("").ok()?;
    url.set_password(None).ok()?;
    url.set_query(None);
    url.set_fragment(None);
    Some(bound_metadata(url.as_str()))
}

/// Serialize a boolean only when it is true.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if contract passes &T"
)]
const fn is_false(value: &bool) -> bool {
    !*value
}

/// Serialize an optional provider-supplied string through [`bound_metadata`].
#[expect(
    clippy::ref_option,
    reason = "serde's serialize_with contract passes &T for a field of type T"
)]
fn serialize_bounded_optional<S>(value: &Option<String>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match value {
        Some(text) => serializer.serialize_some(&bound_metadata(text)),
        None => serializer.serialize_none(),
    }
}

/// Serialize a required provider-supplied string through [`bound_metadata`].
fn serialize_bounded_required<S>(value: &str, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&bound_metadata(value))
}

/// Serialize an optional provider-controlled identifier as a one-way digest.
///
/// The field holds the raw identifier in memory so a live operation can
/// correlate with it; the record only ever carries the digest
/// [`digest_identifier`] produces. An absent or empty identifier serializes as
/// `null` rather than as the digest of an empty string, which every id-less
/// call would share.
#[expect(
    clippy::ref_option,
    reason = "serde's serialize_with contract passes &T for a field of type T"
)]
fn serialize_digested_optional<S>(value: &Option<String>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match value.as_deref() {
        Some(raw) if !raw.is_empty() => serializer.serialize_some(&digest_identifier(raw)),
        _ => serializer.serialize_none(),
    }
}

/// Serialize an optional configured endpoint through [`sanitize_endpoint`].
#[expect(
    clippy::ref_option,
    reason = "serde's serialize_with contract passes &T for a field of type T"
)]
fn serialize_endpoint_optional<S>(value: &Option<String>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match value.as_deref().and_then(sanitize_endpoint) {
        Some(endpoint) => serializer.serialize_some(&endpoint),
        None => serializer.serialize_none(),
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
        model: String,
        api_type: ApiType,
        output_format: OutputFormat,
        tools: Vec<String>,
        settings: SessionTelemetrySettings,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        build_commit: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        build_dirty: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_trust: Option<ProjectTrustTelemetry>,
        /// Sanitized provider identity resolved for this invocation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_identity: Option<Box<ProviderIdentityTelemetry>>,
        /// Set when a resume or fork accepted a changed provider identity for
        /// this invocation only.
        #[serde(default, skip_serializing_if = "is_false")]
        session_model_change_override: bool,
        /// Set when an explicit flag accepted a different working directory.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_rebind: Option<Box<ContextRebindTelemetry>>,
    },
    ApiAttemptInFlight {
        session_id: String,
        invocation_id: String,
        timestamp: DateTime<Utc>,
        #[serde(flatten)]
        attempt: ApiAttemptInFlightTelemetry,
    },
    ApiAttempt {
        session_id: String,
        invocation_id: String,
        timestamp: DateTime<Utc>,
        #[serde(flatten)]
        attempt: ApiAttemptTelemetry,
    },
    RetryScheduled {
        session_id: String,
        invocation_id: String,
        timestamp: DateTime<Utc>,
        #[serde(flatten)]
        retry: RetryScheduledTelemetry,
    },
    RetryWait {
        session_id: String,
        invocation_id: String,
        timestamp: DateTime<Utc>,
        #[serde(flatten)]
        wait: RetryWaitTelemetry,
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
    GateStartup {
        session_id: String,
        invocation_id: String,
        timestamp: DateTime<Utc>,
        #[serde(flatten)]
        startup: GateStartupTelemetry,
    },
    GateDecision {
        session_id: String,
        invocation_id: String,
        timestamp: DateTime<Utc>,
        #[serde(flatten)]
        decision: GateDecisionTelemetry,
    },
    GateEvidence {
        session_id: String,
        invocation_id: String,
        timestamp: DateTime<Utc>,
        #[serde(flatten)]
        evidence: GateEvidenceTelemetry,
    },
    SessionSummary {
        session_id: String,
        invocation_id: String,
        timestamp: DateTime<Utc>,
        success: bool,
        duration_ms: u64,
        turn_count: u32,
        usage: Usage,
        /// The terminal error text for the invocation, bounded at serialization
        /// so a provider-supplied message cannot grow the record.
        #[serde(serialize_with = "serialize_bounded_optional")]
        error: Option<String>,
    },
}

pub struct SessionTelemetryWriter {
    writer: BufWriter<File>,
}

/// Bytes inspected per backward window when locating the final record boundary.
const TAIL_SCAN_CHUNK_BYTES: usize = 8 * 1024;

impl SessionTelemetryWriter {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .create(true)
            .append(true)
            .open(path)?;
        truncate_unterminated_tail(&file, path)?;
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

/// Restore an NDJSON record boundary in a sidecar whose last write was
/// interrupted.
///
/// The only recoverable framing failure is an unterminated final segment: the
/// bytes after the last newline, or the whole file when it has no newline. That
/// segment may be a record the previous writer never finished, so it is
/// truncated before the file is reopened for append. A framed record is one
/// terminated by a newline; an unterminated segment is therefore never counted
/// as complete, even when its bytes happen to parse.
///
/// A newline-terminated line is a complete record whatever its contents. A
/// malformed one is left byte-for-byte untouched and reported by consumers,
/// because no recovery rule can tell where the writer meant to stop; guessing
/// could discard a framed record.
fn truncate_unterminated_tail(file: &File, path: &Path) -> anyhow::Result<()> {
    let len = file.metadata()?.len();
    let tail_len = unterminated_tail_len(file, len)?;
    if tail_len == 0 {
        return Ok(());
    }

    tracing::warn!(
        target: "cake",
        "Truncating {tail_len} incomplete byte(s) from the final record of telemetry sidecar {}",
        path.display()
    );
    file.set_len(len - tail_len)?;
    Ok(())
}

/// Return the length of the bytes after the final newline, or the whole file
/// when it has no newline. Reads backwards in windows so a large sidecar is not
/// buffered whole.
fn unterminated_tail_len(file: &File, len: u64) -> std::io::Result<u64> {
    let chunk_bytes = u64::try_from(TAIL_SCAN_CHUNK_BYTES).unwrap_or(u64::MAX);
    let mut end = len;
    while end > 0 {
        let start = end.saturating_sub(chunk_bytes);
        let chunk_len = usize::try_from(end - start).unwrap_or(TAIL_SCAN_CHUNK_BYTES);
        let mut chunk = vec![0; chunk_len];
        let mut handle = file;
        handle.seek(SeekFrom::Start(start))?;
        handle.read_exact(&mut chunk)?;
        if let Some(index) = chunk.iter().rposition(|byte| *byte == b'\n') {
            return Ok(len - (start + index as u64 + 1));
        }
        end = start;
    }
    Ok(len)
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
/// The agent loop and the gate telemetry sink share one instance, so a write
/// failure on either path disables telemetry for both: once any append fails,
/// no further records are written and a partial NDJSON line is never followed
/// by more records. Availability and the failure transition are one
/// mutex-wrapped state, so the pair cannot drift: exactly one caller observes
/// `Failed`, and every later caller observes `Disabled`.
pub struct SharedSessionTelemetryWriter {
    state: Mutex<TelemetryWriterState>,
}

/// Whether the shared writer is still accepting records.
///
/// `Disabled` is terminal; it is never written again once set, so every caller
/// after a failure observes `Disabled`.
enum TelemetryWriterState {
    Active(SessionTelemetryWriter),
    Disabled,
}

impl TelemetryWriterState {
    fn append(&mut self, record: &SessionTelemetryRecord) -> Option<anyhow::Result<()>> {
        match self {
            Self::Active(writer) => Some(writer.append(record)),
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

/// Shared handle that appends generic gate records to the session sidecar.
///
/// The pipeline writes one `gate_startup` record per gate at startup and one
/// `gate_decision` record for every gate that participated in a check. Current
/// decisions also emit compatibility `judge_*` compensation events; historical
/// `judge_attempt` and `type_safe_shadow` records remain readable but are not
/// produced.
#[derive(Clone)]
pub struct GateTelemetrySink {
    writer: Arc<SharedSessionTelemetryWriter>,
    context: SessionTelemetryContext,
}

impl GateTelemetrySink {
    pub const fn new(
        writer: Arc<SharedSessionTelemetryWriter>,
        context: SessionTelemetryContext,
    ) -> Self {
        Self { writer, context }
    }

    /// Append one startup record. Metadata only; gate text never enters it.
    pub fn record_startup(&self, startup: GateStartupTelemetry) {
        let record = SessionTelemetryRecord::GateStartup {
            session_id: self.context.session_id.clone(),
            invocation_id: self.context.invocation_id.clone(),
            timestamp: Utc::now(),
            startup,
        };
        if let TelemetryAppend::Failed(error) = self.writer.append(&record) {
            tracing::warn!(target: "cake", "Failed to record gate startup: {error}");
        }
    }

    /// Append one decision record. A bypass carries `gate: None` and
    /// `decision: None`; a disabled or failed gate carries no decision either.
    pub fn record_decision(&self, decision: GateDecisionTelemetry) {
        let record = SessionTelemetryRecord::GateDecision {
            session_id: self.context.session_id.clone(),
            invocation_id: self.context.invocation_id.clone(),
            timestamp: Utc::now(),
            decision,
        };
        if let TelemetryAppend::Failed(error) = self.writer.append(&record) {
            tracing::warn!(target: "cake", "Failed to record gate decision: {error}");
        }
    }

    /// Append one bounded evidence record. An oversized payload is replaced by
    /// one `truncated` record, matching the protocol's evidence bound.
    pub fn record_evidence(&self, mut evidence: GateEvidenceTelemetry) {
        let bytes = serde_json::to_vec(&evidence.data).map_or(usize::MAX, |data| data.len());
        if bytes > MAX_GATE_EVIDENCE_BYTES {
            evidence.kind = "truncated".to_string();
            evidence.data = serde_json::json!({
                "dropped_items": 1,
                "dropped_bytes": bytes,
            });
        }
        let record = SessionTelemetryRecord::GateEvidence {
            session_id: self.context.session_id.clone(),
            invocation_id: self.context.invocation_id.clone(),
            timestamp: Utc::now(),
            evidence,
        };
        if let TelemetryAppend::Failed(error) = self.writer.append(&record) {
            tracing::warn!(target: "cake", "Failed to record gate evidence: {error}");
        }
    }
}

impl std::fmt::Debug for GateTelemetrySink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GateTelemetrySink")
            .field("context", &self.context)
            .finish_non_exhaustive()
    }
}

/// Maximum serialized bytes of one gate evidence payload.
const MAX_GATE_EVIDENCE_BYTES: usize = 8 * 1024;
