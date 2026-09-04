//! Per-attempt observability and bounded recovery for the command-safety judge.

use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tokio::time::sleep;

use crate::clients::agent_runner::build_http_client;
use crate::clients::backend::{Backend, ResponseDecodeError};
use crate::clients::retry::{self, HttpFailure, RequestOverrides, RetryReason};
use crate::config::model::ApiType;
use crate::session_telemetry::{
    JudgeAttemptTelemetry, JudgeAttemptTerminalClass, ProviderTermination, RetryReasonSnapshot,
};
use crate::types::{ConversationItem, Role};

use crate::clients::judge::{
    JudgeClient, JudgeDiagnostic, JudgeError, JudgeRequest, JudgeVerdict, assistant_message,
    build_judge_history, parse_verdict, refusal_error,
};

/// Session id used only for the judge retry's deterministic jitter. The judge
/// does not carry session identity; a nil id keeps the small backoff jitter
/// stable across calls and tests.
const JITTER_SESSION_ID: uuid::Uuid = uuid::Uuid::nil();

/// The full observed judge evaluation: the final result plus every provider
/// attempt (one, or two after a bounded recovery).
pub(super) struct JudgeCall {
    pub(super) result: Result<JudgeVerdict, JudgeError>,
    /// Every provider attempt in order.
    pub(super) attempts: Vec<JudgeAttemptTelemetry>,
    pub(super) diagnostic: Option<JudgeDiagnostic>,
}

impl JudgeCall {
    const fn new(
        result: Result<JudgeVerdict, JudgeError>,
        attempts: Vec<JudgeAttemptTelemetry>,
        diagnostic: Option<JudgeDiagnostic>,
    ) -> Self {
        Self {
            result,
            attempts,
            diagnostic,
        }
    }
}

/// Outcome of one judge provider attempt.
struct AttemptCall {
    result: Result<JudgeVerdict, JudgeError>,
    attempt: JudgeAttemptTelemetry,
    diagnostic: Option<JudgeDiagnostic>,
    /// Failure inputs the retry driver classifies. Absent on success and on
    /// terminal failures (refusal, malformed verdict, semantic response parse,
    /// request build), which are never retried.
    failure: Option<AttemptFailure>,
}

/// The raw failure inputs the retry driver classifies for a failed attempt.
enum AttemptFailure {
    /// The attempt exceeded its allowance.
    Timeout,
    /// The provider request failed at the transport layer.
    Transport(anyhow::Error),
    /// The provider returned a non-success HTTP response.
    Http(HttpFailure),
    /// The provider returned a 2xx whose body failed to decode into the
    /// expected JSON envelope. The cause may be upstream or transport-related;
    /// this class gets one bounded recovery on a fresh client.
    UndecodableResponse,
}

/// Per-attempt parameters chosen by the retry driver.
struct AttemptParams {
    /// Request/parse allowance for this attempt.
    budget: Duration,
    /// One-based attempt ordinal.
    attempt: u32,
    /// Why this attempt is a recovery, when it is one.
    retry_reason: Option<RetryReasonSnapshot>,
    /// Backoff wait before this attempt.
    retry_delay_ms: u64,
}

/// A bounded recovery decision for a failed judge attempt.
struct JudgeRetryDecision {
    reason: RetryReason,
    wait: Duration,
    budget: Duration,
    fresh_client: bool,
}

