use serde::de::DeserializeOwned;
use tracing::{debug, trace, warn};

use crate::clients::agent::TurnResult;
use crate::clients::backend::{
    Backend, FinalOutputConstraint, ResponseDecodeError, ResponseParseError, ToolChoice,
};
use crate::clients::chat_types::ChatUsage;
use crate::clients::provider_strategy::ProviderStrategy;
use crate::clients::response_stall::ResponseStall;
use crate::clients::responses::ResponsesStreamFailed;
use crate::clients::responses_types::ApiUsage;
use crate::clients::retry::RequestOverrides;
use crate::clients::tools::Tool;
use crate::clients::{chat_completions, responses};
use crate::config::model::ResolvedModelConfig;
use crate::session_telemetry::ResponsesFailedMetadata;
use crate::types::{
    ApiAttemptTerminalClass, ConversationItem, InputTokensDetails, OutputTokensDetails,
    ReportedUsage, Usage, UsagePresence,
};

/// One provider turn's request-shaping inputs.
///
/// Carries the configuration and conversation references together with the
/// per-attempt overrides, so the transport, builders, and retry loop thread one
/// value instead of six parameters.
#[derive(Debug)]
pub(super) struct TurnRequest<'a> {
    pub(super) config: &'a ResolvedModelConfig,
    pub(super) history: &'a [ConversationItem],
    pub(super) tools: &'a [Tool],
    pub(super) tool_choice: Option<ToolChoice>,
    pub(super) overrides: RequestOverrides,
    pub(super) constraint: Option<FinalOutputConstraint<'a>>,
}

impl<'a> TurnRequest<'a> {
    pub(super) const fn new(
        config: &'a ResolvedModelConfig,
        history: &'a [ConversationItem],
        tools: &'a [Tool],
        tool_choice: Option<ToolChoice>,
        constraint: Option<FinalOutputConstraint<'a>>,
    ) -> Self {
        Self {
            config,
            history,
            tools,
            tool_choice,
            overrides: RequestOverrides {
                max_output_tokens: config.model_config.max_output_tokens,
                reasoning_max_tokens: config.model_config.reasoning_max_tokens,
                context_overflow_retry_used: false,
            },
            constraint,
        }
    }
}

/// A provider attempt error with the attempt-level facts the runner needs.
///
/// The provider edge extracts usage, terminal class, telemetry text, and
/// Responses failure metadata once. The runner no longer walks the error chain
/// separately for each fact, and a terminal error returns the original source
/// unchanged.
#[derive(Debug)]
pub(super) struct ProviderAttemptError(Box<ProviderAttemptErrorInner>);

#[derive(Debug)]
struct ProviderAttemptErrorInner {
    error: anyhow::Error,
    usage: Option<ReportedUsage>,
    terminal_class: ApiAttemptTerminalClass,
    telemetry_detail: String,
    provider_request_id: Option<String>,
    responses_failed: Option<Box<ResponsesFailedMetadata>>,
}

impl ProviderAttemptError {
    fn new(error: anyhow::Error) -> Self {
        let responses_failed = error.downcast_ref::<ResponsesStreamFailed>();
        let terminal_class = if error_is_timeout(&error) {
            ApiAttemptTerminalClass::Timeout
        } else if responses_failed.is_some() {
            ApiAttemptTerminalClass::ResponseFailed
        } else if is_body_read_transport_error(&error) {
            ApiAttemptTerminalClass::Transport
        } else {
            ApiAttemptTerminalClass::BodyParse
        };
        let usage = responses_failed
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
            });
        let telemetry_detail = crate::clients::telemetry_safe_provider_error_detail(&error)
            .unwrap_or_else(|| format!("{error:#}"));
        let provider_request_id =
            responses_failed.and_then(|failed| failed.metadata().provider_request_id.clone());
        let responses_failed = responses_failed.map(|failed| Box::new(failed.metadata().clone()));

        Self(Box::new(ProviderAttemptErrorInner {
            error,
            usage,
            terminal_class,
            telemetry_detail,
            provider_request_id,
            responses_failed,
        }))
    }

    pub(super) const fn usage(&self) -> Option<ReportedUsage> {
        self.0.usage
    }

    pub(super) const fn terminal_class(&self) -> ApiAttemptTerminalClass {
        self.0.terminal_class
    }

    pub(super) fn telemetry_detail(&self) -> &str {
        &self.0.telemetry_detail
    }

    pub(super) fn provider_request_id(&self) -> Option<&str> {
        self.0.provider_request_id.as_deref()
    }

    pub(super) fn responses_failed(&self) -> Option<&ResponsesFailedMetadata> {
        self.0.responses_failed.as_deref()
    }

    pub(super) fn into_error(self) -> anyhow::Error {
        self.0.error
    }
}

