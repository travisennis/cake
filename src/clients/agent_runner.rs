use std::time::{Duration, Instant};

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
    AgentRunnerTelemetryEvent, ApiAttemptTelemetry, CompensationEventTelemetry, CompensationKind,
    RequestOverridesSnapshot, RetryScheduledTelemetry,
};
use crate::types::{ApiAttemptTerminalClass, ConversationItem, Usage};

pub(super) fn build_http_client(disable_connection_reuse: bool) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_mins(5));

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

impl AgentRunner {
    pub(super) fn new(backend: Backend) -> Self {
        Self {
            backend,
            client: build_http_client(false),
            retry_policy: RetryPolicy::default(),
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "a turn naturally threads config, identity, history, tools, and constraint"
    )]
    pub(super) async fn complete_turn<'a>(
        &self,
        config: &ResolvedModelConfig,
        session_id: uuid::Uuid,
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
            let total_start = Instant::now();
            let request_start = Instant::now();
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
            let request_ms = elapsed_ms(request_start);

            let outcome = match request_result {
                Ok(response) => {
                    let status_code = response.status().as_u16();
                    if response.status().is_success() {
                        self.handle_success_response(
                            response,
                            session_id,
                            turn_index,
                            attempt,
                            request_ms,
                            total_start,
                            history.len(),
                            status_code,
                            &request_overrides,
                            &mut report_telemetry,
                            &mut settle_usage,
                        )
                        .await
                    } else {
                        self.handle_http_failure(
                            response,
                            config,
                            session_id,
                            turn_index,
                            attempt,
                            request_ms,
                            total_start,
                            history.len(),
                            status_code,
                            &mut request_overrides,
                            &mut report_telemetry,
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
                        attempt,
                        request_ms,
                        total_start,
                        history.len(),
                        &mut client,
                        &mut disable_connection_reuse,
                        &mut request_overrides,
                        &mut report_telemetry,
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
    #[expect(
        clippy::too_many_arguments,
        reason = "one turn phase threads identity, timing, attempt, and telemetry"
    )]
    async fn handle_success_response(
        &self,
        response: reqwest::Response,
        session_id: uuid::Uuid,
        turn_index: u32,
        attempt: u32,
        request_ms: u64,
        total_start: Instant,
        history_items: usize,
        status_code: u16,
        request_overrides: &RequestOverrides,
        report_telemetry: &mut impl FnMut(AgentRunnerTelemetryEvent),
        settle_usage: &mut impl FnMut(TurnUsageSettlement),
    ) -> AttemptResult {
        let parse_start = Instant::now();
        let parse_result = self.backend.parse_response(response).await;
        let parse_ms = elapsed_ms(parse_start);
        let total_ms = elapsed_ms(total_start);

        let failed = parse_result
            .as_ref()
            .err()
            .and_then(|error| error.downcast_ref::<ResponsesStreamFailed>());
        let terminal_class = match (parse_result.is_ok(), failed.is_some()) {
            (true, _) => ApiAttemptTerminalClass::Completed,
            (false, true) => ApiAttemptTerminalClass::ResponseFailed,
            (false, false) => ApiAttemptTerminalClass::BodyParse,
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
        let error = parse_result
            .as_ref()
            .err()
            .map(|error| format!("{error:#}"));

        report_telemetry(AgentRunnerTelemetryEvent::ApiAttempt(ApiAttemptTelemetry {
            turn_index,
            attempt,
            request_ms,
            parse_ms,
            total_ms,
            history_items,
            status_code: Some(status_code),
            error,
            usage,
            termination,
            terminal_class: Some(terminal_class),
            provider_request_id,
            responses_failed,
            request_overrides: RequestOverridesSnapshot::from(request_overrides),
        }));

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
                    attempt,
                    request_overrides,
                    report_telemetry,
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
    async fn recover_response_failed(
        &self,
        parse_error: anyhow::Error,
        session_id: uuid::Uuid,
        turn_index: u32,
        attempt: u32,
        request_overrides: &RequestOverrides,
        report_telemetry: &mut impl FnMut(AgentRunnerTelemetryEvent),
    ) -> Result<(), anyhow::Error> {
        let Some(failed) = parse_error.downcast_ref::<ResponsesStreamFailed>() else {
            return Err(parse_error);
        };
        match retry::classify_response_failed(
            &self.retry_policy,
            failed.metadata().error_code.as_deref(),
            failed.metadata().error_type.as_deref(),
            attempt,
            session_id,
        ) {
            retry::RetryDecision::Retry { status } => {
                report_telemetry(AgentRunnerTelemetryEvent::RetryScheduled(
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
    async fn handle_http_failure(
        &self,
        response: reqwest::Response,
        config: &ResolvedModelConfig,
        session_id: uuid::Uuid,
        turn_index: u32,
        attempt: u32,
        request_ms: u64,
        total_start: Instant,
        history_items: usize,
        status_code: u16,
        request_overrides: &mut RequestOverrides,
        report_telemetry: &mut impl FnMut(AgentRunnerTelemetryEvent),
        settle_usage: &mut impl FnMut(TurnUsageSettlement),
    ) -> AttemptResult {
        let headers = response.headers().clone();
        let body = response.text().await.unwrap_or_default();
        let failure = HttpFailure {
            status: status_code,
            headers,
            body,
        };
        let usage = self.backend.reported_usage(failure.body.as_bytes());
        report_telemetry(AgentRunnerTelemetryEvent::ApiAttempt(ApiAttemptTelemetry {
            turn_index,
            attempt,
            request_ms,
            parse_ms: 0,
            total_ms: elapsed_ms(total_start),
            history_items,
            status_code: Some(status_code),
            error: Some(format!("{} {}", failure.status, failure.body)),
            usage,
            termination: None,
            terminal_class: Some(ApiAttemptTerminalClass::Http),
            provider_request_id: None,
            responses_failed: None,
            request_overrides: RequestOverridesSnapshot::from(&*request_overrides),
        }));
        if let Some(usage) = usage {
            settle_usage(TurnUsageSettlement {
                turn_index,
                attempt,
                terminal_class: ApiAttemptTerminalClass::Http,
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
                report_telemetry(AgentRunnerTelemetryEvent::RetryScheduled(
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
                report_telemetry(AgentRunnerTelemetryEvent::RetryScheduled(
                    RetryScheduledTelemetry::from_status(&status, turn_index, true, &overrides),
                ));
                report_telemetry(AgentRunnerTelemetryEvent::Compensation(
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
    /// classify a retryable transport failure (swapping in a no-reuse client)
    /// or return the error as terminal.
    #[expect(
        clippy::too_many_arguments,
        reason = "the transport-failure phase threads identity, timing, and telemetry"
    )]
    async fn handle_transport_error(
        &self,
        error: anyhow::Error,
        session_id: uuid::Uuid,
        turn_index: u32,
        attempt: u32,
        request_ms: u64,
        total_start: Instant,
        history_items: usize,
        client: &mut reqwest::Client,
        disable_connection_reuse: &mut bool,
        request_overrides: &mut RequestOverrides,
        report_telemetry: &mut impl FnMut(AgentRunnerTelemetryEvent),
    ) -> AttemptResult {
        let error_detail = format!("{error:#}");
        debug!(
            target: "cake",
            error = %error_detail,
            "API request failed before receiving an HTTP response"
        );

        report_telemetry(AgentRunnerTelemetryEvent::ApiAttempt(ApiAttemptTelemetry {
            turn_index,
            attempt,
            request_ms,
            parse_ms: 0,
            total_ms: elapsed_ms(total_start),
            history_items,
            status_code: None,
            error: Some(error_detail),
            usage: None,
            termination: None,
            terminal_class: Some(ApiAttemptTerminalClass::Transport),
            provider_request_id: None,
            responses_failed: None,
            request_overrides: RequestOverridesSnapshot::from(&*request_overrides),
        }));

        match retry::classify_transport_error(&self.retry_policy, &error, attempt, session_id) {
            retry::RetryDecision::Retry { status } => {
                if retry::should_disable_connection_reuse(&error) && !*disable_connection_reuse {
                    // Only this turn's remaining attempts use the no-reuse
                    // client; `self.client` is untouched.
                    *client = build_http_client(true);
                    *disable_connection_reuse = true;
                }
                report_telemetry(AgentRunnerTelemetryEvent::RetryScheduled(
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
                report_telemetry(AgentRunnerTelemetryEvent::RetryScheduled(
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
