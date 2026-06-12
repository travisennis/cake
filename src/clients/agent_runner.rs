use std::time::{Duration, Instant};

use tokio::time::sleep;
use tracing::debug;

use crate::clients::agent::TurnResult;
use crate::clients::backend::Backend;
use crate::clients::retry::{self, HttpFailure, RequestOverrides, RetryPolicy, RetryStatus};
use crate::clients::tools::Tool;
use crate::config::model::ResolvedModelConfig;
use crate::session_telemetry::{
    AgentRunnerTelemetryEvent, ApiAttemptTelemetry, RequestOverridesSnapshot,
    RetryScheduledTelemetry,
};
use crate::types::ConversationItem;

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
        reason = "runner needs config, state, callbacks, and telemetry context at the API boundary"
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "retry loop keeps request, parse, retry, and telemetry sequencing together"
    )]
    pub(super) async fn complete_turn<'a>(
        &mut self,
        config: &ResolvedModelConfig,
        session_id: uuid::Uuid,
        turn_index: u32,
        history: &'a [ConversationItem],
        tools: &'a [Tool],
        report_retry: impl Fn(&RetryStatus) + Send + Sync,
        mut report_telemetry: impl FnMut(AgentRunnerTelemetryEvent),
    ) -> anyhow::Result<TurnResult> {
        let mut attempt = 1;
        let mut request_overrides = RequestOverrides {
            max_output_tokens: config.model_config.max_output_tokens,
            reasoning_max_tokens: config.model_config.reasoning_max_tokens,
            context_overflow_retry_used: false,
        };
        let mut disable_connection_reuse = false;

        loop {
            let total_start = Instant::now();
            let request_start = Instant::now();
            let request_result = self
                .backend
                .send_request(&self.client, config, history, tools, &request_overrides)
                .await;
            let request_ms = elapsed_ms(request_start);

            match request_result {
                Ok(response) => {
                    let status_code = response.status().as_u16();
                    if response.status().is_success() {
                        let parse_start = Instant::now();
                        let parse_result = self.backend.parse_response(response).await;
                        let parse_ms = elapsed_ms(parse_start);
                        let total_ms = elapsed_ms(total_start);
                        let usage = parse_result.as_ref().ok().and_then(|turn| turn.usage);
                        let error = parse_result.as_ref().err().map(ToString::to_string);
                        report_telemetry(AgentRunnerTelemetryEvent::ApiAttempt(
                            ApiAttemptTelemetry {
                                turn_index,
                                attempt,
                                request_ms,
                                parse_ms,
                                total_ms,
                                history_items: history.len(),
                                status_code: Some(status_code),
                                error,
                                usage,
                                request_overrides: RequestOverridesSnapshot::from(
                                    &request_overrides,
                                ),
                            },
                        ));

                        if disable_connection_reuse {
                            self.client = build_http_client(false);
                        }

                        return parse_result;
                    }

                    let headers = response.headers().clone();
                    let body = response.text().await.unwrap_or_default();

                    let failure = HttpFailure {
                        status: status_code,
                        headers,
                        body,
                    };
                    report_telemetry(AgentRunnerTelemetryEvent::ApiAttempt(ApiAttemptTelemetry {
                        turn_index,
                        attempt,
                        request_ms,
                        parse_ms: 0,
                        total_ms: elapsed_ms(total_start),
                        history_items: history.len(),
                        status_code: Some(status_code),
                        error: Some(format!("{} {}", failure.status, failure.body)),
                        usage: None,
                        request_overrides: RequestOverridesSnapshot::from(&request_overrides),
                    }));

                    match retry::classify_http_failure(
                        &self.retry_policy,
                        &failure,
                        attempt,
                        session_id,
                        &request_overrides,
                    ) {
                        retry::RetryDecision::Retry { status } => {
                            report_telemetry(AgentRunnerTelemetryEvent::RetryScheduled(
                                RetryScheduledTelemetry::from_status(
                                    &status,
                                    turn_index,
                                    false,
                                    &request_overrides,
                                ),
                            ));
                            wait_for_retry(&status, &report_retry).await;
                            attempt += 1;
                        },
                        retry::RetryDecision::RetryWithOverrides { status, overrides } => {
                            report_telemetry(AgentRunnerTelemetryEvent::RetryScheduled(
                                RetryScheduledTelemetry::from_status(
                                    &status, turn_index, true, &overrides,
                                ),
                            ));
                            request_overrides = overrides;
                            wait_for_retry(&status, &report_retry).await;
                            attempt += 1;
                        },
                        retry::RetryDecision::DoNotRetry => {
                            return Err(api_error_from_failure(
                                &config.model_config.model,
                                &failure,
                            )
                            .into());
                        },
                    }
                },
                Err(error) => {
                    report_telemetry(AgentRunnerTelemetryEvent::ApiAttempt(ApiAttemptTelemetry {
                        turn_index,
                        attempt,
                        request_ms,
                        parse_ms: 0,
                        total_ms: elapsed_ms(total_start),
                        history_items: history.len(),
                        status_code: None,
                        error: Some(error.to_string()),
                        usage: None,
                        request_overrides: RequestOverridesSnapshot::from(&request_overrides),
                    }));
                    match retry::classify_transport_error(
                        &self.retry_policy,
                        &error,
                        attempt,
                        session_id,
                    ) {
                        retry::RetryDecision::Retry { status } => {
                            if retry::should_disable_connection_reuse(&error)
                                && !disable_connection_reuse
                            {
                                self.client = build_http_client(true);
                                disable_connection_reuse = true;
                            }

                            report_telemetry(AgentRunnerTelemetryEvent::RetryScheduled(
                                RetryScheduledTelemetry::from_status(
                                    &status,
                                    turn_index,
                                    false,
                                    &request_overrides,
                                ),
                            ));
                            wait_for_retry(&status, &report_retry).await;
                            attempt += 1;
                        },
                        retry::RetryDecision::RetryWithOverrides { status, overrides } => {
                            report_telemetry(AgentRunnerTelemetryEvent::RetryScheduled(
                                RetryScheduledTelemetry::from_status(
                                    &status, turn_index, true, &overrides,
                                ),
                            ));
                            request_overrides = overrides;
                            wait_for_retry(&status, &report_retry).await;
                            attempt += 1;
                        },
                        retry::RetryDecision::DoNotRetry => return Err(error),
                    }
                },
            }
        }
    }
}

fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

async fn wait_for_retry(
    status: &RetryStatus,
    report_retry: &(impl Fn(&RetryStatus) + Send + Sync),
) {
    report_retry(status);
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
