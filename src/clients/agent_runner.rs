use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tokio::time::sleep;
use tracing::debug;

use crate::clients::agent::{AttemptIdentity, TurnResult, TurnUsageSettlement};
use crate::clients::agent_observer::HealthCallback;
use crate::clients::backend::Backend;
use crate::clients::provider_edge::{ProviderAttemptError, TurnRequest, error_is_timeout};
use crate::clients::response_stall::{
    BodyReadFailure, BodyReadOutcome, FailedBody, ResponseStall, STALL_WINDOW, StallPhase,
    StalledBody, read_body,
};
use crate::clients::retry::{self, HttpFailure, RequestOverrides, RetryPolicy, RetryStatus};
use crate::clients::termination::{self, IntendedLimits, TerminationOutcome, TurnEvidence};
use crate::config::model::ResolvedModelConfig;
use crate::session_telemetry::{
    AgentRunnerTelemetryEvent, ApiAttemptInFlightTelemetry, ApiAttemptPhase, ApiAttemptTelemetry,
    CompensationEventTelemetry, CompensationKind, RequestOverridesSnapshot,
    RetryScheduledTelemetry, RetryWaitTelemetry,
};
use crate::types::{ApiAttemptTerminalClass, ReportedUsage, UsagePresence};

pub(super) fn build_http_client(disable_connection_reuse: bool) -> reqwest::Client {
    let mut builder = reqwest::Client::builder().connect_timeout(Duration::from_secs(10));

    if disable_connection_reuse {
        builder = builder.pool_max_idle_per_host(0);
    }

    builder.build().unwrap_or_else(|error| {
        panic!("HTTP client builder should be valid with fixed pool settings: {error}")
    })
}