pub(super) fn error_is_timeout(error: &anyhow::Error) -> bool {
    error.downcast_ref::<ResponseStall>().is_some()
        || error.chain().any(|cause| {
            cause
                .downcast_ref::<reqwest::Error>()
                .is_some_and(reqwest::Error::is_timeout)
        })
}

fn is_body_read_transport_error(error: &anyhow::Error) -> bool {
    if error.downcast_ref::<ResponseDecodeError>().is_some() {
        return false;
    }

    !error_is_timeout(error)
        && error
            .chain()
            .any(|cause| cause.downcast_ref::<reqwest::Error>().is_some())
}

impl Backend {
    const fn api_path(self) -> &'static str {
        match self {
            Self::Responses => "/responses",
            Self::ChatCompletions => "/chat/completions",
        }
    }

    pub(super) fn build_request_json(self, request: &TurnRequest<'_>) -> anyhow::Result<Vec<u8>> {
        match self {
            Self::Responses => responses::build_request_json(request),
            Self::ChatCompletions => chat_completions::build_request_json(request),
        }
    }

    pub(super) async fn send_request_json(
        self,
        client: &reqwest::Client,
        config: &ResolvedModelConfig,
        session_id: uuid::Uuid,
        request: Vec<u8>,
    ) -> anyhow::Result<reqwest::Response> {
        let request = self.build_http_request(client, config, session_id, request);
        Ok(request.send().await?)
    }

    pub(super) async fn send_request(
        self,
        client: &reqwest::Client,
        session_id: uuid::Uuid,
        request: &TurnRequest<'_>,
    ) -> anyhow::Result<reqwest::Response> {
        let body = self.build_request_json(request)?;
        self.send_request_json(client, request.config, session_id, body)
            .await
    }

    pub(super) fn reported_usage(self, body: &[u8]) -> Option<ReportedUsage> {
        match self {
            Self::Responses => reported_usage::<ApiUsage>(body),
            Self::ChatCompletions => reported_usage::<ChatUsage>(body),
        }
    }

    pub(super) async fn parse_response(
        self,
        response: reqwest::Response,
        model: &str,
        stall_window: std::time::Duration,
    ) -> Result<TurnResult, ProviderAttemptError> {
        let result = match self {
            Self::Responses => responses::parse_response(response, model, stall_window).await,
            Self::ChatCompletions => {
                chat_completions::parse_response(response, model, stall_window).await
            },
        };
        result.map_err(ProviderAttemptError::new)
    }

    fn build_http_request(
        self,
        client: &reqwest::Client,
        config: &ResolvedModelConfig,
        session_id: uuid::Uuid,
        request: Vec<u8>,
    ) -> reqwest::RequestBuilder {
        let strategy = ProviderStrategy::from_config(config);
        let url = format!(
            "{}{}",
            config.model_config.base_url.trim_end_matches('/'),
            self.api_path()
        );
        debug!(target: "cake", "{url}");
        if tracing::enabled!(tracing::Level::TRACE) {
            let request_json = String::from_utf8_lossy(&request);
            trace!(target: "cake", "{request_json}");
        }

        strategy
            .apply_headers(
                client
                    .post(&url)
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(request),
                session_id,
            )
            .bearer_auth(&config.api_key)
    }
}

pub(super) trait ProviderUsage {
    const BACKEND: &'static str;
    const INPUT_FIELD: &'static str;
    const OUTPUT_FIELD: &'static str;

