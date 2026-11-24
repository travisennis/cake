use crate::clients::agent::TurnResult;
use crate::clients::retry::RequestOverrides;
use crate::clients::tools::Tool;
use crate::clients::{chat_completions, responses};
use crate::config::model::{ApiType, ResolvedModelConfig};
use crate::types::{ConversationItem, ReportedUsage};

/// A successful provider response whose body does not match the backend's
/// JSON envelope.
///
/// Keeping this as a typed boundary error lets callers distinguish a body
/// decode failure, which may be transient, from later semantic parsing errors
/// that should remain terminal.
#[derive(Debug, thiserror::Error)]
#[error(
    "failed to decode {backend} response body: {source}; first {preview_len} bytes: {preview:?}"
)]
pub struct ResponseDecodeError {
    backend: &'static str,
    preview_len: usize,
    preview: String,
    reported_usage: Option<ReportedUsage>,
    #[source]
    source: serde_json::Error,
}

impl ResponseDecodeError {
    pub fn new(
        backend: &'static str,
        body: &[u8],
        reported_usage: Option<ReportedUsage>,
        source: serde_json::Error,
    ) -> Self {
        let preview_len = body.len().min(400);
        Self {
            backend,
            preview_len,
            preview: String::from_utf8_lossy(&body[..preview_len]).into_owned(),
            reported_usage,
            source,
        }
    }

    pub(super) const fn usage(&self) -> Option<ReportedUsage> {
        self.reported_usage
    }

    /// Detail without the response-body preview, safe for telemetry.
    pub fn telemetry_detail(&self) -> String {
        format!(
            "failed to decode {} response body: {:?} error at line {} column {}",
            self.backend,
            self.source.classify(),
            self.source.line(),
            self.source.column()
        )
    }
}

/// A valid provider response whose content could not be converted into
/// conversation items. The normalized usage remains available to the runner
/// even though the response itself is discarded.
#[derive(Debug, thiserror::Error)]
#[error("{source}")]
pub struct ResponseParseError {
    #[source]
    source: anyhow::Error,
    telemetry_detail: String,
    reported_usage: Option<ReportedUsage>,
}

impl ResponseParseError {
    pub fn new(source: anyhow::Error, reported_usage: Option<ReportedUsage>) -> Self {
        let telemetry_detail = format!("{source:#}");
        Self {
            source,
            telemetry_detail,
            reported_usage,
        }
    }

    /// Construct a parse error whose telemetry detail omits a body preview
    /// that the human-facing `Display` text still carries.
    pub const fn new_with_telemetry_detail(
        source: anyhow::Error,
        reported_usage: Option<ReportedUsage>,
        telemetry_detail: String,
    ) -> Self {
        Self {
            source,
            telemetry_detail,
            reported_usage,
        }
    }

    pub(super) const fn usage(&self) -> Option<ReportedUsage> {
        self.reported_usage
    }

    /// Detail without the response-body preview, safe for telemetry.
    pub fn telemetry_detail(&self) -> &str {
        &self.telemetry_detail
    }
}

/// Native structured-output constraint attached to correction-turn requests
/// when `--output-schema` enforcement needs a corrected final message.
///
/// Local validation remains authoritative; this is best-effort acceleration.
/// It is threaded as its own parameter rather than a `RequestOverrides` field
/// because overrides drive HTTP retry semantics and telemetry snapshots.
#[derive(Debug, Clone, Copy)]
pub(super) struct FinalOutputConstraint<'a> {
    pub(super) name: &'a str,
    pub(super) schema: &'a serde_json::Value,
}

/// The provider-level choice among the tools offered in a request.
///
/// This changes only `tool_choice`, never the tool set, so a constrained run
/// keeps the same tool schemas and prompt prefix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum ToolChoice {
    /// Let the model decide whether to call a tool.
    #[default]
    Auto,
    /// Forbid tool calls for this run.
    None,
    /// Require at least one tool call.
    Required,
}

impl ToolChoice {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::None => "none",
            Self::Required => "required",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) enum Backend {
    Responses,
    ChatCompletions,
}

impl Backend {
    pub(super) const fn from_api_type(api_type: ApiType) -> Self {
        match api_type {
            ApiType::Responses => Self::Responses,
            ApiType::ChatCompletions => Self::ChatCompletions,
        }
    }

    pub(super) fn build_request_json<'a>(
        self,
        config: &ResolvedModelConfig,
        history: &'a [ConversationItem],
        tools: &'a [Tool],
        tool_choice: Option<ToolChoice>,
        overrides: &RequestOverrides,
        constraint: Option<FinalOutputConstraint<'a>>,
    ) -> anyhow::Result<Vec<u8>> {
        match self {
            Self::Responses => responses::build_request_json(
                config,
                history,
                tools,
                tool_choice,
                overrides,
                constraint,
            ),
            Self::ChatCompletions => chat_completions::build_request_json(
                config,
                history,
                tools,
                tool_choice,
                overrides,
                constraint,
            ),
        }
    }

    pub(super) async fn send_request_json(
        self,
        client: &reqwest::Client,
        config: &ResolvedModelConfig,
        session_id: uuid::Uuid,
        request: Vec<u8>,
    ) -> anyhow::Result<reqwest::Response> {
        match self {
            Self::Responses => {
                responses::send_request_json(client, config, session_id, request).await
            },
            Self::ChatCompletions => {
                chat_completions::send_request_json(client, config, session_id, request).await
            },
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "a provider request threads identity, history, tools, choice, overrides, and constraint"
    )]
    pub(super) async fn send_request<'a>(
        self,
        client: &reqwest::Client,
        config: &ResolvedModelConfig,
        session_id: uuid::Uuid,
        history: &'a [ConversationItem],
        tools: &'a [Tool],
        tool_choice: Option<ToolChoice>,
        overrides: &RequestOverrides,
        constraint: Option<FinalOutputConstraint<'a>>,
    ) -> anyhow::Result<reqwest::Response> {
        match self {
            Self::Responses => {
                responses::send_request(
                    client,
                    config,
                    session_id,
                    history,
                    tools,
                    tool_choice,
                    overrides,
                    constraint,
                )
                .await
            },
            Self::ChatCompletions => {
                chat_completions::send_request(
                    client,
                    config,
                    session_id,
                    history,
                    tools,
                    tool_choice,
                    overrides,
                    constraint,
                )
                .await
            },
        }
    }

    pub(super) fn reported_usage(self, body: &[u8]) -> Option<ReportedUsage> {
        match self {
            Self::Responses => responses::reported_usage(body),
            Self::ChatCompletions => chat_completions::reported_usage(body),
        }
    }

    pub(super) async fn parse_response(
        self,
        response: reqwest::Response,
        model: &str,
        stall_window: std::time::Duration,
    ) -> anyhow::Result<TurnResult> {
        match self {
            Self::Responses => responses::parse_response(response, model, stall_window).await,
            Self::ChatCompletions => {
                chat_completions::parse_response(response, model, stall_window).await
            },
        }
    }
}