/// Run the complete judge evaluation: one bounded provider call, then at most
/// one recovery attempt within the operation deadline when the first call
/// failed with a timeout, retryable transport/HTTP error, or undecodable
/// successful response body.
///
/// The operation deadline is `client.timeout + client.retry_budget`. Attempt 1
/// keeps the full configured per-call allowance; the recovery attempt consumes
/// at most `min(client.timeout, remaining_after_wait)`. A recovery never
/// happens for a valid verdict, refusal, malformed verdict, or semantic
/// backend parse failure. An exhausted recovery fails closed with the final
/// attempt's error.
pub(super) async fn judge_observed(
    client: &JudgeClient,
    request: &JudgeRequest,
    include_raw_diagnostic: bool,
) -> JudgeCall {
    let operation_start = Instant::now();
    let deadline = client.timeout + client.retry_budget;
    let deadline_ms = duration_ms(deadline);
    // The judge carries no session identity, so each logical evaluation mints
    // its own fresh ID (shared across its at-most-one retry).
    let judge_session_id = uuid::Uuid::new_v4();

    let first_params = AttemptParams {
        budget: client.timeout,
        attempt: 1,
        retry_reason: None,
        retry_delay_ms: 0,
    };
    let first = run_attempt(
        client,
        request,
        include_raw_diagnostic,
        &first_params,
        deadline_ms,
        judge_session_id,
    )
    .await;

    let Some(decision) = classify_retry(client, first.failure.as_ref(), operation_start, deadline)
    else {
        return JudgeCall::new(first.result, vec![first.attempt], first.diagnostic);
    };

    // A stalled request may leave a bad pooled connection; recovery uses a
    // fresh client so the second request cannot inherit it, and later commands
    // stop paying for the stale pool (the swap replaces the stored client).
    if decision.fresh_client {
        *client
            .client
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = build_http_client(true);
    }
    if !decision.wait.is_zero() {
        sleep(decision.wait).await;
    }
    let second_params = AttemptParams {
        budget: decision.budget,
        attempt: 2,
        retry_reason: Some(RetryReasonSnapshot::from(&decision.reason)),
        retry_delay_ms: duration_ms(decision.wait),
    };
    let second = run_attempt(
        client,
        request,
        include_raw_diagnostic,
        &second_params,
        deadline_ms,
        judge_session_id,
    )
    .await;

    JudgeCall::new(
        second.result,
        vec![first.attempt, second.attempt],
        second.diagnostic,
    )
}

/// Run one bounded provider attempt and return its outcome.
async fn run_attempt(
    client: &JudgeClient,
    request: &JudgeRequest,
    include_raw_diagnostic: bool,
    params: &AttemptParams,
    effective_deadline_ms: u64,
    session_id: uuid::Uuid,
) -> AttemptCall {
    let observed = match ObservedJudgeCall::start(
        client,
        request,
        include_raw_diagnostic,
        params,
        effective_deadline_ms,
        session_id,
    ) {
        Ok(observed) => observed,
        Err(call) => return *call,
    };
    let (response, observed) = match observed.send(client).await {
        Ok(sent) => sent,
        Err(call) => return *call,
    };
    if response.status().is_success() {
        observed.finish_success(client, response).await
    } else {
        observed.finish_http_error(response).await
    }
}

/// Decide whether a failed first attempt may recover, against the operation
/// deadline. Returns `None` when recovery is disabled, the failure is not
/// retryable, or the wait would exhaust the remaining budget.
fn classify_retry(
    client: &JudgeClient,
    failure: Option<&AttemptFailure>,
    operation_start: Instant,
    deadline: Duration,
) -> Option<JudgeRetryDecision> {
    let failure = failure?;
    if client.retry_budget.is_zero() {
        return None;
    }
    let remaining = deadline.checked_sub(operation_start.elapsed())?;
    if remaining.is_zero() {
        return None;
    }

    let (reason, delay, fresh_client) = retry_classification(client, failure)?;

    let wait = delay.min(remaining);
    let remaining = remaining.checked_sub(wait)?;
    if remaining.is_zero() {
        return None;
    }
    Some(JudgeRetryDecision {
        reason,
        wait,
        budget: remaining.min(client.timeout),
        fresh_client,
    })
}

/// Classify a failed attempt into the recovery inputs: the retry reason, the
/// policy backoff delay, and whether recovery needs a fresh client (a stalled
/// request may have poisoned the pooled connection). Returns `None` when the
/// failure is not retryable.
fn retry_classification(
    client: &JudgeClient,
    failure: &AttemptFailure,
) -> Option<(RetryReason, Duration, bool)> {
    match failure {
        AttemptFailure::Timeout => {
            Some(transient_retry_inputs(client, RetryReason::RequestTimeout))
        },
        AttemptFailure::Transport(error) => transport_retry_inputs(client, error),
        AttemptFailure::Http(failure) => http_retry_inputs(client, failure),
        // The available evidence does not distinguish an upstream invalid
        // body from a connection-level cause. Recovery uses a fresh client so
        // connection reuse is excluded from the second attempt.
        AttemptFailure::UndecodableResponse => {
            Some(transient_retry_inputs(client, RetryReason::Network))
        },
    }
}

