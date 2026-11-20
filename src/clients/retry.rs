use std::time::Duration;

use anyhow::Error;
use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap, RETRY_AFTER};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::debug;

use crate::clients::response_stall::ResponseStall;

const DEFAULT_MAX_RETRIES: u32 = 5;
const DEFAULT_BASE_DELAY_MS: u64 = 500;
const DEFAULT_MAX_BACKOFF_MS: u64 = 30_000;
const DEFAULT_JITTER_DIVISOR: u64 = 5;
const CONTEXT_OVERFLOW_SAFETY_BUFFER_TOKENS: u32 = 1024;
const MIN_CONTEXT_OVERFLOW_OUTPUT_TOKENS: u32 = 256;
const X_SHOULD_RETRY: &str = "x-should-retry";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_delay: Duration,
    pub max_backoff: Duration,
    pub jitter_divisor: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_MAX_RETRIES,
            base_delay: Duration::from_millis(DEFAULT_BASE_DELAY_MS),
            max_backoff: Duration::from_millis(DEFAULT_MAX_BACKOFF_MS),
            jitter_divisor: DEFAULT_JITTER_DIVISOR,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestOverrides {
    pub max_output_tokens: Option<u32>,
    pub reasoning_max_tokens: Option<u32>,
    pub context_overflow_retry_used: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryReason {
    RateLimit,
    Overloaded,
    ServerError,
    RequestTimeout,
    LockTimeout,
    Network,
    ContextOverflow,
    SemanticIncomplete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryStatus {
    pub attempt: u32,
    pub max_retries: u32,
    pub delay: Duration,
    pub reason: RetryReason,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct HttpFailure {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryDecision {
    Retry {
        status: RetryStatus,
    },
    RetryWithOverrides {
        status: RetryStatus,
        overrides: RequestOverrides,
    },
    DoNotRetry,
}

/// A provider's typed input-overflow rejection.
///
/// The provider reports the request's input size, the output allowance it was
/// asked for, and the total context limit. One value drives both the
/// output-allowance retry and the termination classifier, so the retry path and
/// the pressure verdict cannot disagree about what fits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputOverflow {
    pub input_tokens: u32,
    pub requested_tokens: u32,
    pub context_limit: u32,
}

impl InputOverflow {
    /// Output tokens left after the input and the safety buffer.
    pub const fn available_output(self) -> u32 {
        self.context_limit
            .saturating_sub(self.input_tokens)
            .saturating_sub(CONTEXT_OVERFLOW_SAFETY_BUFFER_TOKENS)
    }

    /// Whether reducing the output allowance can still fit the request. When it
    /// cannot, the request's input itself does not fit: that is genuine input
    /// overflow, not an output-allowance mismatch.
    pub const fn fits_with_reduced_output(self) -> bool {
        self.available_output() >= MIN_CONTEXT_OVERFLOW_OUTPUT_TOKENS
    }

    /// The output allowance the retry sends: what is available, capped by what
    /// the request originally asked for.
    pub fn reduced_output(self) -> u32 {
        self.available_output().min(self.requested_tokens)
    }
}

struct RetryClassification {
    reason: RetryReason,
    detail: String,
}

pub fn classify_http_failure(
    policy: &RetryPolicy,
    failure: &HttpFailure,
    attempt: u32,
    session_id: uuid::Uuid,
    current_overrides: &RequestOverrides,
) -> RetryDecision {
    if attempt >= policy.max_retries {
        return RetryDecision::DoNotRetry;
    }

    if let Some((status, overrides)) =
        context_overflow_retry(policy, failure, attempt, current_overrides)
    {
        return RetryDecision::RetryWithOverrides { status, overrides };
    }

    let x_should_retry = parse_x_should_retry(&failure.headers);
    if x_should_retry == Some(false) {
        return RetryDecision::DoNotRetry;
    }

    let is_overloaded = has_overloaded_marker(&failure.body);

    classify_retryable_status(
        policy,
        failure,
        attempt,
        session_id,
        x_should_retry,
        is_overloaded,
    )
}

fn classify_retryable_status(
    policy: &RetryPolicy,
    failure: &HttpFailure,
    attempt: u32,
    session_id: uuid::Uuid,
    x_should_retry: Option<bool>,
    is_overloaded: bool,
) -> RetryDecision {
    let Some(classification) = retry_classification(failure.status, x_should_retry, is_overloaded)
    else {
        return RetryDecision::DoNotRetry;
    };

    RetryDecision::Retry {
        status: retry_status(
            policy,
            attempt,
            session_id,
            classification.reason,
            classification.detail,
            &failure.headers,
        ),
    }
}

fn retry_classification(
    status: u16,
    x_should_retry: Option<bool>,
    is_overloaded: bool,
) -> Option<RetryClassification> {
    let classification = match status {
        408 => fixed_retry_classification(RetryReason::RequestTimeout, "408 request timeout"),
        409 => fixed_retry_classification(RetryReason::LockTimeout, "409 lock timeout"),
        429 => fixed_retry_classification(RetryReason::RateLimit, "429 rate limit"),
        529 => overloaded_retry_classification(status),
        500 | 502 | 503 | 504 if is_overloaded => overloaded_retry_classification(status),
        500 | 502 | 503 | 504 => server_error_retry_classification(status),
        _ if is_overloaded => overloaded_retry_classification(status),
        _ if x_should_retry == Some(true) && (500..600).contains(&status) => {
            server_error_retry_classification(status)
        },
        _ => return None,
    };

    Some(classification)
}

fn fixed_retry_classification(reason: RetryReason, detail: &str) -> RetryClassification {
    RetryClassification {
        reason,
        detail: detail.to_string(),
    }
}

fn overloaded_retry_classification(status: u16) -> RetryClassification {
    let detail = if matches!(status, 400 | 500 | 502 | 503 | 504) {
        "overloaded provider".to_string()
    } else {
        format!("{status} overloaded provider")
    };

    RetryClassification {
        reason: RetryReason::Overloaded,
        detail,
    }
}

fn server_error_retry_classification(status: u16) -> RetryClassification {
    RetryClassification {
        reason: RetryReason::ServerError,
        detail: format!("{status} server error"),
    }
}

pub fn classify_transport_error(
    policy: &RetryPolicy,
    error: &Error,
    attempt: u32,
    session_id: uuid::Uuid,
) -> RetryDecision {
    if attempt >= policy.max_retries {
        return RetryDecision::DoNotRetry;
    }

    let Some(detail) = transport_retry_detail(error) else {
        return RetryDecision::DoNotRetry;
    };

    let reason = if error.downcast_ref::<ResponseStall>().is_some() {
        RetryReason::RequestTimeout
    } else {
        RetryReason::Network
    };

    RetryDecision::Retry {
        status: RetryStatus {
            attempt: attempt + 1,
            max_retries: policy.max_retries,
            delay: fallback_delay(policy, attempt, session_id),
            reason,
            detail,
        },
    }
}

/// Classify a terminal `response.failed` event received after an accepted HTTP
/// 2xx on the Responses API.
///
/// Error `code` is authoritative when present; `type` is a compatibility
/// fallback only when `code` is absent. Only `server_error` is treated as
/// transient and eligible for bounded retry. Authentication, invalid-request,
/// quota, context, policy, and other semantic failures are not retried because
/// re-issuing the same request cannot change the outcome. `code` and
/// `error_type` come from the bounded metadata extracted by the Responses
/// parser; passing them as primitives keeps this module decoupled from the
/// provider-specific extraction.
pub fn classify_response_failed(
    policy: &RetryPolicy,
    code: Option<&str>,
    error_type: Option<&str>,
    attempt: u32,
    session_id: uuid::Uuid,
) -> RetryDecision {
    if attempt >= policy.max_retries {
        return RetryDecision::DoNotRetry;
    }

    let is_server_error = code.or(error_type) == Some("server_error");
    if !is_server_error {
        return RetryDecision::DoNotRetry;
    }

    RetryDecision::Retry {
        status: RetryStatus {
            attempt: attempt + 1,
            max_retries: policy.max_retries,
            delay: fallback_delay(policy, attempt, session_id),
            reason: RetryReason::ServerError,
            detail: "response.failed server_error".to_string(),
        },
    }
}

pub fn should_disable_connection_reuse(error: &Error) -> bool {
    transport_retry_detail(error).is_some()
}

/// Recognize a Responses failure code that proves the provider rejected the
/// request's context length.
///
/// `code` is authoritative when present; `error_type` is a compatibility
/// fallback only when `code` is absent. A present but unrecognized code never
/// falls through to `error_type`, matching [`classify_response_failed`].
pub fn context_failure_code(code: Option<&str>, error_type: Option<&str>) -> Option<&'static str> {
    match code.or(error_type)? {
        "context_length_exceeded" => Some("context_length_exceeded"),
        "input_too_long" => Some("input_too_long"),
        _ => None,
    }
}

/// Whether a provider failure status can carry the typed input-overflow form.
///
/// Providers return the form on `400` only. Sharing the gate keeps a quota,
/// auth, or rate-limit body that happens to quote overflow numbers from being
/// read as an input overflow, whether by the retry path or by the termination
/// classifier.
pub const fn is_input_overflow_status(status: u16) -> bool {
    status == 400
}

/// Classify a context-overflow failure and derive the complete retry
/// transition — status plus token-budget overrides — in a single pass over
/// the failure body. Returns `None` when the failure is not an overflow, the
/// overflow was already compensated once, or the remaining output budget is
/// too small to retry.
fn context_overflow_retry(
    policy: &RetryPolicy,
    failure: &HttpFailure,
    attempt: u32,
    current_overrides: &RequestOverrides,
) -> Option<(RetryStatus, RequestOverrides)> {
    if !is_input_overflow_status(failure.status) || current_overrides.context_overflow_retry_used {
        return None;
    }

    let overflow = parse_input_overflow(&failure.body)?;
    if !overflow.fits_with_reduced_output() {
        return None;
    }

    let max_output_tokens = overflow.reduced_output();

    let status = RetryStatus {
        attempt: attempt + 1,
        max_retries: policy.max_retries,
        delay: Duration::ZERO,
        reason: RetryReason::ContextOverflow,
        detail: format!("max_output_tokens={max_output_tokens}"),
    };
    let overrides = RequestOverrides {
        max_output_tokens: Some(max_output_tokens),
        reasoning_max_tokens: current_overrides
            .reasoning_max_tokens
            .map(|budget| budget.min(max_output_tokens.saturating_sub(1))),
        context_overflow_retry_used: true,
    };

    Some((status, overrides))
}

fn retry_status(
    policy: &RetryPolicy,
    attempt: u32,
    session_id: uuid::Uuid,
    reason: RetryReason,
    detail: String,
    headers: &HeaderMap,
) -> RetryStatus {
    let delay = parse_retry_after(headers)
        .unwrap_or_else(|| fallback_delay(policy, attempt, session_id))
        .min(policy.max_backoff);

    RetryStatus {
        attempt: attempt + 1,
        max_retries: policy.max_retries,
        delay,
        reason,
        detail,
    }
}

fn fallback_delay(policy: &RetryPolicy, attempt: u32, session_id: uuid::Uuid) -> Duration {
    let base_delay_ms = capped_backoff_ms(policy, attempt);
    let max_backoff_ms = millis_u64(policy.max_backoff);
    let jitter_bound_ms = if base_delay_ms >= max_backoff_ms || policy.jitter_divisor == 0 {
        0
    } else {
        (base_delay_ms / policy.jitter_divisor).max(1)
    };
    let jitter_ms = deterministic_jitter_ms(session_id, attempt, jitter_bound_ms);
    Duration::from_millis(base_delay_ms.saturating_add(jitter_ms))
}

fn capped_backoff_ms(policy: &RetryPolicy, attempt: u32) -> u64 {
    let exponent = attempt.saturating_sub(1).min(31);
    millis_u64(policy.base_delay)
        .saturating_mul(1_u64 << exponent)
        .min(millis_u64(policy.max_backoff))
}

fn millis_u64(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn deterministic_jitter_ms(session_id: uuid::Uuid, attempt: u32, max_jitter_ms: u64) -> u64 {
    if max_jitter_ms == 0 {
        return 0;
    }

    let mut hasher = Sha256::new();
    hasher.update(session_id.as_bytes());
    hasher.update(attempt.to_be_bytes());
    let digest = hasher.finalize();

    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(bytes) % (max_jitter_ms + 1)
}

fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    parse_retry_after_at(headers, Utc::now())
}

fn parse_retry_after_at(headers: &HeaderMap, now: DateTime<Utc>) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();

    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    let retry_at = DateTime::parse_from_rfc2822(value)
        .ok()?
        .with_timezone(&Utc);
    let delay = retry_at.signed_duration_since(now);

    if delay <= chrono::Duration::zero() {
        return Some(Duration::ZERO);
    }

    delay.to_std().ok()
}

fn parse_x_should_retry(headers: &HeaderMap) -> Option<bool> {
    let value = headers.get(X_SHOULD_RETRY)?.to_str().ok()?.trim();

    if value.eq_ignore_ascii_case("true") {
        Some(true)
    } else if value.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

fn has_overloaded_marker(body: &str) -> bool {
    match serde_json::from_str::<Value>(body) {
        Ok(parsed) => has_structured_overloaded_signal(&parsed),
        Err(error) => {
            let has_marker = body.to_ascii_lowercase().contains("overloaded_error");
            if has_marker {
                debug!(
                    target: "cake",
                    %error,
                    "Classified overloaded retry from unstructured response body fallback"
                );
            }
            has_marker
        },
    }
}

fn has_structured_overloaded_signal(value: &Value) -> bool {
    ["/error/type", "/error/code", "/type", "/code"]
        .into_iter()
        .filter_map(|pointer| value.pointer(pointer))
        .filter_map(Value::as_str)
        .any(|field| field.eq_ignore_ascii_case("overloaded_error"))
}

/// Parse a provider's typed input-overflow error body.
///
/// The typed form names the input size, the requested output allowance, and the
/// context limit, for example `context limit: 12000 + 5000 > 16384`. Prose that
/// merely mentions a context limit is not an overflow.
pub fn parse_input_overflow(body: &str) -> Option<InputOverflow> {
    let owned_message = extract_error_message(body);
    let message = owned_message.as_deref().unwrap_or(body);
    let message_lower = message.to_ascii_lowercase();

    if !(message_lower.contains("context limit") && message_lower.contains("max_tokens")) {
        return None;
    }

    let (_, expression) = message_lower.split_once("context limit")?;
    let (input_expression, remainder) = expression.split_once('+')?;
    let (requested_expression, context_limit_expression) = remainder.split_once('>')?;

    let input_tokens = parse_last_u32(input_expression)?;
    let requested_tokens = parse_first_u32(requested_expression)?;
    let context_limit = parse_first_u32(context_limit_expression)?;

    Some(InputOverflow {
        input_tokens,
        requested_tokens,
        context_limit,
    })
}

fn extract_error_message(body: &str) -> Option<String> {
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok()?;

    parsed
        .pointer("/error/message")
        .and_then(serde_json::Value::as_str)
        .or_else(|| parsed.get("message").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
}

fn extract_u32_values(input: &str) -> Vec<u32> {
    let mut values = Vec::new();
    let mut digits = String::new();

    for ch in input.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
            continue;
        }

        if !digits.is_empty() {
            if let Ok(value) = digits.parse::<u32>() {
                values.push(value);
            }
            digits.clear();
        }
    }

    if !digits.is_empty()
        && let Ok(value) = digits.parse::<u32>()
    {
        values.push(value);
    }

    values
}

fn parse_first_u32(input: &str) -> Option<u32> {
    extract_u32_values(input).into_iter().next()
}

fn parse_last_u32(input: &str) -> Option<u32> {
    extract_u32_values(input).into_iter().last()
}

fn transport_retry_detail(error: &Error) -> Option<String> {
    if let Some(stall) = error.downcast_ref::<ResponseStall>() {
        return Some(stall.retry_detail());
    }

    stale_reqwest_retry_detail(error)
        .or_else(|| legacy_transport_retry_detail(error))
        .or_else(|| {
            is_request_phase_transport_error(error)
                .then_some("request transport failure".to_string())
        })
}

fn stale_reqwest_retry_detail(error: &Error) -> Option<String> {
    error.chain().find_map(|cause| {
        let reqwest_error = cause.downcast_ref::<reqwest::Error>()?;
        if !reqwest_error.is_request() || reqwest_error.status().is_some() {
            return None;
        }
        if reqwest_error.is_timeout() {
            return Some("stale connection timeout".to_string());
        }
        if reqwest_error.is_connect() {
            return Some("stale connection failure".to_string());
        }
        None
    })
}

fn legacy_transport_retry_detail(error: &Error) -> Option<String> {
    let lower = error
        .chain()
        .map(|cause| cause.to_string().to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" | ");

    if lower.contains("connection reset") {
        Some("stale connection reset".to_string())
    } else if lower.contains("broken pipe") {
        Some("stale broken pipe".to_string())
    } else if lower.contains("unexpected eof") || lower.contains("end of file") {
        Some("stale unexpected eof".to_string())
    } else if lower.contains("timed out") {
        Some("stale connection timeout".to_string())
    } else {
        None
    }
}

fn is_request_phase_transport_error(error: &Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<reqwest::Error>()
            .is_some_and(|reqwest_error| {
                reqwest_error.is_request() && reqwest_error.status().is_none()
            })
    })
}
