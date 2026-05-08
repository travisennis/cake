use crate::clients::agent::TurnResult;
use crate::clients::retry::RequestOverrides;
use crate::clients::tools::Tool;
use crate::clients::types::ConversationItem;
use crate::clients::{chat_completions, responses};
use crate::config::model::{ApiType, ResolvedModelConfig};

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

    pub(super) async fn send_request(
        self,
        client: &reqwest::Client,
        config: &ResolvedModelConfig,
        history: &[ConversationItem],
        tools: &[Tool],
        overrides: &RequestOverrides,
    ) -> anyhow::Result<reqwest::Response> {
        match self {
            Self::Responses => {
                responses::send_request(client, config, history, tools, overrides).await
            },
            Self::ChatCompletions => {
                chat_completions::send_request(client, config, history, tools, overrides).await
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