/// Recovery inputs for the transient failure classes (timeout, undecodable
/// response): the policy backoff wait and a fresh client with connection reuse
/// disabled, so the recovery cannot inherit a stalled request or a poisoned
/// pooled connection.
fn transient_retry_inputs(
    client: &JudgeClient,
    reason: RetryReason,
) -> (RetryReason, Duration, bool) {
    (
        reason,
        retry::policy_backoff_delay(&client.retry_policy, 1, JITTER_SESSION_ID),
        true,
    )
}

/// Recovery inputs for a transport failure classified retryable by the agent
/// runner's transport classifier (stale connection, reset, broken pipe).
fn transport_retry_inputs(
    client: &JudgeClient,
    error: &anyhow::Error,
) -> Option<(RetryReason, Duration, bool)> {
    match retry::classify_transport_error(&client.retry_policy, error, 1, JITTER_SESSION_ID) {
        retry::RetryDecision::Retry { status } => Some((
            status.reason,
            status.delay,
            retry::should_disable_connection_reuse(error),
        )),
        _ => None,
    }
}

/// Recovery inputs for an HTTP failure classified retryable by the agent
/// runner's HTTP classifier (rate limit, overload, server error).
fn http_retry_inputs(
    client: &JudgeClient,
    failure: &HttpFailure,
) -> Option<(RetryReason, Duration, bool)> {
    match retry::classify_http_failure(
        &client.retry_policy,
        failure,
        1,
        JITTER_SESSION_ID,
        &RequestOverrides::default(),
    ) {
        retry::RetryDecision::Retry { status } => Some((status.reason, status.delay, false)),
        _ => None,
    }
}

struct ObservedJudgeCall {
    backend: Backend,
    total_start: Instant,
    budget: Duration,
    request_json: Vec<u8>,
    attempt: JudgeAttemptTelemetry,
    diagnostic: Option<JudgeDiagnostic>,
    /// The resolved API key, applied to the config-controlled model identifier.
    api_key: String,
    session_id: uuid::Uuid,
}

impl ObservedJudgeCall {
    fn start(
        client: &JudgeClient,
        request: &JudgeRequest,
        include_raw_diagnostic: bool,
        params: &AttemptParams,
        effective_deadline_ms: u64,
        session_id: uuid::Uuid,
    ) -> Result<Self, Box<AttemptCall>> {
        let total_start = Instant::now();
        let build_start = Instant::now();
        let history = build_judge_history(request, client.user_rubric.as_deref());
        let backend = Backend::from_api_type(client.config.model_config.api_type);
        let mut attempt = initial_attempt(client, request, &history, params, effective_deadline_ms);
        let request_json = backend.build_request_json(
            &client.config,
            &history,
            &[],
            &RequestOverrides::default(),
            None,
        );
        attempt.request_build_ms = elapsed_ms(build_start);
        let request_json = request_json.map_err(|error| {
            attempt.total_ms = elapsed_ms(total_start);
            Box::new(AttemptCall::build_failed(attempt.clone(), None, error))
        })?;
        let diagnostic =
            include_raw_diagnostic.then(|| initial_diagnostic(client, &history, &request_json));
        Ok(Self {
            backend,
            total_start,
            budget: params.budget,
            request_json,
            attempt,
            diagnostic,
            api_key: client.config.api_key.clone(),
            session_id,
        })
    }

    async fn send(
        mut self,
        client: &JudgeClient,
    ) -> Result<(reqwest::Response, Self), Box<AttemptCall>> {
        let request_start = Instant::now();
        // Cloning the client is a cheap Arc bump; the lock guard drops before
        // the await so no lock is held across the request. A poisoned lock is
        // recovered rather than panicking so a panicked judge task cannot take
        // the gate down.
        let http = client
            .client
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let result = tokio::time::timeout(
            self.remaining(self.budget),
            self.backend.send_request_json(
                &http,
                &client.config,
                self.session_id,
                std::mem::take(&mut self.request_json),
            ),
        )
        .await;
        self.attempt.request_ms = elapsed_ms(request_start);
        match result {
            Err(_) => Err(Box::new(self.timeout())),
            Ok(Err(error)) => Err(Box::new(self.transport(error))),
            Ok(Ok(response)) => {
                self.attempt.status_code = Some(response.status().as_u16());
                self.attempt.provider_request_id = provider_request_id(response.headers());
                Ok((response, self))
            },
        }
    }

