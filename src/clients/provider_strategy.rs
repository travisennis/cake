use std::borrow::Cow;

use crate::types::Role;

use crate::clients::chat_types::ChatMessage;
use crate::clients::responses_types::ProviderConfig;
use crate::config::model::{ModelProvider, ProviderHeaders, ResolvedModelConfig};

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

    pub(super) fn responses_provider_config(&self) -> Option<ProviderConfig> {
        if self.provider != Some(ModelProvider::OpenRouter) {
            return None;
        }

        provider_routing_config(&self.config.model_config.providers)
    }

    pub(super) fn transform_chat_messages(messages: &mut Vec<ChatMessage<'_>>) {
        // Demote developer messages to user role for Chat Completions
        // providers that don't support the `developer` role.
        demote_developer_to_user(messages);

        // Backfill a placeholder into assistant tool-call messages that lack
        // reasoning content. Reasoning models (Kimi, DeepSeek, and others) in
        // thinking mode require the field to round-trip, and provider-side
        // routing can open the gap mid-session without cake's knowledge. The
        // placeholder is a no-op for providers that ignore the field.
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

/// (domain, provider) pairs for base-URL inference when `provider` is unset.
/// A table keeps inference branch-free as providers are added.
const INFERRED_PROVIDERS: &[(&str, ModelProvider)] = &[
    ("openrouter.ai", ModelProvider::OpenRouter),
    ("opencode.ai", ModelProvider::OpenCode),
];

fn infer_provider(base_url: &str) -> Option<ModelProvider> {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return None;
    };

    let host = url.host_str()?;

    INFERRED_PROVIDERS
        .iter()
        .find(|(domain, _)| host_matches_domain(host, domain))
        .map(|(_, provider)| *provider)
}

