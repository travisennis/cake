use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tokio::time::sleep;
use tracing::debug;

use crate::clients::agent::{TurnResult, TurnUsageSettlement};
use crate::clients::backend::{
    Backend, FinalOutputConstraint, ResponseDecodeError, ResponseParseError,
};
use crate::clients::responses::ResponsesStreamFailed;
use crate::clients::retry::{self, HttpFailure, RequestOverrides, RetryPolicy, RetryStatus};
use crate::clients::tools::Tool;
use crate::config::model::ResolvedModelConfig;
use crate::session_telemetry::{
    AgentRunnerTelemetryEvent, ApiAttemptInFlightTelemetry, ApiAttemptPhase, ApiAttemptTelemetry,
    CompensationEventTelemetry, CompensationKind, RequestOverridesSnapshot,
    RetryScheduledTelemetry,
};
use crate::types::{ApiAttemptTerminalClass, ConversationItem, Usage};

pub(super) fn build_http_client(disable_connection_reuse: bool) -> reqwest::Client {
    build_http_client_with_timeout(disable_connection_reuse, Duration::from_mins(5))
}

fn build_http_client_with_timeout(
    disable_connection_reuse: bool,
    timeout: Duration,
) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(timeout);

    if disable_connection_reuse {
        builder = builder.pool_max_idle_per_host(0);
    }

    builder.build().unwrap_or_else(|error| {
        panic!("HTTP client builder should be valid with fixed timeout and pool settings: {error}")
    })
}

pub(super) struct AgentRunner {
    backend: Backend,
    client: reqwest::Client,
    retry_policy: RetryPolicy,
}

/// Outcome of one provider attempt, driving the bounded retry loop.
enum AttemptResult {
    /// The turn completed; return the `TurnResult`.
    Completed(TurnResult),
    /// The attempt failed but a retry is scheduled; continue the loop.
    RetryNeeded,
    /// The attempt failed terminally; return the error.
    Terminal(anyhow::Error),
}

/// Emits a bounded start/phase record immediately and closes the attempt when
/// the provider operation finishes or is cancelled.
struct InFlightAttempt<'a, F: FnMut(AgentRunnerTelemetryEvent)> {
    reporter: &'a mut F,
    task_id: String,
    turn_index: u32,
    attempt: u32,
    provider: Option<crate::config::model::ModelProvider>,
    model: String,
    started_at: DateTime<Utc>,
    total_start: Instant,
    request_start: Instant,
    request_ms: Option<u64>,
    parse_start: Option<Instant>,
    phase: ApiAttemptPhase,
    status_code: Option<u16>,
    history_items: usize,
    request_overrides: RequestOverridesSnapshot,
    closed: bool,
}

impl<'a, F: FnMut(AgentRunnerTelemetryEvent)> InFlightAttempt<'a, F> {
    fn new(
        reporter: &'a mut F,
        task_id: uuid::Uuid,
        config: &ResolvedModelConfig,
        turn_index: u32,
        attempt: u32,
        history_items: usize,
        request_overrides: &RequestOverrides,
    ) -> Self {
        let started_at = Utc::now();
        let total_start = Instant::now();
        let request_start = Instant::now();
        let mut attempt = Self {
            reporter,
            task_id: task_id.to_string(),
            turn_index,
            attempt,
            provider: crate::clients::provider_strategy::ProviderStrategy::from_config(config)
                .provider(),
            model: config.model_config.model.clone(),
            started_at,
            total_start,
            request_start,
            request_ms: None,
            parse_start: None,
            phase: ApiAttemptPhase::AwaitingHeaders,
            status_code: None,
            history_items,
            request_overrides: RequestOverridesSnapshot::from(request_overrides),
            closed: false,
        };
        attempt.report_in_flight();
        // Keep provider timing separate from the synchronous telemetry write.
        let operation_start = Instant::now();
        attempt.total_start = operation_start;
        attempt.request_start = operation_start;
        attempt
    }

    fn report_in_flight(&mut self) {
        (self.reporter)(AgentRunnerTelemetryEvent::ApiAttemptInFlight(
            ApiAttemptInFlightTelemetry {
                task_id: self.task_id.clone(),
                turn_index: self.turn_index,
                attempt: self.attempt,
                provider: self.provider,
                model: self.model.clone(),
                started_at: self.started_at,
                phase: self.phase,
                status_code: self.status_code,
            },
        ));
    }