    async fn finish_http_error(mut self, response: reqwest::Response) -> AttemptCall {
        let status = response.status();
        let headers = response.headers().clone();
        let parse_start = Instant::now();
        let body = tokio::time::timeout(self.remaining(self.budget), response.text()).await;
        self.attempt.response_parse_ms = elapsed_ms(parse_start);
        match body {
            Err(_) => self.timeout(),
            Ok(body) => {
                let body = body.unwrap_or_default();
                self.attempt.terminal_class = JudgeAttemptTerminalClass::HttpError;
                self.finish(
                    Err(JudgeError::Transport {
                        status: Some(status.as_u16()),
                        detail: format!("HTTP {status}: {body}"),
                    }),
                    Some(AttemptFailure::Http(HttpFailure {
                        status: status.as_u16(),
                        headers,
                        body,
                    })),
                )
            },
        }
    }

    async fn finish_success(
        mut self,
        client: &JudgeClient,
        response: reqwest::Response,
    ) -> AttemptCall {
        let parse_start = Instant::now();
        let turn = tokio::time::timeout(
            self.remaining(self.budget),
            self.backend.parse_response(response),
        )
        .await;
        self.attempt.response_parse_ms = elapsed_ms(parse_start);
        match turn {
            Err(_) => self.timeout(),
            Ok(Err(error)) => {
                self.attempt.terminal_class = JudgeAttemptTerminalClass::ResponseParse;
                // `{:#}` renders the anyhow cause chain, so a typed body-decode
                // failure retains its serde cause in the fail-closed detail.
                let detail = format!("{error:#}");
                let failure = error
                    .downcast_ref::<ResponseDecodeError>()
                    .is_some()
                    .then_some(AttemptFailure::UndecodableResponse);
                self.finish(
                    Err(JudgeError::Transport {
                        status: None,
                        detail,
                    }),
                    failure,
                )
            },
            Ok(Ok(turn)) => self.finish_turn(client, &turn),
        }
    }

    fn finish_turn(
        mut self,
        client: &JudgeClient,
        turn: &crate::clients::agent::TurnResult,
    ) -> AttemptCall {
        self.attempt.usage = turn.usage;
        self.attempt.termination.clone_from(&turn.termination);
        if self.attempt.provider_request_id.is_none() {
            self.attempt
                .provider_request_id
                .clone_from(&turn.provider_request_id);
        }
        let content = assistant_message(&turn.items).map(str::to_string);
        update_diagnostic(&mut self.diagnostic, client, turn, content.as_deref());
        let no_verdict_text = content.as_deref().is_none_or(str::is_empty);
        if let Some(error) = refusal_error(turn.termination.as_ref(), no_verdict_text) {
            self.attempt.terminal_class = JudgeAttemptTerminalClass::Refusal;
            return self.finish(Err(error), None);
        }

        let verdict_start = Instant::now();
        let result = content.as_deref().map_or_else(
            || {
                Err(JudgeError::Malformed(
                    "judge response contained no assistant message".to_string(),
                ))
            },
            parse_verdict,
        );
        self.attempt.verdict_parse_ms = elapsed_ms(verdict_start);
        self.attempt.terminal_class = if result.is_ok() {
            JudgeAttemptTerminalClass::Verdict
        } else {
            JudgeAttemptTerminalClass::MalformedVerdict
        };
        self.finish(result, None)
    }

    fn remaining(&self, timeout: Duration) -> Duration {
        timeout
            .checked_sub(self.total_start.elapsed())
            .unwrap_or(Duration::ZERO)
    }

    fn timeout(mut self) -> AttemptCall {
        let budget = self.budget;
        self.attempt.terminal_class = JudgeAttemptTerminalClass::Timeout;
        self.finish(
            Err(JudgeError::Timeout(budget)),
            Some(AttemptFailure::Timeout),
        )
    }

