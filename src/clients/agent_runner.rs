use std::time::Duration;

use tokio::time::sleep;
use tracing::debug;

use crate::clients::agent::TurnResult;
use crate::clients::backend::Backend;
use crate::clients::retry::{self, HttpFailure, RequestOverrides, RetryPolicy, RetryStatus};
use crate::clients::tools::Tool;
use crate::clients::types::ConversationItem;
use crate::config::model::ResolvedModelConfig;

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

    pub(super) async fn complete_turn(
        &mut self,
        config: &ResolvedModelConfig,
        session_id: uuid::Uuid,
        history: &[ConversationItem],
        tools: &[Tool],
        report_retry: impl Fn(&RetryStatus) + Send + Sync,
    ) -> anyhow::Result<TurnResult> {
        let mut attempt = 1;
        let mut request_overrides = RequestOverrides {
            max_output_tokens: config.model_config.max_output_tokens,
            reasoning_max_tokens: config.model_config.reasoning_max_tokens,
            context_overflow_retry_used: false,
        };
        let mut disable_connection_reuse = false;

        loop {
            let request_result = self
                .backend
                .send_request(&self.client, config, history, tools, &request_overrides)
                .await;

            match request_result {
                Ok(response) => {
                    if response.status().is_success() {
                        if disable_connection_reuse {
                            self.client = build_http_client(false);
                        }

                        return self.backend.parse_response(response).await;
                    }

                    let failure = HttpFailure {
                        status: response.status().as_u16(),
                        headers: response.headers().clone(),
                        body: response.text().await?,
                    };

                    match retry::classify_http_failure(
                        &self.retry_policy,
                        &failure,
                        attempt,
                        session_id,
                        &request_overrides,
                    ) {
                        retry::RetryDecision::Retry { status } => {
                            wait_for_retry(&status, &report_retry).await;
                            attempt += 1;
                        },
                        retry::RetryDecision::RetryWithOverrides { status, overrides } => {
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
                Err(error) => match retry::classify_transport_error(
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

                        wait_for_retry(&status, &report_retry).await;
                        attempt += 1;
                    },
                    retry::RetryDecision::RetryWithOverrides { status, overrides } => {
                        request_overrides = overrides;
                        wait_for_retry(&status, &report_retry).await;
                        attempt += 1;
                    },
                    retry::RetryDecision::DoNotRetry => return Err(error),
                },
            }
        }
    }
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
    serde_json::from_str::<serde_json::Value>(error_text).map_or_else(
        |_err| format!("{model}\n\n{error_text}"),
        |resp_json| {
            serde_json::to_string_pretty(&resp_json).map_or_else(
                |_| format!("{model}\n\n{error_text}"),
                |formatted| format!("{model}\n\n{formatted}"),
            )
        },
    )
}