/// Whether `host` is `domain` itself or a dot-subdomain of it.
fn host_matches_domain(host: &str, domain: &str) -> bool {
    host == domain
        || host
            .strip_suffix(domain)
            .is_some_and(|rest| rest.ends_with('.'))
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
                context_window: None,
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
            role: Role::Assistant,
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
            .apply_headers(
                client.post("https://api.example.com/v1/chat/completions"),
                uuid::Uuid::nil(),
            )
            .build()
            .unwrap();
        assert!(generic_request.headers().get("HTTP-Referer").is_none());
        assert!(generic_request.headers().get("X-Title").is_none());

        let openrouter_config = test_config("https://openrouter.ai/api/v1", "openai/gpt-4.1", []);
        let openrouter_request = ProviderStrategy::from_config(&openrouter_config)
            .apply_headers(
                client.post("https://openrouter.ai/api/v1/chat/completions"),
                uuid::Uuid::nil(),
            )
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
    fn user_agent_applies_to_all_providers() {
        let client = reqwest::Client::new();
        let expected = format!("cake/{}", env!("CARGO_PKG_VERSION"));

        let generic_config = test_config("https://api.example.com/v1", "openai/gpt-4.1", []);
        let generic_request = ProviderStrategy::from_config(&generic_config)
            .apply_headers(client.post("https://api.example.com/v1/chat/completions"))
            .build()
            .unwrap();
        assert_eq!(
            generic_request
                .headers()
                .get(reqwest::header::USER_AGENT)
                .and_then(|value| value.to_str().ok()),
            Some(expected.as_str())
        );

        let openrouter_config = test_config("https://openrouter.ai/api/v1", "openai/gpt-4.1", []);
        let openrouter_request = ProviderStrategy::from_config(&openrouter_config)
            .apply_headers(client.post("https://openrouter.ai/api/v1/chat/completions"))
            .build()
            .unwrap();
        assert_eq!(
            openrouter_request
                .headers()
                .get(reqwest::header::USER_AGENT)
                .and_then(|value| value.to_str().ok()),
            Some(expected.as_str())
        );
        // OpenRouter attribution headers ride alongside the user agent.
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
    fn opencode_detection_prefers_explicit_provider_over_url() {
        let mut config = test_config("https://api.example.com/v1", "glm-5.1", []);
        config.model_config.provider = Some(ModelProvider::OpenCode);
        assert_eq!(
            ProviderStrategy::from_config(&config).provider(),
            Some(ModelProvider::OpenCode)
        );

        assert_eq!(
            infer_provider("https://opencode.ai/zen"),
            Some(ModelProvider::OpenCode)
        );
        assert_eq!(
            infer_provider("https://opencode.ai/zen/go/v1/"),
            Some(ModelProvider::OpenCode)
        );
        assert_eq!(
            infer_provider("https://gateway.opencode.ai/api/v1"),
            Some(ModelProvider::OpenCode)
        );
        assert_eq!(
            infer_provider("https://not-opencode.example.com/api/v1"),
            None
        );
        assert_eq!(infer_provider("not a url"), None);
    }

    #[test]
    fn explicit_provider_wins_over_url_inference() {
        let client = reqwest::Client::new();
        let session_id = uuid::Uuid::new_v4();

        // Explicit OpenRouter on an OpenCode URL stays OpenRouter: attribution
        // headers apply, no session header.
        let mut config = test_config("https://opencode.ai/zen", "openai/gpt-4.1", []);
        config.model_config.provider = Some(ModelProvider::OpenRouter);
        assert_eq!(
            ProviderStrategy::from_config(&config).provider(),
            Some(ModelProvider::OpenRouter)
        );
        let request = ProviderStrategy::from_config(&config)
            .apply_headers(
                client.post("https://opencode.ai/zen/chat/completions"),
                session_id,
            )
            .build()
            .unwrap();
        assert!(request.headers().get("HTTP-Referer").is_some());
        assert!(request.headers().get("x-opencode-session").is_none());

        // Explicit OpenCode on an OpenRouter URL stays OpenCode: session
        // header applies, no attribution headers.
        let mut config = test_config("https://openrouter.ai/api/v1", "glm-5.1", []);
        config.model_config.provider = Some(ModelProvider::OpenCode);
        assert_eq!(
            ProviderStrategy::from_config(&config).provider(),
            Some(ModelProvider::OpenCode)
        );
        let request = ProviderStrategy::from_config(&config)
            .apply_headers(
                client.post("https://openrouter.ai/api/v1/chat/completions"),
                session_id,
            )
            .build()
            .unwrap();
        assert_eq!(
            request
                .headers()
                .get("x-opencode-session")
                .and_then(|value| value.to_str().ok()),
            Some(session_id.to_string()).as_deref()
        );
        assert!(request.headers().get("HTTP-Referer").is_none());
        assert!(request.headers().get("X-Title").is_none());
    }

    #[test]
    fn opencode_header_carries_session_uuid() {
        let client = reqwest::Client::new();
        let session_id = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();

        let mut explicit = test_config("https://api.example.com/v1", "glm-5.1", []);
        explicit.model_config.provider = Some(ModelProvider::OpenCode);
        let request = ProviderStrategy::from_config(&explicit)
            .apply_headers(
                client.post("https://api.example.com/v1/chat/completions"),
                session_id,
            )
            .build()
            .unwrap();
        assert_eq!(
            request
                .headers()
                .get("x-opencode-session")
                .and_then(|value| value.to_str().ok()),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
        assert!(request.headers().get("HTTP-Referer").is_none());
        assert!(request.headers().get("X-Title").is_none());

        let inferred = test_config("https://opencode.ai/zen/go/v1/", "glm-5.1", []);
        let request = ProviderStrategy::from_config(&inferred)
            .apply_headers(
                client.post("https://opencode.ai/zen/go/v1/chat/completions"),
                session_id,
            )
            .build()
            .unwrap();
        assert_eq!(
            request
                .headers()
                .get("x-opencode-session")
                .and_then(|value| value.to_str().ok()),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
    }

    #[test]
    fn generic_provider_sends_no_opencode_session_header() {
        let client = reqwest::Client::new();
        let session_id = uuid::Uuid::new_v4();
        let generic = test_config("https://api.example.com/v1", "openai/gpt-4.1", []);
        let request = ProviderStrategy::from_config(&generic)
            .apply_headers(
                client.post("https://api.example.com/v1/chat/completions"),
                session_id,
            )
            .build()
            .unwrap();
        assert!(request.headers().get("x-opencode-session").is_none());
    }

    #[tokio::test]
    async fn opencode_session_header_reaches_both_backends() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let session_id = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let expected = "550e8400-e29b-41d4-a716-446655440000";

        for backend in [
            crate::clients::backend::Backend::ChatCompletions,
            crate::clients::backend::Backend::Responses,
        ] {
            let mock_server = MockServer::start().await;
            let endpoint = match backend {
                crate::clients::backend::Backend::ChatCompletions => "/chat/completions",
                crate::clients::backend::Backend::Responses => "/responses",
            };
            Mock::given(method("POST"))
                .and(path(endpoint))
                .and(header("x-opencode-session", expected))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
                .expect(1)
                .mount(&mock_server)
                .await;

            let mut config = test_config(mock_server.uri().as_str(), "glm-5.1", []);
            config.model_config.provider = Some(ModelProvider::OpenCode);
            let client = reqwest::Client::new();
            let body = serde_json::json!({"model": "glm-5.1"})
                .to_string()
                .into_bytes();
            let response = backend
                .send_request_json(&client, &config, session_id, body)
                .await
                .expect("mocked backend send should succeed");
            assert!(response.status().is_success());
            mock_server.verify().await;
        }
    }

    #[tokio::test]
    async fn generic_backend_sends_no_opencode_session_header() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&mock_server)
            .await;

        let config = test_config(mock_server.uri().as_str(), "openai/gpt-4.1", []);
        let client = reqwest::Client::new();
        let body = serde_json::json!({"model": "openai/gpt-4.1"})
            .to_string()
            .into_bytes();
        crate::clients::backend::Backend::ChatCompletions
            .send_request_json(&client, &config, uuid::Uuid::new_v4(), body)
            .await
            .expect("mocked backend send should succeed");

        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].headers.get("x-opencode-session").is_none());
    }

    #[test]
    fn openrouter_provider_sends_no_opencode_session_header() {
        let client = reqwest::Client::new();
        let config = test_config("https://openrouter.ai/api/v1", "openai/gpt-4.1", []);
        let request = ProviderStrategy::from_config(&config)
            .apply_headers(
                client.post("https://openrouter.ai/api/v1/chat/completions"),
                uuid::Uuid::new_v4(),
            )
            .build()
            .unwrap();
        assert!(request.headers().get("x-opencode-session").is_none());
        assert!(request.headers().get("HTTP-Referer").is_some());
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
            .apply_headers(
                client.post("https://api.example.com/v1/chat/completions"),
                uuid::Uuid::nil(),
            )
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
            .apply_headers(
                client.post("https://openrouter.ai/api/v1/chat/completions"),
                uuid::Uuid::nil(),
            )
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
    fn transform_injects_reasoning_placeholder_for_tool_call_messages() {
        let mut messages = vec![assistant_tool_call_message()];

        ProviderStrategy::transform_chat_messages(&mut messages);

        assert_eq!(messages[0].reasoning_content.as_deref(), Some(" "));
    }

    #[test]
    fn transform_preserves_existing_reasoning_content() {
        let mut messages = vec![assistant_tool_call_message()];
        messages[0].reasoning_content = Some(Cow::Borrowed("actual reasoning"));

        ProviderStrategy::transform_chat_messages(&mut messages);

        assert_eq!(
            messages[0].reasoning_content.as_deref(),
            Some("actual reasoning")
        );
    }

    #[test]
    fn demote_developer_changes_role_to_user() {
        let mut messages = vec![
            ChatMessage {
                role: Role::System,
                content: Some(Cow::Borrowed("system")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: Role::Developer,
                content: Some(Cow::Borrowed("AGENTS.md context")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: Role::Developer,
                content: Some(Cow::Borrowed("Environment context")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: Role::User,
                content: Some(Cow::Borrowed("Hello")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        demote_developer_to_user(&mut messages);

        // Each developer message keeps its content, just becomes "user" role
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, Role::System);
        assert_eq!(messages[1].role, Role::User);
        assert_eq!(messages[1].content.as_deref(), Some("AGENTS.md context"));
        assert_eq!(messages[2].role, Role::User);
        assert_eq!(messages[2].content.as_deref(), Some("Environment context"));
        assert_eq!(messages[3].role, Role::User);
        assert_eq!(messages[3].content.as_deref(), Some("Hello"));
    }

    #[test]
    fn demote_developer_works_without_preceding_user_message() {
        let mut messages = vec![
            ChatMessage {
                role: Role::Developer,
                content: Some(Cow::Borrowed("context")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: Role::Assistant,
                content: Some(Cow::Borrowed("response")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        demote_developer_to_user(&mut messages);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[0].content.as_deref(), Some("context"));
    }

    #[test]
    fn demote_developer_no_developer_messages_is_noop() {
        let mut messages = vec![
            ChatMessage {
                role: Role::System,
                content: Some(Cow::Borrowed("system")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
            },
            ChatMessage {
                role: Role::User,
                content: Some(Cow::Borrowed("Hello")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        demote_developer_to_user(&mut messages);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::System);
        assert_eq!(messages[1].role, Role::User);
    }
}