    fn transport(mut self, error: anyhow::Error) -> AttemptCall {
        self.attempt.terminal_class = JudgeAttemptTerminalClass::Transport;
        self.finish(
            Err(JudgeError::Transport {
                status: None,
                detail: error.to_string(),
            }),
            Some(AttemptFailure::Transport(error)),
        )
    }

    fn finish(
        mut self,
        result: Result<JudgeVerdict, JudgeError>,
        failure: Option<AttemptFailure>,
    ) -> AttemptCall {
        self.attempt.total_ms = elapsed_ms(self.total_start);
        // The model identifier is config-controlled, never provider-returned, so
        // it is scrubbed only against the API key: scrubbing it against command
        // tokens would corrupt ordinary identifiers (for example the command
        // `go test` mangling the model `google/gemini`).
        self.attempt.model = redact_secret(&self.attempt.model, &self.api_key);
        // Strictly bound the provider-controlled fields: identifiers are
        // persisted only as one-way digests and termination must be a known
        // vocabulary value. Substring redaction alone cannot exclude a
        // normalized fragment (for example a command token echoed without its
        // surrounding quotes), so raw provider-controlled text is never
        // persisted at all.
        sanitize_attempt_provider_fields(&mut self.attempt);
        AttemptCall {
            result,
            attempt: self.attempt,
            diagnostic: self.diagnostic,
            failure,
        }
    }
}

impl AttemptCall {
    /// A request-build failure: deterministic, so never retried.
    fn build_failed(
        attempt: JudgeAttemptTelemetry,
        diagnostic: Option<JudgeDiagnostic>,
        error: impl std::fmt::Display,
    ) -> Self {
        Self {
            result: Err(JudgeError::Transport {
                status: None,
                detail: error.to_string(),
            }),
            attempt,
            diagnostic,
            failure: None,
        }
    }
}

fn initial_attempt(
    client: &JudgeClient,
    request: &JudgeRequest,
    history: &[ConversationItem],
    params: &AttemptParams,
    effective_deadline_ms: u64,
) -> JudgeAttemptTelemetry {
    let (system_prompt, user_prompt) = prompt_text_refs(history);
    JudgeAttemptTelemetry {
        attempt: params.attempt,
        retry_ordinal: params.attempt.saturating_sub(1),
        retry_reason: params.retry_reason,
        retry_delay_ms: params.retry_delay_ms,
        effective_deadline_ms,
        request_build_ms: 0,
        request_ms: 0,
        response_parse_ms: 0,
        verdict_parse_ms: 0,
        total_ms: 0,
        history_items: history.len(),
        system_prompt_bytes: system_prompt.len(),
        user_prompt_bytes: user_prompt.len(),
        model: client.config.model_config.model.clone(),
        api_type: client.config.model_config.api_type,
        reasoning_effort: client.config.model_config.reasoning_effort,
        temperature: client.config.model_config.temperature,
        top_p: client.config.model_config.top_p,
        max_output_tokens: client.config.model_config.max_output_tokens,
        // Chat Completions requests never carry `reasoning.max_tokens`, so
        // reporting the configured value would describe a control the request
        // did not send; only the Responses backend sends it.
        reasoning_max_tokens: match client.config.model_config.api_type {
            ApiType::Responses => client.config.model_config.reasoning_max_tokens,
            ApiType::ChatCompletions => None,
        },
        configured_timeout_ms: duration_ms(client.timeout),
        tool_count: 0,
        tool_choice: None,
        status_code: None,
        call_id: request.call_id.clone(),
        provider_request_id: None,
        terminal_class: JudgeAttemptTerminalClass::Transport,
        usage: None,
        termination: None,
    }
}

fn initial_diagnostic(
    client: &JudgeClient,
    history: &[ConversationItem],
    request_json: &[u8],
) -> JudgeDiagnostic {
    let (system_prompt, user_prompt) = prompt_text_refs(history);
    // The request body already serialized successfully in `build_request_json`,
    // so parsing it back is best-effort; fall back to null rather than losing
    // the whole diagnostic.
    let request_json = serde_json::from_slice(request_json).unwrap_or(serde_json::Value::Null);
    JudgeDiagnostic {
        system_prompt: redact_secret(system_prompt, &client.config.api_key),
        user_prompt: redact_secret(user_prompt, &client.config.api_key),
        request_json: redact_json(request_json, &client.config.api_key),
        assistant_content: None,
        usage: None,
        termination: None,
    }
}

