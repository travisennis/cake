use std::borrow::Cow;

use crate::types::Role;

use crate::clients::chat_types::ChatMessage;
use crate::clients::responses_types::ProviderConfig;
use crate::config::model::{ModelProvider, ProviderHeaders, ResolvedModelConfig, infer_provider};

const OPENROUTER_REFERER: &str = "https://github.com/travisennis/cake";
const OPENROUTER_TITLE: &str = "cake";
const REASONING_CONTENT_PLACEHOLDER: &str = " ";
const CAKE_USER_AGENT: &str = concat!("cake/", env!("CARGO_PKG_VERSION"));

pub(super) struct ProviderStrategy<'a> {
    config: &'a ResolvedModelConfig,
    provider: Option<ModelProvider>,
}

impl<'a> ProviderStrategy<'a> {
    pub(super) fn from_config(config: &'a ResolvedModelConfig) -> Self {
        Self {
            config,
            provider: config
                .model_config
                .provider
                .or_else(|| infer_provider(&config.model_config.base_url)),
        }
    }

    pub(super) const fn provider(&self) -> Option<ModelProvider> {
        self.provider
    }

    pub(super) fn apply_headers(
        &self,
        request: reqwest::RequestBuilder,
        session_id: uuid::Uuid,
    ) -> reqwest::RequestBuilder {
        let request = request.header(reqwest::header::USER_AGENT, CAKE_USER_AGENT);
        match self.provider {
            Some(provider) => self.apply_provider_headers(request, provider, session_id),
            None => request,
        }
    }

    fn apply_provider_headers(
        &self,
        request: reqwest::RequestBuilder,
        provider: ModelProvider,
        session_id: uuid::Uuid,
    ) -> reqwest::RequestBuilder {
        match provider {
            ModelProvider::OpenRouter => {
                apply_openrouter_headers(request, self.openrouter_headers())
            },
            ModelProvider::OpenCode => request.header("x-opencode-session", session_id.to_string()),
        }
    }

    pub(super) fn responses_tool_strict(&self) -> Option<bool> {
        let base_url = &self.config.model_config.base_url;
        let url = reqwest::Url::parse(base_url).ok()?;
        (url.host_str() == Some("api.openai.com")
            || crate::auth::is_chatgpt_codex_backend(base_url))
        .then_some(false)
    }

    pub(super) fn responses_provider_config(&self) -> Option<ProviderConfig> {
        if self.provider != Some(ModelProvider::OpenRouter) {
            return None;
        }

        provider_routing_config(&self.config.model_config.providers)
    }

    pub(super) fn transform_chat_messages(messages: &mut Vec<ChatMessage<'_>>) {
        demote_developer_to_user(messages);

        for msg in messages.iter_mut() {
            if msg.role == Role::Assistant
                && msg.tool_calls.is_some()
                && msg.reasoning_content.is_none()
            {
                msg.reasoning_content = Some(Cow::Borrowed(REASONING_CONTENT_PLACEHOLDER));
            }
        }
    }

    fn openrouter_headers(&self) -> ProviderHeaders {
        self.config
            .model_config
            .provider_headers
            .clone()
            .unwrap_or_else(default_openrouter_headers)
    }
}

fn default_openrouter_headers() -> ProviderHeaders {
    ProviderHeaders {
        http_referer: Some(OPENROUTER_REFERER.to_string()),
        x_title: Some(OPENROUTER_TITLE.to_string()),
    }
}

fn apply_openrouter_headers(
    mut request: reqwest::RequestBuilder,
    headers: ProviderHeaders,
) -> reqwest::RequestBuilder {
    if let Some(http_referer) = headers.http_referer {
        request = request.header("HTTP-Referer", http_referer);
    }
    if let Some(x_title) = headers.x_title {
        request = request.header("X-Title", x_title);
    }
    request
}

fn provider_routing_config(providers: &[String]) -> Option<ProviderConfig> {
    if providers.is_empty() || (providers.len() == 1 && providers[0] == "all") {
        None
    } else {
        Some(ProviderConfig {
            only: providers.to_vec(),
        })
    }
}

/// Rewrite `developer` role messages to `user` role for providers that don't
/// support the `developer` role in Chat Completions.
///
/// The `developer` role is standard in the `OpenAI` Chat Completions API but not
/// universally supported (e.g. `DeepSeek` via `OpenCode` Zen rejects it). This
/// preserves each context piece as its own message with role `user` rather
/// than concatenating them, keeping context boundaries intact.
fn demote_developer_to_user(messages: &mut Vec<ChatMessage<'_>>) {
    for msg in messages.iter_mut() {
        if msg.role == Role::Developer {
            msg.role = Role::User;
        }
    }
}