    fn headers_received(&mut self, status_code: u16, request_ms: u64) {
        self.request_ms = Some(request_ms);
        self.status_code = Some(status_code);
        self.phase = ApiAttemptPhase::ReadingBody;
        self.report_in_flight();
        // The body timer begins after the phase update has been persisted, so
        // telemetry I/O is not attributed to provider response parsing.
        self.parse_start = Some(Instant::now());
    }

    fn request_ms(&self) -> u64 {
        self.request_ms
            .unwrap_or_else(|| elapsed_ms(self.request_start))
    }

    fn parse_ms(&self) -> u64 {
        self.parse_start.map_or(0, elapsed_ms)
    }

    fn total_ms(&self) -> u64 {
        elapsed_ms(self.total_start)
    }

    fn finish(&mut self, attempt: ApiAttemptTelemetry) {
        self.closed = true;
        (self.reporter)(AgentRunnerTelemetryEvent::ApiAttempt(attempt));
    }

    fn report(&mut self, event: AgentRunnerTelemetryEvent) {
        (self.reporter)(event);
    }
}

impl<F: FnMut(AgentRunnerTelemetryEvent)> Drop for InFlightAttempt<'_, F> {
    fn drop(&mut self) {
        if self.closed {
            return;
        }

        let phase = self.phase;
        let request_ms = self.request_ms();
        let parse_ms = self.parse_ms();
        let total_ms = self.total_ms();
        let history_items = self.history_items;
        let status_code = self.status_code;
        let request_overrides = self.request_overrides.clone();
        self.closed = true;
        (self.reporter)(AgentRunnerTelemetryEvent::ApiAttempt(ApiAttemptTelemetry {
            turn_index: self.turn_index,
            attempt: self.attempt,
            request_ms,
            parse_ms,
            total_ms,
            history_items,
            status_code,
            error: Some(format!(
                "provider attempt cancelled while {}",
                phase.as_str()
            )),
            usage: None,
            termination: None,
            terminal_class: Some(ApiAttemptTerminalClass::Cancelled),
            provider_request_id: None,
            responses_failed: None,
            phase: Some(phase),
            request_overrides,
        }));
    }
}

impl AgentRunner {
    pub(super) fn new(backend: Backend) -> Self {
        Self {
            backend,
            client: build_http_client(false),
            retry_policy: RetryPolicy::default(),
        }
    }

