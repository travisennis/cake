use crate::clients::agent::TurnResult;
use crate::clients::retry::RequestOverrides;
use crate::clients::tools::Tool;
use crate::clients::{chat_completions, responses};
use crate::config::model::{ApiType, ResolvedModelConfig};
use crate::types::ConversationItem;

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

    pub(super) async fn send_request<'a>(
        self,
        client: &reqwest::Client,
        config: &ResolvedModelConfig,
        history: &'a [ConversationItem],
        tools: &'a [Tool],
        overrides: &RequestOverrides,
        constraint: Option<FinalOutputConstraint<'a>>,
    ) -> anyhow::Result<reqwest::Response> {
        match self {
            Self::Responses => {
                responses::send_request(client, config, history, tools, overrides, constraint).await
            },
            Self::ChatCompletions => {
                chat_completions::send_request(
                    client, config, history, tools, overrides, constraint,
                )
                .await
            },
        }
    }

    /// Build the exact provider-transformed JSON body for a request.
    pub(super) fn build_request_json<'a>(
        self,
        config: &ResolvedModelConfig,
        history: &'a [ConversationItem],
        tools: &'a [Tool],
        overrides: &RequestOverrides,
        constraint: Option<FinalOutputConstraint<'a>>,
    ) -> anyhow::Result<Vec<u8>> {
        match self {
            Self::Responses => {
                responses::build_request_json(config, history, tools, overrides, constraint)
            },
            Self::ChatCompletions => {
                chat_completions::build_request_json(config, history, tools, overrides, constraint)
            },
        }
    }

    /// Send one request body previously returned by [`Self::build_request_json`].
    pub(super) async fn send_request_json(
        self,
        client: &reqwest::Client,
        config: &ResolvedModelConfig,
        request: Vec<u8>,
    ) -> anyhow::Result<reqwest::Response> {
        match self {
            Self::Responses => responses::send_request_json(client, config, request).await,
            Self::ChatCompletions => {
                chat_completions::send_request_json(client, config, request).await
            },
        }
    }

    pub(super) async fn parse_response(
        self,
        response: reqwest::Response,
    ) -> anyhow::Result<TurnResult> {
        match self {
            Self::Responses => responses::parse_response(response).await,
            Self::ChatCompletions => chat_completions::parse_response(response).await,
        }
    }
}
