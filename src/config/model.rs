use std::fmt;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Provider-specific strategy selected for a model configuration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ModelProvider {
    /// OpenRouter-compatible endpoint behavior.
    #[serde(rename = "openrouter")]
    OpenRouter,
    /// `OpenCode` Zen endpoint behavior (`x-opencode-session` header).
    #[serde(rename = "opencode")]
    OpenCode,
}

impl ModelProvider {
    /// Wire vocabulary shared by settings, identity snapshots, and telemetry.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenRouter => "openrouter",
            Self::OpenCode => "opencode",
        }
    }
}

/// Structured provider-specific HTTP headers.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderHeaders {
    /// Value for `OpenRouter`'s `HTTP-Referer` attribution header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_referer: Option<String>,
    /// Value for `OpenRouter`'s `X-Title` attribution header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_title: Option<String>,
}

/// The type of API endpoint to use for model completions.
///
/// Cake supports multiple API backends for interacting with AI providers:
///
/// - `Responses`: `OpenRouter`'s Responses API format, which supports reasoning traces
///   and structured outputs. Use this for providers that support the Responses API.
///
/// - `ChatCompletions`: The standard OpenAI-compatible Chat Completions format, which
///   is widely supported by most AI providers. Use this for maximum compatibility.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApiType {
    /// OpenAI-compatible Chat Completions API - widely supported by most providers
    #[default]
    ChatCompletions,
    /// `OpenRouter` Responses API - supports reasoning traces and structured outputs
    Responses,
}

impl ApiType {
    /// Wire vocabulary shared by settings, identity snapshots, and telemetry.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat_completions",
            Self::Responses => "responses",
        }
    }
}

/// Reasoning effort level requested for models that support configurable reasoning.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    /// Disable configurable reasoning effort where the provider supports it.
    None,
    /// Use a low reasoning effort.
    Low,
    /// Use a medium reasoning effort.
    Medium,
    /// Use a high reasoning effort.
    High,
    /// Use extra-high reasoning effort.
    Xhigh,
    /// Use the maximum reasoning effort the provider exposes.
    Max,
}

impl ReasoningEffort {
    /// Wire vocabulary shared by identity snapshots and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// Configuration for a model provider.
///
/// Contains all settings needed to connect to an AI model API, including
/// the model identifier, API endpoint, authentication, and generation parameters.
///
/// `ModelConfig` has no `Default` impl — it must be constructed from a
/// [`ModelDefinition`](crate::config::ModelDefinition) loaded from `settings.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Model identifier (e.g. "openai/gpt-4o")
    pub model: String,
    /// Which API format to use
    pub api_type: ApiType,
    /// Base URL for the API endpoint
    pub base_url: String,
    /// Name of the environment variable containing the API key
    pub api_key_env: String,
    /// Provider strategy to use. If unset, strategies may infer from `base_url`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<ModelProvider>,
    /// Provider-specific request headers. If unset, provider defaults may apply.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_headers: Option<ProviderHeaders>,
    /// Sampling temperature
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Nucleus sampling parameter
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Top-k sampling parameter
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Maximum number of output tokens
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Model context window in input tokens. When set, the agent can compare
    /// accumulated usage against the window and report the remaining budget.
    /// Absent means the window is unknown and cake keeps current behavior.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    /// Reasoning effort level (none, low, medium, high, xhigh, max)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Reasoning summary mode (concise, detailed, auto) - Responses API only
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_summary: Option<String>,
    /// Maximum reasoning tokens budget (for budget-style reasoning)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_max_tokens: Option<u32>,
    /// Provider routing hints
    pub providers: Vec<String>,
    /// Whether this model can accept image input, which gates the `ReadImage`
    /// tool. Defaults to false: a model that has not declared image support
    /// never sees `ReadImage` in its tool list or request, so cake cannot send
    /// pixels to a text-only model.
    pub supports_images: bool,
}

/// A `ModelConfig` with the API key resolved from the environment.
///
/// This struct is created by calling [`ResolvedModelConfig::resolve`] and contains
/// the actual API key value needed to make authenticated requests.
///
/// # Examples
///
/// Create a [`ModelConfig`], call [`ResolvedModelConfig::resolve`] to obtain
/// the resolved configuration with the API key read from the environment.
#[derive(Clone)]
pub struct ResolvedModelConfig {
    /// The underlying model configuration
    pub model_config: ModelConfig,
    /// The resolved API key value
    pub api_key: String,
}