    #[cfg(test)]
    pub(super) fn with_request_timeout(&mut self, timeout: Duration) {
        self.client = build_http_client_with_timeout(false, timeout);
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "a turn naturally threads config, identity, history, tools, and constraint"
    )]
    pub(super) async fn complete_turn<'a>(
        &self,
        config: &ResolvedModelConfig,
        session_id: uuid::Uuid,
        task_id: uuid::Uuid,
        turn_index: u32,
        history: &'a [ConversationItem],
        tools: &'a [Tool],
        constraint: Option<FinalOutputConstraint<'a>>,
        next_attempt: &mut u32,
        mut report_telemetry: impl FnMut(AgentRunnerTelemetryEvent),
        mut settle_usage: impl FnMut(TurnUsageSettlement),
    ) -> anyhow::Result<TurnResult> {
        let mut attempt = *next_attempt;
        let mut request_overrides = RequestOverrides {
            max_output_tokens: config.model_config.max_output_tokens,
            reasoning_max_tokens: config.model_config.reasoning_max_tokens,
            context_overflow_retry_used: false,
        };
        let mut disable_connection_reuse = false;
        // Start from the persistent pooled client. Stale-connection recovery
        // swaps in a temporary no-reuse client local to this turn, so
        // `self.client` keeps normal pooling whether this turn retries
        // successfully or fails, and later turns observe the pooled client.
        let mut client = self.client.clone();

        loop {
            let mut in_flight = InFlightAttempt::new(
                &mut report_telemetry,
                task_id,
                config,
                turn_index,
                attempt,
                history.len(),
                &request_overrides,
            );
            let request_result = self
                .backend
                .send_request(
                    &client,
                    config,
                    history,
                    tools,
                    &request_overrides,
                    constraint,
                )
                .await;
            let request_ms = in_flight.request_ms();

            let outcome = match request_result {
                Ok(response) => {
                    let status_code = response.status().as_u16();
                    in_flight.headers_received(status_code, request_ms);
                    if response.status().is_success() {
                        self.handle_success_response(
                            response,
                            session_id,
                            turn_index,
                            &mut in_flight,
                            &request_overrides,
                            &mut settle_usage,
                        )
                        .await
                    } else {
                        self.handle_http_failure(
                            response,
                            config,
                            session_id,
                            turn_index,
                            &mut in_flight,
                            &mut request_overrides,
                            &mut settle_usage,
                        )
                        .await
                    }
                },
                Err(error) => {
                    self.handle_transport_error(
                        error,
                        session_id,
                        turn_index,
                        &mut in_flight,
                        &mut client,
                        &mut disable_connection_reuse,
                        &mut request_overrides,
                    )
                    .await
                },
            };

            match outcome {
                AttemptResult::Completed(turn) => {
                    *next_attempt = attempt.saturating_add(1);
                    return Ok(turn);
                },
                AttemptResult::RetryNeeded => {
                    attempt = attempt.saturating_add(1);
                },
                AttemptResult::Terminal(error) => {
                    *next_attempt = attempt.saturating_add(1);
                    return Err(error);
                },
            }
        }
    }

    /// Parse an accepted 2xx provider body, record per-attempt telemetry, and
    /// either return the completed turn, schedule a retry for a transient
    /// `response.failed`, or surface the terminal parse error.
    async fn handle_success_response<F>(
        &self,
        response: reqwest::Response,
        session_id: uuid::Uuid,
        turn_index: u32,
        in_flight: &mut InFlightAttempt<'_, F>,
        request_overrides: &RequestOverrides,
        settle_usage: &mut impl FnMut(TurnUsageSettlement),
    ) -> AttemptResult
    where
        F: FnMut(AgentRunnerTelemetryEvent),
    {
        let parse_result = self.backend.parse_response(response).await;
        let parse_ms = in_flight.parse_ms();
        let total_ms = in_flight.total_ms();

        let failed = parse_result
            .as_ref()
            .err()
            .and_then(|error| error.downcast_ref::<ResponsesStreamFailed>());
        let terminal_class = if parse_result.as_ref().err().is_some_and(error_is_timeout) {
            ApiAttemptTerminalClass::Timeout
        } else {
            match (parse_result.is_ok(), failed.is_some()) {
                (true, _) => ApiAttemptTerminalClass::Completed,
                (false, true) => ApiAttemptTerminalClass::ResponseFailed,
                (false, false) => ApiAttemptTerminalClass::BodyParse,
            }
        };
        let provider_request_id = parse_result
            .as_ref()
            .ok()
            .and_then(|turn| turn.provider_request_id.clone())
            .or_else(|| failed.and_then(|failed| failed.metadata().provider_request_id.clone()));
        let responses_failed = failed.map(|failed| Box::new(failed.metadata().clone()));
        let usage = reported_usage_from_result(&parse_result);
        let termination = parse_result
            .as_ref()
            .ok()
            .and_then(|turn| turn.termination.clone());
        let error = parse_result.as_ref().err().map(ToString::to_string);
        let attempt = in_flight.attempt;
        let request_ms = in_flight.request_ms();
        let status_code = in_flight.status_code;
        let phase = Some(in_flight.phase);

        in_flight.finish(ApiAttemptTelemetry {
            turn_index,
            attempt: in_flight.attempt,
            request_ms,
            parse_ms,
            total_ms,
            history_items: in_flight.history_items,
            status_code,
            error,
            usage,
            termination,
            terminal_class: Some(terminal_class),
            provider_request_id,
            responses_failed,
            phase,
            request_overrides: RequestOverridesSnapshot::from(request_overrides),
        });

        if let Some(usage) = usage {
            settle_usage(TurnUsageSettlement {
                turn_index,
                attempt,
                terminal_class,
                usage,
            });
        }

        match parse_result {
            Ok(turn) => AttemptResult::Completed(turn),
            Err(parse_error) => match self
                .recover_response_failed(
                    parse_error,
                    session_id,
                    turn_index,
                    in_flight,
                    request_overrides,
                )
                .await
            {
                Ok(()) => AttemptResult::RetryNeeded,
                Err(parse_error) => AttemptResult::Terminal(parse_error),
            },
        }
    }

    /// Handle a terminal `response.failed` parse error after an accepted HTTP
    /// 2xx. Retries a transient `server_error` under the bounded retry/deadline
    /// policy, or returns the error when the failure is semantic (auth,
    /// invalid-request, quota, context, policy) or retries are exhausted.
    async fn recover_response_failed<F>(
        &self,
        parse_error: anyhow::Error,
        session_id: uuid::Uuid,
        turn_index: u32,
        in_flight: &mut InFlightAttempt<'_, F>,
        request_overrides: &RequestOverrides,
    ) -> Result<(), anyhow::Error>
    where
        F: FnMut(AgentRunnerTelemetryEvent),
    {
        let Some(failed) = parse_error.downcast_ref::<ResponsesStreamFailed>() else {
            return Err(parse_error);
        };
        match retry::classify_response_failed(
            &self.retry_policy,
            failed.metadata().error_code.as_deref(),
            failed.metadata().error_type.as_deref(),
            in_flight.attempt,
            session_id,
        ) {
            retry::RetryDecision::Retry { status } => {
                in_flight.report(AgentRunnerTelemetryEvent::RetryScheduled(
                    RetryScheduledTelemetry::from_status(
                        &status,
                        turn_index,
                        false,
                        request_overrides,
                    ),
                ));
                wait_for_retry(&status).await;
                Ok(())
            },
            retry::RetryDecision::RetryWithOverrides { .. } | retry::RetryDecision::DoNotRetry => {
                Err(parse_error)
            },
        }
    }

    /// Handle a non-2xx HTTP response: record attempt telemetry, then classify
    /// the failure for retry (rate-limit, overload, context overflow) or return
    /// it as an `ApiError`.
    #[expect(
        clippy::too_many_arguments,
        reason = "the HTTP-failure phase threads identity, timing, attempt, and telemetry"
    )]
    async fn handle_http_failure<F>(
        &self,
        response: reqwest::Response,
        config: &ResolvedModelConfig,
        session_id: uuid::Uuid,
        turn_index: u32,
        in_flight: &mut InFlightAttempt<'_, F>,
        request_overrides: &mut RequestOverrides,
        settle_usage: &mut impl FnMut(TurnUsageSettlement),
    ) -> AttemptResult
    where
        F: FnMut(AgentRunnerTelemetryEvent),
    {
        let headers = response.headers().clone();
        let body_result = response.text().await;
        let (body, body_error) = match body_result {
            Ok(body) => (body, None),
            Err(error) => (String::new(), Some(error)),
        };
        let failure = HttpFailure {
            status: in_flight.status_code.unwrap_or_default(),
            headers,
            body,
        };
        let usage = self.backend.reported_usage(failure.body.as_bytes());
        let terminal_class = body_error
            .as_ref()
            .map_or(ApiAttemptTerminalClass::Http, |error| {
                if error.is_timeout() {
                    ApiAttemptTerminalClass::Timeout
                } else {
                    ApiAttemptTerminalClass::Http
                }
            });
        let error = format!("{} {}", failure.status, failure.body);
        let attempt = in_flight.attempt;
        in_flight.finish(ApiAttemptTelemetry {
            turn_index,
            attempt,
            request_ms: in_flight.request_ms(),
            parse_ms: 0,
            total_ms: in_flight.total_ms(),
            history_items: in_flight.history_items,
            status_code: in_flight.status_code,
            error: Some(error),
            usage,
            termination: None,
            terminal_class: Some(terminal_class),
            provider_request_id: None,
            responses_failed: None,
            phase: Some(in_flight.phase),
            request_overrides: RequestOverridesSnapshot::from(&*request_overrides),
        });
        if let Some(usage) = usage {
            settle_usage(TurnUsageSettlement {
                turn_index,
                attempt,
                terminal_class,
                usage,
            });
        }

        match retry::classify_http_failure(
            &self.retry_policy,
            &failure,
            attempt,
            session_id,
            request_overrides,
        ) {
            retry::RetryDecision::Retry { status } => {
                in_flight.report(AgentRunnerTelemetryEvent::RetryScheduled(
                    RetryScheduledTelemetry::from_status(
                        &status,
                        turn_index,
                        false,
                        &*request_overrides,
                    ),
                ));
                wait_for_retry(&status).await;
                AttemptResult::RetryNeeded
            },
            retry::RetryDecision::RetryWithOverrides { status, overrides } => {
                in_flight.report(AgentRunnerTelemetryEvent::RetryScheduled(
                    RetryScheduledTelemetry::from_status(&status, turn_index, true, &overrides),
                ));
                in_flight.report(AgentRunnerTelemetryEvent::Compensation(
                    CompensationEventTelemetry::new(CompensationKind::ContextOverflowRetry, None),
                ));
                *request_overrides = overrides;
                wait_for_retry(&status).await;
                AttemptResult::RetryNeeded
            },
            retry::RetryDecision::DoNotRetry => AttemptResult::Terminal(
                api_error_from_failure(&config.model_config.model, &failure).into(),
            ),
        }
    }

    /// Handle a request-phase transport error: record attempt telemetry, then
    /// classify a stale-connection retry (swapping in a no-reuse client) or
    /// return the error as terminal.
    #[expect(
        clippy::too_many_arguments,
        reason = "the transport-failure phase threads identity, timing, attempt, and telemetry"
    )]
    async fn handle_transport_error<F>(
        &self,
        error: anyhow::Error,
        session_id: uuid::Uuid,
        turn_index: u32,
        in_flight: &mut InFlightAttempt<'_, F>,
        client: &mut reqwest::Client,
        disable_connection_reuse: &mut bool,
        request_overrides: &mut RequestOverrides,
    ) -> AttemptResult
    where
        F: FnMut(AgentRunnerTelemetryEvent),
    {
        let terminal_class = if error_is_timeout(&error) {
            ApiAttemptTerminalClass::Timeout
        } else {
            ApiAttemptTerminalClass::Transport
        };
        let attempt = in_flight.attempt;
        in_flight.finish(ApiAttemptTelemetry {
            turn_index,
            attempt,
            request_ms: in_flight.request_ms(),
            parse_ms: 0,
            total_ms: in_flight.total_ms(),
            history_items: in_flight.history_items,
            status_code: None,
            error: Some(error.to_string()),
            usage: None,
            termination: None,
            terminal_class: Some(terminal_class),
            provider_request_id: None,
            responses_failed: None,
            phase: Some(in_flight.phase),
            request_overrides: RequestOverridesSnapshot::from(&*request_overrides),
        });

        match retry::classify_transport_error(&self.retry_policy, &error, attempt, session_id) {
            retry::RetryDecision::Retry { status } => {
                if retry::should_disable_connection_reuse(&error) && !*disable_connection_reuse {
                    // Only this turn's remaining attempts use the no-reuse
                    // client; `self.client` is untouched.
                    *client = build_http_client(true);
                    *disable_connection_reuse = true;
                }
                in_flight.report(AgentRunnerTelemetryEvent::RetryScheduled(
                    RetryScheduledTelemetry::from_status(
                        &status,
                        turn_index,
                        false,
                        &*request_overrides,
                    ),
                ));
                wait_for_retry(&status).await;
                AttemptResult::RetryNeeded
            },
            retry::RetryDecision::RetryWithOverrides { status, overrides } => {
                in_flight.report(AgentRunnerTelemetryEvent::RetryScheduled(
                    RetryScheduledTelemetry::from_status(&status, turn_index, true, &overrides),
                ));
                *request_overrides = overrides;
                wait_for_retry(&status).await;
                AttemptResult::RetryNeeded
            },
            retry::RetryDecision::DoNotRetry => AttemptResult::Terminal(error),
        }
    }
}

