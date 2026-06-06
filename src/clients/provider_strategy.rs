use std::borrow::Cow;

use crate::clients::chat_types::ChatMessage;
use crate::clients::responses_types::ProviderConfig;
use crate::config::model::{ModelProvider, ProviderHeaders, ResolvedModelConfig};

const OPENROUTER_REFERER: &str = "https://github.com/travisennis/cake";
const OPENROUTER_TITLE: &str = "cake";
const KIMI_REASONING_CONTENT_PLACEHOLDER: &str = " ";

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

    pub(super) fn apply_headers(
        &self,
        request: reqwest::RequestBuilder,
    ) -> reqwest::RequestBuilder {
        match self.provider {
            Some(ModelProvider::OpenRouter) => {
                apply_openrouter_headers(request, self.openrouter_headers())
            },
            None => request,
        }
    }

    pub(super) fn responses_provider_config(&self) -> Option<ProviderConfig> {
        if self.provider != Some(ModelProvider::OpenRouter) {
            return None;
        }

        provider_routing_config(&self.config.model_config.providers)
    }

    pub(super) fn transform_chat_messages(&self, messages: &mut Vec<ChatMessage<'_>>) {
        // Demote developer messages to user role for Chat Completions
        // providers that don't support the `developer` role.
        demote_developer_to_user(messages);

        if !requires_reasoning_content_tool_call_fallback(&self.config.model_config.model) {
            return;
        }

        for msg in messages.iter_mut() {
            if msg.role == "assistant"
                && msg.tool_calls.is_some()
                && msg.reasoning_content.is_none()
            {
                msg.reasoning_content = Some(Cow::Borrowed(KIMI_REASONING_CONTENT_PLACEHOLDER));
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

fn infer_provider(base_url: &str) -> Option<ModelProvider> {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return None;
    };

    let host = url.host_str()?;

    (host == "openrouter.ai" || host.ends_with(".openrouter.ai"))
        .then_some(ModelProvider::OpenRouter)
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

fn requires_reasoning_content_tool_call_fallback(model: &str) -> bool {
    model.to_ascii_lowercase().contains("kimi")
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
        if msg.role == "developer" {
            msg.role = Cow::Borrowed("user");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::chat_types::{ChatFunctionCallRef, ChatToolCallRef};
    use crate::config::model::{ApiType, ModelConfig};

    fn test_config(
        base_url: &str,
        model: &str,
        providers: impl IntoIterator<Item = &'static str>,
    ) -> ResolvedModelConfig {
        ResolvedModelConfig {
            model_config: ModelConfig {
                model: model.to_string(),
                api_type: ApiType::ChatCompletions,
                base_url: base_url.to_string(),
                api_key_env: "TEST_API_KEY".to_string(),
                provider: None,
                provider_headers: None,
                temperature: None,
                top_p: None,
                max_output_tokens: None,
                reasoning_effort: None,
                reasoning_summary: None,
                reasoning_max_tokens: None,
                providers: providers.into_iter().map(str::to_string).collect(),
            },
            api_key: "test-key".to_string(),
        }
    }

    fn assistant_tool_call_message<'a>() -> ChatMessage<'a> {
        ChatMessage {
            role: Cow::Borrowed("assistant"),
            content: None,
            reasoning_content: None,
            tool_calls: Some(vec![ChatToolCallRef {
                id: Cow::Borrowed("call-1"),
                type_: Cow::Borrowed("function"),
                function: ChatFunctionCallRef {
                    name: Cow::Borrowed("bash"),
                    arguments: Cow::Borrowed(r#"{"cmd":"ls"}"#),
                },
            }]),
            tool_call_id: None,
        }
    }

    #[test]
    fn openrouter_headers_apply_only_to_openrouter_urls() {
        let client = reqwest::Client::new();
        let generic_config = test_config("https://api.example.com/v1", "openai/gpt-4.1", []);
        let generic_request = ProviderStrategy::from_config(&generic_config)
            .apply_headers(client.post("https://api.example.com/v1/chat/completions"))
            .build()
            .unwrap();
        assert!(generic_request.headers().get("HTTP-Referer").is_none());
        assert!(generic_request.headers().get("X-Title").is_none());

        let openrouter_config = test_config("https://openrouter.ai/api/v1", "openai/gpt-4.1", []);
        let openrouter_request = ProviderStrategy::from_config(&openrouter_config)
            .apply_headers(client.post("https://openrouter.ai/api/v1/chat/completions"))
            .build()
            .unwrap();
        assert_eq!(
            openrouter_request
                .headers()
                .get("HTTP-Referer")
                .and_then(|value| value.to_str().ok()),
            Some(OPENROUTER_REFERER)
        );
        assert_eq!(
            openrouter_request
                .headers()
                .get("X-Title")
                .and_then(|value| value.to_str().ok()),
            Some(OPENROUTER_TITLE)
        );
    }

    #[test]
    fn openrouter_detection_accepts_subdomains() {
        assert_eq!(
            infer_provider("https://gateway.openrouter.ai/api/v1"),
            Some(ModelProvider::OpenRouter)
        );
        assert_eq!(
            infer_provider("https://not-openrouter.example.com/api/v1"),
            None
        );
        assert_eq!(infer_provider("not a url"), None);
    }

    #[test]
    fn explicit_openrouter_provider_applies_configured_headers() {
        let client = reqwest::Client::new();
        let mut config = test_config("https://api.example.com/v1", "openai/gpt-4.1", []);
        config.model_config.provider = Some(ModelProvider::OpenRouter);
        config.model_config.provider_headers = Some(ProviderHeaders {
            http_referer: Some("https://example.com/cake".to_string()),
            x_title: Some("custom-cake".to_string()),
        });

        let request = ProviderStrategy::from_config(&config)
            .apply_headers(client.post("https://api.example.com/v1/chat/completions"))
            .build()
            .unwrap();

        assert_eq!(
            request
                .headers()
                .get("HTTP-Referer")
                .and_then(|value| value.to_str().ok()),
            Some("https://example.com/cake")
        );
        assert_eq!(
            request
                .headers()
                .get("X-Title")
                .and_then(|value| value.to_str().ok()),
            Some("custom-cake")
        );
    }

    #[test]
    fn configured_empty_openrouter_headers_disable_default_headers() {
        let client = reqwest::Client::new();
        let mut config = test_config("https://openrouter.ai/api/v1", "openai/gpt-4.1", []);
        config.model_config.provider_headers = Some(ProviderHeaders::default());

        let request = ProviderStrategy::from_config(&config)
            .apply_headers(client.post("https://openrouter.ai/api/v1/chat/completions"))
            .build()
            .unwrap();

        assert!(request.headers().get("HTTP-Referer").is_none());
        assert!(request.headers().get("X-Title").is_none());
    }

    #[test]
    fn provider_routing_applies_only_to_openrouter_with_specific_providers() {
        let generic_config = test_config(
            "https://api.example.com/v1",
            "openai/gpt-4.1",
            ["anthropic"],
        );
        assert!(
            ProviderStrategy::from_config(&generic_config)
                .responses_provider_config()
                .is_none()
        );

        let openrouter_all_config =
            test_config("https://openrouter.ai/api/v1", "openai/gpt-4.1", ["all"]);
        assert!(
            ProviderStrategy::from_config(&openrouter_all_config)
                .responses_provider_config()
                .is_none()
        );

        let openrouter_config = test_config(
            "https://openrouter.ai/api/v1",
            "openai/gpt-4.1",
            ["anthropic"],
        );
        let provider = ProviderStrategy::from_config(&openrouter_config)
            .responses_provider_config()
            .unwrap();
        assert_eq!(
            serde_json::to_value(provider).unwrap(),
            serde_json::json!({ "only": ["anthropic"] })
        );
    }

    #[test]
    fn kimi_strategy_injects_reasoning_placeholder_for_tool_calls() {
        let config = test_config("https://api.example.com/v1", "moonshot/kimi-k2.6", []);
        let mut messages = vec![assistant_tool_call_message()];

        ProviderStrategy::from_config(&config).transform_chat_messages(&mut messages);

        assert_eq!(messages[0].reasoning_content.as_deref(), Some(" "));
    }

    #[test]
    fn chat_transform_skips_non_kimi_models_and_preserves_existing_reasoning() {
        let generic_config = test_config("https://api.example.com/v1", "openai/gpt-4.1", []);
        let mut generic_messages = vec![assistant_tool_call_message()];
        ProviderStrategy::from_config(&generic_config)
            .transform_chat_messages(&mut generic_messages);
        assert!(generic_messages[0].reasoning_content.is_none());

        let kimi_config = test_config("https://api.example.com/v1", "moonshot/kimi-k2.6", []);
        let mut kimi_messages = vec![assistant_tool_call_message()];
        kimi_messages[0].reasoning_content = Some(Cow::Borrowed("actual reasoning"));
        ProviderStrategy::from_config(&kimi_config).transform_chat_messages(&mut kimi_messages);
        assert_eq!(
            kimi_messages[0].reasoning_content.as_deref(),
            Some("actual reasoning")
        );
    }

    #[test]
    fn demote_developer_changes_role_to_user() {
        let mut messages = vec![
            ChatMessage {
                role: Cow::Borrowed("system"),
                content: Some(Cow::Borrowed("system")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: Cow::Borrowed("developer"),
                content: Some(Cow::Borrowed("AGENTS.md context")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: Cow::Borrowed("developer"),
                content: Some(Cow::Borrowed("Environment context")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: Cow::Borrowed("user"),
                content: Some(Cow::Borrowed("Hello")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        demote_developer_to_user(&mut messages);

        // Each developer message keeps its content, just becomes "user" role
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[1].content.as_deref(), Some("AGENTS.md context"));
        assert_eq!(messages[2].role, "user");
        assert_eq!(messages[2].content.as_deref(), Some("Environment context"));
        assert_eq!(messages[3].role, "user");
        assert_eq!(messages[3].content.as_deref(), Some("Hello"));
    }

    #[test]
    fn demote_developer_works_without_preceding_user_message() {
        let mut messages = vec![
            ChatMessage {
                role: Cow::Borrowed("developer"),
                content: Some(Cow::Borrowed("context")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: Cow::Borrowed("assistant"),
                content: Some(Cow::Borrowed("response")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        demote_developer_to_user(&mut messages);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content.as_deref(), Some("context"));
    }

    #[test]
    fn demote_developer_no_developer_messages_is_noop() {
        let mut messages = vec![
            ChatMessage {
                role: Cow::Borrowed("system"),
                content: Some(Cow::Borrowed("system")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: Cow::Borrowed("user"),
                content: Some(Cow::Borrowed("Hello")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        demote_developer_to_user(&mut messages);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
    }
}