pub(super) struct AgentRunner {
    backend: Backend,
    client: reqwest::Client,
    retry_policy: RetryPolicy,
    stall_window: Duration,
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

/// A non-2xx body and, when the read did not complete, its typed cause.
struct FailureBody {
    body: String,
    failure: Option<BodyReadFailure>,
}

/// Emits a bounded start/phase record immediately and closes the attempt when
/// the provider operation finishes or is cancelled.
struct InFlightAttempt<'a, F: FnMut(AgentRunnerTelemetryEvent)> {
    reporter: &'a mut F,
    identity: AttemptIdentity,
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
        config: &ResolvedModelConfig,
        identity: &AttemptIdentity,
        history_items: usize,
        request_overrides: &RequestOverrides,
    ) -> Self {
        let started_at = Utc::now();
        let total_start = Instant::now();
        let request_start = Instant::now();
        let mut attempt = Self {
            reporter,
            identity: identity.clone(),
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
        let operation_start = Instant::now();
        attempt.total_start = operation_start;
        attempt.request_start = operation_start;
        attempt
    }

    fn report_in_flight(&mut self) {
        (self.reporter)(AgentRunnerTelemetryEvent::ApiAttemptInFlight(
            ApiAttemptInFlightTelemetry {
                task_id: self.identity.task_id.clone(),
                turn_index: self.identity.turn_index,
                attempt: self.identity.attempt,
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
        let telemetry_start = Instant::now();
        self.report_in_flight();
        if let Some(total_start) = self.total_start.checked_add(telemetry_start.elapsed()) {
            self.total_start = total_start;
        }
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

    fn purpose_fields(
        &self,
    ) -> (
        Option<crate::types::TurnUsagePurpose>,
        Option<String>,
        Option<u32>,
    ) {
        self.identity.purpose_fields()
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
        let (purpose, compaction_id, summary_step) = self.purpose_fields();
        self.closed = true;
        (self.reporter)(AgentRunnerTelemetryEvent::ApiAttempt(ApiAttemptTelemetry {
            turn_index: self.identity.turn_index,
            attempt: self.identity.attempt,
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
            usage_presence: UsagePresence::Unreported,
            provider: self.provider,
            model: self.model.clone(),
            response_model: None,
            termination: None,
            terminal_class: Some(ApiAttemptTerminalClass::Cancelled),
            provider_request_id: None,
            responses_failed: None,
            phase: Some(phase),
            request_overrides,
            purpose,
            compaction_id,
            summary_step,
        }));
    }
}

impl AgentRunner {
    pub(super) fn new(backend: Backend) -> Self {
        Self {
            backend,
            client: build_http_client(false),
            retry_policy: RetryPolicy::default(),
            stall_window: STALL_WINDOW,
        }
    }

    pub(super) const fn backend(&self) -> Backend {
        self.backend
    }

    /// Send one prebuilt request body and parse its response.
    ///
    /// This path is for request reuse, where the body must stay byte-for-byte
    /// as captured. It deliberately has no retry loop: a failed suggestion call
    /// falls back to the normal resume path at the caller.
    pub(super) async fn fixed_turn(
        &self,
        config: &ResolvedModelConfig,
        session_id: uuid::Uuid,
        request: Vec<u8>,
    ) -> anyhow::Result<TurnResult> {
        let response = match tokio::time::timeout(
            self.stall_window,
            self.backend
                .send_request_json(&self.client, config, session_id, request),
        )
        .await
        {
            Ok(result) => result?,
            Err(_elapsed) => {
                return Err(ResponseStall::new(
                    config.model_config.model.clone(),
                    StallPhase::Connect,
                    self.stall_window,
                    0,
                )
                .into());
            },
        };
        let status = response.status();
        if status.is_success() {
            return self
                .backend
                .parse_response(response, &config.model_config.model, self.stall_window)
                .await
                .map_err(ProviderAttemptError::into_error);
        }
        let headers = response.headers().clone();
        let FailureBody {
            body,
            failure: body_read_failure,
        } = self
            .read_failure_body(response, &config.model_config.model)
            .await;
        let failure = with_body_read_detail(
            HttpFailure {
                status: status.as_u16(),
                headers,
                body,
            },
            body_read_failure.as_ref(),
        );
        Err(api_error_from_failure(config, &failure, body_read_failure).into())
    }

    /// Send one request while bounding the wait for response headers.
    ///
    /// The body reader applies the same deadline per chunk, so this covers the
    /// gap between the request and the first byte of the response.
    async fn send_with_stall_deadline(
        &self,
        client: &reqwest::Client,
        session_id: uuid::Uuid,
        request: &TurnRequest<'_>,
    ) -> anyhow::Result<reqwest::Response> {
        match tokio::time::timeout(
            self.stall_window,
            self.backend.send_request(client, session_id, request),
        )
        .await
        {
            Ok(result) => result,
            Err(_elapsed) => Err(ResponseStall::new(
                request.config.model_config.model.clone(),
                StallPhase::Connect,
                self.stall_window,
                0,
            )
            .into()),
        }
    }

    pub(super) async fn complete_turn(
        &self,
        mut request: TurnRequest<'_>,
        identity: &mut AttemptIdentity,
        before_attempt: Option<&HealthCallback>,
        mut report_telemetry: impl FnMut(AgentRunnerTelemetryEvent),
        mut settle_usage: impl FnMut(TurnUsageSettlement),
    ) -> anyhow::Result<TurnResult> {
        let mut disable_connection_reuse = false;
        let mut client = self.client.clone();

        loop {
            if let Some(check) = before_attempt {
                check()?;
            }
            let mut in_flight = InFlightAttempt::new(
                &mut report_telemetry,
                request.config,
                identity,
                request.history.len(),
                &request.overrides,
            );
            let request_result = self
                .send_with_stall_deadline(&client, identity.session_id, &request)
                .await;
            let request_ms = in_flight.request_ms();

            let outcome = match request_result {
                Ok(response) => {
                    let status_code = response.status().as_u16();
                    in_flight.headers_received(status_code, request_ms);
                    if response.status().is_success() {
                        self.handle_success_response(
                            response,
                            &mut request,
                            &mut in_flight,
                            &mut client,
                            &mut disable_connection_reuse,
                            &mut settle_usage,
                        )
                        .await
                    } else {
                        self.handle_http_failure(
                            response,
                            &mut request,
                            &mut in_flight,
                            &mut settle_usage,
                        )
                        .await
                    }
                },
                Err(error) => {
                    self.handle_transport_error(
                        error,
                        &mut in_flight,
                        &mut client,
                        &mut disable_connection_reuse,
                        &mut request.overrides,
                    )
                    .await
                },
            };

            match outcome {
                AttemptResult::Completed(turn) => {
                    identity.advance();
                    return Ok(turn);
                },
                AttemptResult::RetryNeeded => identity.advance(),
                AttemptResult::Terminal(error) => {
                    identity.advance();
                    return Err(error);
                },
            }
        }
    }

    /// Parse an accepted 2xx provider body, record per-attempt telemetry, and
    /// either return the completed turn, schedule a retry for a transient
    /// transport failure or `response.failed`, or surface the terminal parse
    /// error.
    #[expect(
        clippy::too_many_lines,
        reason = "accepted-body accounting and retry classification belong to one attempt"
    )]
    async fn handle_success_response<F>(
        &self,
        response: reqwest::Response,
        request: &mut TurnRequest<'_>,
        in_flight: &mut InFlightAttempt<'_, F>,
        client: &mut reqwest::Client,
        disable_connection_reuse: &mut bool,
        settle_usage: &mut impl FnMut(TurnUsageSettlement),
    ) -> AttemptResult
    where
        F: FnMut(AgentRunnerTelemetryEvent),
    {
        let parse_result = self
            .backend
            .parse_response(
                response,
                &request.config.model_config.model,
                self.stall_window,
            )
            .await;
        let parse_ms = in_flight.parse_ms();
        let total_ms = in_flight.total_ms();

        let (
            terminal_class,
            usage,
            provider_request_id,
            responses_failed,
            termination,
            response_model,
            error_detail,
        ) = match &parse_result {
            Ok(turn) => (
                ApiAttemptTerminalClass::Completed,
                turn.usage,
                turn.provider_request_id.clone(),
                None,
                turn.termination.clone(),
                turn.response_model.clone(),
                None,
            ),
            Err(error) => (
                error.terminal_class(),
                error.usage(),
                error.provider_request_id().map(str::to_owned),
                error
                    .responses_failed()
                    .map(|failed| Box::new(failed.clone())),
                None,
                None,
                Some(error.telemetry_detail().to_owned()),
            ),
        };
        let body_read_transport = terminal_class == ApiAttemptTerminalClass::Transport;
        let turn_index = in_flight.identity.turn_index;
        let attempt = in_flight.identity.attempt;
        let request_ms = in_flight.request_ms();
        let status_code = in_flight.status_code;
        let phase = Some(in_flight.phase);

        let settled_model = in_flight.model.clone();
        let settled_provider_request_id = provider_request_id.clone();
        let settled_response_model = response_model.clone();
        let (purpose, compaction_id, summary_step) = in_flight.purpose_fields();

        in_flight.finish(ApiAttemptTelemetry {
            turn_index,
            attempt,
            request_ms,
            parse_ms,
            total_ms,
            history_items: in_flight.history_items,
            status_code,
            error: error_detail,
            usage: usage.map(|reported| reported.usage),
            usage_presence: usage_presence(usage),
            provider: in_flight.provider,
            model: in_flight.model.clone(),
            response_model,
            termination,
            terminal_class: Some(terminal_class),
            provider_request_id,
            responses_failed,
            phase,
            request_overrides: RequestOverridesSnapshot::from(&request.overrides),
            purpose,
            compaction_id: compaction_id.clone(),
            summary_step,
        });

        if let Some(usage) = usage {
            settle_usage(TurnUsageSettlement {
                identity: in_flight.identity.clone(),
                terminal_class,
                usage,
                model: settled_model,
                provider_request_id: settled_provider_request_id,
                response_model: settled_response_model,
            });
        }

        match parse_result {
            Ok(turn) => AttemptResult::Completed(turn),
            Err(parse_error) if body_read_transport => {
                self.schedule_transport_retry(
                    parse_error.into_error(),
                    in_flight,
                    client,
                    disable_connection_reuse,
                    &mut request.overrides,
                )
                .await
            },
            Err(parse_error) => match self
                .recover_response_failed(parse_error, in_flight, &request.overrides)
                .await
            {
                Ok(()) => AttemptResult::RetryNeeded,
                Err(parse_error) => AttemptResult::Terminal(parse_error.into_error()),
            },
        }
    }

    /// Handle a terminal `response.failed` parse error after an accepted HTTP
    /// 2xx. Retries a transient `server_error` under the bounded retry/deadline
    /// policy, or returns the error when the failure is semantic (auth,
    /// invalid-request, quota, context, policy) or retries are exhausted.
    async fn recover_response_failed<F>(
        &self,
        parse_error: ProviderAttemptError,
        in_flight: &mut InFlightAttempt<'_, F>,
        request_overrides: &RequestOverrides,
    ) -> Result<(), ProviderAttemptError>
    where
        F: FnMut(AgentRunnerTelemetryEvent),
    {
        let Some(failed) = parse_error.responses_failed() else {
            return Err(parse_error);
        };
        let decision = retry::classify_response_failed(
            &self.retry_policy,
            failed.error_code.as_deref(),
            failed.error_type.as_deref(),
            in_flight.identity.attempt,
            in_flight.identity.session_id,
        );
        match decision {
            retry::RetryDecision::Retry { status } => {
                let turn_index = in_flight.identity.turn_index;
                in_flight.report(AgentRunnerTelemetryEvent::retry_scheduled(
                    RetryScheduledTelemetry::from_status(
                        &status,
                        turn_index,
                        false,
                        request_overrides,
                    ),
                ));
                in_flight.report(AgentRunnerTelemetryEvent::retry_wait(
                    wait_for_retry(&status, turn_index).await,
                ));
                Ok(())
            },
            retry::RetryDecision::RetryWithOverrides { .. } | retry::RetryDecision::DoNotRetry => {
                Err(parse_error)
            },
        }
    }

    /// Read a non-2xx body under the stall deadline, keeping whatever arrived
    /// so the provider's diagnostic text is still available.
    async fn read_failure_body(&self, response: reqwest::Response, model: &str) -> FailureBody {
        let mut response = response;
        match read_body(&mut response, model, self.stall_window).await {
            BodyReadOutcome::Complete(bytes) => FailureBody {
                body: String::from_utf8_lossy(&bytes).into_owned(),
                failure: None,
            },
            BodyReadOutcome::Stalled(StalledBody { bytes, stall }) => {
                let body = String::from_utf8_lossy(&bytes).into_owned();
                let failure = BodyReadFailure::stalled(stall.with_preview(&bytes));
                FailureBody {
                    body,
                    failure: Some(failure),
                }
            },
            BodyReadOutcome::Failed(FailedBody { bytes, error }) => FailureBody {
                body: String::from_utf8_lossy(&bytes).into_owned(),
                failure: Some(BodyReadFailure::failed(error)),
            },
        }
    }

    /// Handle a non-2xx HTTP response: record attempt telemetry, then classify
    /// the failure for retry (rate-limit, overload, context overflow) or return
    /// it as an `ApiError`.
    async fn handle_http_failure<F>(
        &self,
        response: reqwest::Response,
        request: &mut TurnRequest<'_>,
        in_flight: &mut InFlightAttempt<'_, F>,
        settle_usage: &mut impl FnMut(TurnUsageSettlement),
    ) -> AttemptResult
    where
        F: FnMut(AgentRunnerTelemetryEvent),
    {
        let headers = response.headers().clone();
        let FailureBody {
            body,
            failure: body_read_failure,
        } = self
            .read_failure_body(response, &request.config.model_config.model)
            .await;
        let failure = HttpFailure {
            status: in_flight.status_code.unwrap_or_default(),
            headers,
            body,
        };
        let usage = self.backend.reported_usage(failure.body.as_bytes());
        let terminal_class = body_read_terminal_class(body_read_failure.as_ref());
        let error =
            http_failure_telemetry_text(failure.status, &failure.body, body_read_failure.as_ref());
        let turn_index = in_flight.identity.turn_index;
        let attempt = in_flight.identity.attempt;
        let (purpose, compaction_id, summary_step) = in_flight.purpose_fields();
        in_flight.finish(ApiAttemptTelemetry {
            turn_index,
            attempt,
            request_ms: in_flight.request_ms(),
            parse_ms: 0,
            total_ms: in_flight.total_ms(),
            history_items: in_flight.history_items,
            status_code: in_flight.status_code,
            error: Some(error),
            usage: usage.map(|reported| reported.usage),
            usage_presence: usage_presence(usage),
            provider: in_flight.provider,
            model: in_flight.model.clone(),
            response_model: None,
            termination: None,
            terminal_class: Some(terminal_class),
            provider_request_id: None,
            responses_failed: None,
            phase: Some(in_flight.phase),
            request_overrides: RequestOverridesSnapshot::from(&request.overrides),
            purpose,
            compaction_id: compaction_id.clone(),
            summary_step,
        });
        if let Some(usage) = usage {
            settle_usage(TurnUsageSettlement {
                identity: in_flight.identity.clone(),
                terminal_class,
                usage,
                model: in_flight.model.clone(),
                provider_request_id: None,
                response_model: None,
            });
        }

        match retry::classify_http_failure(
            &self.retry_policy,
            &failure,
            attempt,
            in_flight.identity.session_id,
            &request.overrides,
        ) {
            retry::RetryDecision::Retry { status } => {
                in_flight.report(AgentRunnerTelemetryEvent::retry_scheduled(
                    RetryScheduledTelemetry::from_status(
                        &status,
                        turn_index,
                        false,
                        &request.overrides,
                    ),
                ));
                in_flight.report(AgentRunnerTelemetryEvent::retry_wait(
                    wait_for_retry(&status, turn_index).await,
                ));
                AttemptResult::RetryNeeded
            },
            retry::RetryDecision::RetryWithOverrides { status, overrides } => {
                in_flight.report(AgentRunnerTelemetryEvent::retry_scheduled(
                    RetryScheduledTelemetry::from_status(&status, turn_index, true, &overrides),
                ));
                in_flight.report(AgentRunnerTelemetryEvent::Compensation(
                    CompensationEventTelemetry::new(CompensationKind::ContextOverflowRetry, None),
                ));
                request.overrides = overrides;
                in_flight.report(AgentRunnerTelemetryEvent::retry_wait(
                    wait_for_retry(&status, turn_index).await,
                ));
                AttemptResult::RetryNeeded
            },
            retry::RetryDecision::DoNotRetry => {
                let failure = with_body_read_detail(failure, body_read_failure.as_ref());
                AttemptResult::Terminal(
                    api_error_from_failure(request.config, &failure, body_read_failure).into(),
                )
            },
        }
    }

    /// Handle a request-phase transport error: record attempt telemetry, then
    /// classify a retryable transport failure (swapping in a no-reuse client)
    /// or return the error as terminal.
    async fn handle_transport_error<F>(
        &self,
        error: anyhow::Error,
        in_flight: &mut InFlightAttempt<'_, F>,
        client: &mut reqwest::Client,
        disable_connection_reuse: &mut bool,
        request_overrides: &mut RequestOverrides,
    ) -> AttemptResult
    where
        F: FnMut(AgentRunnerTelemetryEvent),
    {
        let error_detail = format!("{error:#}");
        debug!(
            target: "cake",
            error = %error_detail,
            "API request failed before receiving an HTTP response"
        );

        let terminal_class = if error_is_timeout(&error) {
            ApiAttemptTerminalClass::Timeout
        } else {
            ApiAttemptTerminalClass::Transport
        };
        let turn_index = in_flight.identity.turn_index;
        let attempt = in_flight.identity.attempt;
        let (purpose, compaction_id, summary_step) = in_flight.purpose_fields();
        in_flight.finish(ApiAttemptTelemetry {
            turn_index,
            attempt,
            request_ms: in_flight.request_ms(),
            parse_ms: 0,
            total_ms: in_flight.total_ms(),
            history_items: in_flight.history_items,
            status_code: None,
            error: Some(error_detail),
            usage: None,
            usage_presence: UsagePresence::Unreported,
            provider: in_flight.provider,
            model: in_flight.model.clone(),
            response_model: None,
            termination: None,
            terminal_class: Some(terminal_class),
            provider_request_id: None,
            responses_failed: None,
            phase: Some(in_flight.phase),
            request_overrides: RequestOverridesSnapshot::from(&*request_overrides),
            purpose,
            compaction_id,
            summary_step,
        });

        self.schedule_transport_retry(
            error,
            in_flight,
            client,
            disable_connection_reuse,
            request_overrides,
        )
        .await
    }

    /// Schedule the bounded retry for a transient transport failure, or
    /// return it terminal.
    ///
    /// Shared by the request phase (before any HTTP response) and the accepted
    /// body-read phase, so both use the same classifier, retry cap, backoff,
    /// and stale-connection no-reuse policy. Only one of the two phases can
    /// fail for a given attempt, so a body-read failure cannot be retried
    /// twice, and no tool call has run when this is reached because the
    /// accepted body is fully buffered before the turn returns.
    async fn schedule_transport_retry<F>(
        &self,
        error: anyhow::Error,
        in_flight: &mut InFlightAttempt<'_, F>,
        client: &mut reqwest::Client,
        disable_connection_reuse: &mut bool,
        request_overrides: &mut RequestOverrides,
    ) -> AttemptResult
    where
        F: FnMut(AgentRunnerTelemetryEvent),
    {
        let turn_index = in_flight.identity.turn_index;
        match retry::classify_transport_error(
            &self.retry_policy,
            &error,
            in_flight.identity.attempt,
            in_flight.identity.session_id,
        ) {
            retry::RetryDecision::Retry { status } => {
                if retry::should_disable_connection_reuse(&error) && !*disable_connection_reuse {
                    *client = build_http_client(true);
                    *disable_connection_reuse = true;
                }
                in_flight.report(AgentRunnerTelemetryEvent::retry_scheduled(
                    RetryScheduledTelemetry::from_status(
                        &status,
                        turn_index,
                        false,
                        request_overrides,
                    ),
                ));
                in_flight.report(AgentRunnerTelemetryEvent::retry_wait(
                    wait_for_retry(&status, turn_index).await,
                ));
                AttemptResult::RetryNeeded
            },
            retry::RetryDecision::RetryWithOverrides { status, overrides } => {
                in_flight.report(AgentRunnerTelemetryEvent::retry_scheduled(
                    RetryScheduledTelemetry::from_status(&status, turn_index, true, &overrides),
                ));
                *request_overrides = overrides;
                in_flight.report(AgentRunnerTelemetryEvent::retry_wait(
                    wait_for_retry(&status, turn_index).await,
                ));
                AttemptResult::RetryNeeded
            },
            retry::RetryDecision::DoNotRetry => AttemptResult::Terminal(error),
        }
    }
}

/// Presence marker for an attempt record: the reported usage's own presence, or
/// `unreported` when the attempt reported no usage at all.
const fn usage_presence(usage: Option<ReportedUsage>) -> UsagePresence {
    match usage {
        Some(reported) => reported.presence,
        None => UsagePresence::Unreported,
    }
}

fn body_read_terminal_class(
    body_read_failure: Option<&BodyReadFailure>,
) -> ApiAttemptTerminalClass {
    body_read_failure.map_or(ApiAttemptTerminalClass::Http, |body_read_failure| {
        if body_read_failure.is_stalled() || body_read_failure.error().is_some_and(error_is_timeout)
        {
            ApiAttemptTerminalClass::Timeout
        } else {
            ApiAttemptTerminalClass::Http
        }
    })
}

fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

async fn wait_for_retry(status: &RetryStatus, turn_index: u32) -> RetryWaitTelemetry {
    debug!(
        target: "cake",
        reason = ?status.reason,
        detail = %status.detail,
        delay_ms = status.delay.as_millis(),
        attempt = status.attempt,
        max_attempts = status.max_retries,
        "Retrying API request"
    );

    let started_at = Utc::now();
    let start = Instant::now();
    if !status.delay.is_zero() {
        sleep(status.delay).await;
    }
    RetryWaitTelemetry {
        turn_index,
        attempt: status.attempt,
        started_at,
        completed_at: Utc::now(),
        duration_ms: elapsed_ms(start),
    }
}

fn http_failure_telemetry_text(
    status: u16,
    body: &str,
    body_read_failure: Option<&BodyReadFailure>,
) -> String {
    match body_read_failure {
        Some(body_read_failure) if body.is_empty() => {
            format!("{status} {}", body_read_failure.telemetry_detail())
        },
        Some(body_read_failure) => {
            format!("{status} {}\n{body}", body_read_failure.telemetry_detail())
        },
        None => format!("{status} {body}"),
    }
}

fn with_body_read_detail(
    failure: HttpFailure,
    body_read_failure: Option<&BodyReadFailure>,
) -> HttpFailure {
    match body_read_failure {
        Some(body_read_failure) => {
            let detail = if failure.body.is_empty() {
                body_read_failure.terminal_detail()
            } else {
                body_read_failure.telemetry_detail()
            };
            let body = if failure.body.is_empty() {
                detail
            } else {
                format!("{detail}\n\n{}", failure.body)
            };
            HttpFailure { body, ..failure }
        },
        None => failure,
    }
}

fn api_error_from_failure(
    config: &ResolvedModelConfig,
    failure: &HttpFailure,
    body_read_failure: Option<BodyReadFailure>,
) -> crate::exit_code::ApiError {
    debug!(target: "cake", "{}", failure.body);

    let verdict = termination::classify(
        IntendedLimits::default(),
        TurnEvidence::HttpFailure {
            status: failure.status,
            body: &failure.body,
        },
    );
    let input_overflow = match verdict.outcome {
        TerminationOutcome::InputDoesNotFit(overflow) => Some(overflow),
        _ => None,
    };

    crate::exit_code::ApiError {
        status: failure.status,
        body: format_api_error_body(&config.model_config.model, &failure.body),
        input_overflow,
        body_read_failure,
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