fn reported_usage_from_result(result: &Result<TurnResult, anyhow::Error>) -> Option<Usage> {
    result
        .as_ref()
        .ok()
        .and_then(|turn| turn.usage)
        .or_else(|| {
            let error = result.as_ref().err()?;
            error
                .downcast_ref::<ResponsesStreamFailed>()
                .and_then(ResponsesStreamFailed::usage)
                .or_else(|| {
                    error
                        .downcast_ref::<ResponseParseError>()
                        .and_then(ResponseParseError::usage)
                })
                .or_else(|| {
                    error
                        .downcast_ref::<ResponseDecodeError>()
                        .and_then(ResponseDecodeError::usage)
                })
        })
}

fn error_is_timeout(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<reqwest::Error>()
            .is_some_and(reqwest::Error::is_timeout)
    })
}

fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

async fn wait_for_retry(status: &RetryStatus) {
    debug!(
        target: "cake",
        reason = ?status.reason,
        detail = %status.detail,
        delay_ms = status.delay.as_millis(),
        attempt = status.attempt,
        max_attempts = status.max_retries,
        "Retrying API request"
    );

    if !status.delay.is_zero() {
        sleep(status.delay).await;
    }
}

fn api_error_from_failure(model: &str, failure: &HttpFailure) -> crate::exit_code::ApiError {
    debug!(target: "cake", "{}", failure.body);

    crate::exit_code::ApiError {
        status: failure.status,
        body: format_api_error_body(model, &failure.body),
    }
}

fn format_api_error_body(model: &str, error_text: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(error_text)
        && let Ok(formatted) = serde_json::to_string_pretty(&value)
    {
        return format!("{model}\n\n{formatted}");
    }
    format!("{model}\n\n{error_text}")
}