impl fmt::Debug for ResolvedModelConfig {
    /// Formats the struct without ever emitting the API key (or a substring of it).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedModelConfig")
            .field("model_config", &self.model_config)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

impl ResolvedModelConfig {
    /// Resolves a `ModelConfig` by reading the API key from the environment.
    ///
    /// Returns a `ResolvedModelConfig` with the provider credential value.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let config = ModelConfig { model: "test/model".into(), api_key_env: "MY_KEY".into(), .. };
    /// let resolved = ResolvedModelConfig::resolve(config)?;
    /// println!("Using API key from: {}", resolved.model_config.api_key_env);
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if the configured credential is unavailable or empty.
    pub fn resolve(config: ModelConfig) -> anyhow::Result<Self> {
        let api_key = resolve_env_api_key(&config)?;
        Ok(Self {
            model_config: config,
            api_key,
        })
    }
}

/// Reads the credential from the configured environment variable.
fn resolve_env_api_key(config: &ModelConfig) -> anyhow::Result<String> {
    let api_key = std::env::var(&config.api_key_env).map_err(|err| {
        anyhow::anyhow!(
            "Environment variable '{}' is not set. Please set it to your API key: {err}",
            config.api_key_env
        )
    })?;

    anyhow::ensure!(
        !api_key.is_empty(),
        "Environment variable '{}' is set but empty",
        config.api_key_env
    );
    Ok(api_key)
}

/// Canonical endpoint identity plus a one-way digest of its query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseUrlIdentity {
    /// Credential-free endpoint with query, fragment, and userinfo removed.
    pub base_url: String,
    /// SHA-256 of the exact configured query, when it is non-empty.
    pub query_digest: Option<String>,
}

/// Canonicalize a configured provider endpoint for identity comparison.
///
/// The returned URL has any query, fragment, and userinfo removed and keeps
/// its host and path as destination identity. Use [`base_url_identity`] when
/// the query must also be distinguishable without persisting its raw value.
/// A query or userinfo component can carry a credential, and a snapshot must
/// persist neither, so userinfo is rejected rather than silently stripped;
/// move such a credential to `api_key_env`. The error never echoes the
/// configured value.
pub fn canonicalize_base_url(raw: &str) -> Result<String, String> {
    Ok(base_url_identity(raw)?.base_url)
}

/// Whether `raw` targets the retired `ChatGPT` Codex subscription backend.
///
/// Settings reject this endpoint so an old entry cannot fall back to
/// environment-variable auth or send a normal bearer credential to a
/// non-compatible path.
pub fn is_retired_codex_backend(raw: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(raw) else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str() == Some("chatgpt.com")
        && url.path().trim_end_matches('/') == "/backend-api/codex"
}

/// Parse a configured endpoint into its canonical identity and query digest.
///
/// The returned `base_url` has the query, fragment, and userinfo removed, so
/// it never persists a raw query that could carry a credential or routing
/// parameter. `query_digest` is a one-way SHA-256 of the exact query when one
/// is present, which keeps query-only destination drift detectable.
///
/// # Errors
///
/// Returns an error when the URL does not parse or embeds userinfo.
pub fn base_url_identity(raw: &str) -> Result<BaseUrlIdentity, String> {
    let mut url = reqwest::Url::parse(raw)
        .map_err(|error| format!("base_url is not a valid URL: {error}"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(
            "base_url must not embed userinfo credentials; use api_key_env instead".to_string(),
        );
    }
    let query_digest = url.query().filter(|query| !query.is_empty()).map(|query| {
        let mut hasher = Sha256::new();
        hasher.update(b"cake-provider-identity-query\0");
        hasher.update(query.as_bytes());
        hex::encode(hasher.finalize())
    });
    url.set_query(None);
    url.set_fragment(None);
    Ok(BaseUrlIdentity {
        base_url: url.to_string(),
        query_digest,
    })
}

/// (domain, provider) pairs for base-URL inference when `provider` is unset.
/// A table keeps inference branch-free as providers are added.
const INFERRED_PROVIDERS: &[(&str, ModelProvider)] = &[
    ("openrouter.ai", ModelProvider::OpenRouter),
    ("opencode.ai", ModelProvider::OpenCode),
];

/// Infer the provider strategy from a configured base URL.
#[must_use]
pub fn infer_provider(base_url: &str) -> Option<ModelProvider> {
    let url = reqwest::Url::parse(base_url).ok()?;
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