fn update_diagnostic(
    diagnostic: &mut Option<JudgeDiagnostic>,
    client: &JudgeClient,
    turn: &crate::clients::agent::TurnResult,
    content: Option<&str>,
) {
    let Some(raw) = diagnostic else {
        return;
    };
    raw.assistant_content = content.map(|content| redact_secret(content, &client.config.api_key));
    raw.usage = turn.usage;
    raw.termination = turn
        .termination
        .clone()
        .map(|termination| redact_termination(termination, &client.config.api_key));
}

fn prompt_text_refs(history: &[ConversationItem]) -> (&str, &str) {
    let mut system = "";
    let mut user = "";
    for item in history {
        if let ConversationItem::Message { role, content, .. } = item {
            match role {
                Role::System => system = content,
                Role::User => user = content,
                _ => {},
            }
        }
    }
    (system, user)
}

fn provider_request_id(headers: &reqwest::header::HeaderMap) -> Option<String> {
    ["x-request-id", "request-id", "openai-request-id"]
        .into_iter()
        .find_map(|name| headers.get(name))
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn redact_secret(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        text.to_string()
    } else {
        text.replace(secret, "<redacted>")
    }
}

fn redact_json(mut value: serde_json::Value, secret: &str) -> serde_json::Value {
    match &mut value {
        serde_json::Value::String(text) => *text = redact_secret(text, secret),
        serde_json::Value::Array(items) => {
            for item in items {
                *item = redact_json(std::mem::take(item), secret);
            }
        },
        serde_json::Value::Object(fields) => {
            for item in fields.values_mut() {
                *item = redact_json(std::mem::take(item), secret);
            }
        },
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {},
    }
    value
}

fn redact_termination(mut termination: ProviderTermination, secret: &str) -> ProviderTermination {
    termination.provider_status = termination
        .provider_status
        .as_deref()
        .map(|value| redact_secret(value, secret));
    termination.provider_reason = termination
        .provider_reason
        .as_deref()
        .map(|value| redact_secret(value, secret));
    termination
}

/// Strictly bound the provider-controlled attempt fields against a boundary
/// that does not depend on substring matching: provider identifiers are
/// persisted only as one-way digests, and termination status/reason must be a
/// known provider vocabulary value. Anything else is omitted, so a provider
/// echoing command, reason, cwd, digest, or rubric fragments cannot get them
/// into telemetry.
fn sanitize_attempt_provider_fields(attempt: &mut JudgeAttemptTelemetry) {
    digest_provider_identifier(&mut attempt.provider_request_id);
    digest_provider_identifier(&mut attempt.call_id);
    if let Some(termination) = &mut attempt.termination {
        if !termination
            .provider_status
            .as_deref()
            .is_some_and(is_safe_termination_value)
        {
            termination.provider_status = None;
        }
        if !termination
            .provider_reason
            .as_deref()
            .is_some_and(is_safe_termination_value)
        {
            termination.provider_reason = None;
        }
    }
}

/// The known provider termination status/reason vocabulary. Values outside it
/// are omitted rather than persisted: arbitrary provider text cannot be proven
/// free of prompt fragments.
fn is_safe_termination_value(value: &str) -> bool {
    matches!(
        value,
        "stop"
            | "tool_calls"
            | "function_call"
            | "length"
            | "max_tokens"
            | "max_output_tokens"
            | "content_filter"
            | "refusal"
            | "refused"
            | "failed"
            | "cancelled"
            | "incomplete"
            | "in_progress"
            | "queued"
            | "completed"
    )
}

/// Persist a provider-controlled identifier only as a one-way digest, so the
/// sidecar never carries raw provider text. Consumers correlate an attempt
/// with the provider or session by hashing the known raw value with the same
/// function; an empty identifier is omitted.
fn digest_provider_identifier(id: &mut Option<String>) {
    if let Some(value) = id {
        if value.is_empty() {
            *id = None;
        } else {
            *value = digest_identifier(value);
        }
    }
}

fn digest_identifier(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}

fn elapsed_ms(started: Instant) -> u64 {
    duration_ms(started.elapsed())
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}