    fn input_tokens(&self) -> Option<u64>;
    fn output_tokens(&self) -> Option<u64>;
    fn total_tokens(&self) -> Option<u64>;
    fn cached_tokens(&self) -> Option<u64>;
    fn cache_write_tokens(&self) -> Option<u64>;
    fn reasoning_tokens(&self) -> Option<u64>;
}

impl ProviderUsage for ChatUsage {
    const BACKEND: &'static str = "Chat Completions";
    const INPUT_FIELD: &'static str = "prompt_tokens";
    const OUTPUT_FIELD: &'static str = "completion_tokens";

    fn input_tokens(&self) -> Option<u64> {
        self.prompt_tokens
    }

    fn output_tokens(&self) -> Option<u64> {
        self.completion_tokens
    }

    fn total_tokens(&self) -> Option<u64> {
        self.total_tokens
    }

    fn cached_tokens(&self) -> Option<u64> {
        self.prompt_tokens_details
            .as_ref()
            .and_then(|details| details.cached_tokens)
    }

    fn cache_write_tokens(&self) -> Option<u64> {
        self.prompt_tokens_details
            .as_ref()
            .and_then(|details| details.cache_write_tokens)
    }

    fn reasoning_tokens(&self) -> Option<u64> {
        self.completion_tokens_details
            .as_ref()
            .and_then(|details| details.reasoning_tokens)
    }
}

impl ProviderUsage for ApiUsage {
    const BACKEND: &'static str = "Responses API";
    const INPUT_FIELD: &'static str = "input_tokens";
    const OUTPUT_FIELD: &'static str = "output_tokens";

    fn input_tokens(&self) -> Option<u64> {
        self.input_tokens
    }

    fn output_tokens(&self) -> Option<u64> {
        self.output_tokens
    }

    fn total_tokens(&self) -> Option<u64> {
        self.total_tokens
    }

    fn cached_tokens(&self) -> Option<u64> {
        self.input_tokens_details
            .as_ref()
            .and_then(|details| details.cached_tokens)
    }

    fn cache_write_tokens(&self) -> Option<u64> {
        self.input_tokens_details
            .as_ref()
            .and_then(|details| details.cache_write_tokens)
    }

    fn reasoning_tokens(&self) -> Option<u64> {
        self.output_tokens_details
            .as_ref()
            .and_then(|details| details.reasoning_tokens)
    }
}

pub(super) fn map_usage<U: ProviderUsage>(api_usage: &U, response_id: &str) -> ReportedUsage {
    let input_tokens = api_usage.input_tokens();
    let output_tokens = api_usage.output_tokens();
    let total_tokens = api_usage.total_tokens();

    if input_tokens.is_none() {
        warn!(
            target: "cake",
            response_id,
            field = U::INPUT_FIELD,
            "{} usage missing field, defaulting to 0",
            U::BACKEND
        );
    }
    if output_tokens.is_none() {
        warn!(
            target: "cake",
            response_id,
            field = U::OUTPUT_FIELD,
            "{} usage missing field, defaulting to 0",
            U::BACKEND
        );
    }
    if total_tokens.is_none() {
        warn!(
            target: "cake",
            response_id,
            field = "total_tokens",
            "{} usage missing field, defaulting to 0",
            U::BACKEND
        );
    }

    let presence = UsagePresence::from_required_counters(input_tokens, output_tokens, total_tokens);
    ReportedUsage::new(
        Usage {
            input_tokens: input_tokens.unwrap_or(0),
            output_tokens: output_tokens.unwrap_or(0),
            total_tokens: total_tokens.unwrap_or(0),
            input_tokens_details: InputTokensDetails {
                cached_tokens: api_usage.cached_tokens().unwrap_or(0),
                cache_write_tokens: api_usage.cache_write_tokens().unwrap_or(0),
            },
            output_tokens_details: OutputTokensDetails {
                reasoning_tokens: api_usage.reasoning_tokens().unwrap_or(0),
            },
        },
        presence,
    )
}

pub(super) fn reported_usage<U>(body: &[u8]) -> Option<ReportedUsage>
where
    U: ProviderUsage + DeserializeOwned,
{
    let value = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let response_id = value
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<missing id>");
    let api_usage = serde_json::from_value::<U>(value.get("usage")?.clone()).ok()?;
    Some(map_usage(&api_usage, response_id))
}
