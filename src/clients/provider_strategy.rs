use std::borrow::Cow;

use crate::clients::chat_types::ChatMessage;
use crate::clients::types::ProviderConfig;
use crate::config::model::ResolvedModelConfig;

const OPENROUTER_REFERER: &str = "https://github.com/travisennis/cake";
const OPENROUTER_TITLE: &str = "cake";
const KIMI_REASONING_CONTENT_PLACEHOLDER: &str = " ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderKind {
    Generic,
    OpenRouter,
}

pub(super) struct ProviderStrategy<'a> {
    config: &'a ResolvedModelConfig,
    kind: ProviderKind,
}

impl<'a> ProviderStrategy<'a> {
    pub(super) fn from_config(config: &'a ResolvedModelConfig) -> Self {
        Self {
            config,
            kind: provider_kind(&config.model_config.base_url),
        }
    }

    pub(super) fn apply_headers(
        &self,
        request: reqwest::RequestBuilder,
    ) -> reqwest::RequestBuilder {
        match self.kind {
            ProviderKind::Generic => request,
            ProviderKind::OpenRouter => request
                .header("HTTP-Referer", OPENROUTER_REFERER)
                .header("X-Title", OPENROUTER_TITLE),
        }
    }

    pub(super) fn responses_provider_config(&self) -> Option<ProviderConfig> {
        if self.kind != ProviderKind::OpenRouter {
            return None;
        }

        provider_routing_config(&self.config.model_config.providers)
    }

    pub(super) fn transform_chat_messages(&self, messages: &mut [ChatMessage<'_>]) {
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
}

fn provider_kind(base_url: &str) -> ProviderKind {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return ProviderKind::Generic;
    };

    let Some(host) = url.host_str() else {
        return ProviderKind::Generic;
    };

    if host == "openrouter.ai" || host.ends_with(".openrouter.ai") {
        ProviderKind::OpenRouter
    } else {
        ProviderKind::Generic
    }
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
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
            provider_kind("https://gateway.openrouter.ai/api/v1"),
            ProviderKind::OpenRouter
        );
        assert_eq!(
            provider_kind("https://not-openrouter.example.com/api/v1"),
            ProviderKind::Generic
        );
        assert_eq!(provider_kind("not a url"), ProviderKind::Generic);
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
}
