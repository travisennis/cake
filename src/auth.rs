//! Authentication material shared by Cake's provider request paths.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use base64::Engine;
use reqwest::RequestBuilder;
use serde::Deserialize;

use crate::config::ResolvedModelConfig;

#[derive(Debug, Deserialize)]
struct AuthFile {
    tokens: Option<AuthTokens>,
}

#[derive(Debug, Deserialize)]
struct AuthTokens {
    access_token: String,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

/// Credentials loaded from the Codex CLI's existing local auth store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatGptAuth {
    pub access_token: String,
    pub account_id: String,
}

/// Applies the correct bearer credential for a provider request.
pub fn apply_request_auth(
    request: RequestBuilder,
    config: &ResolvedModelConfig,
) -> anyhow::Result<RequestBuilder> {
    if !is_chatgpt_codex_backend(&config.model_config.base_url) {
        return Ok(request.bearer_auth(&config.api_key));
    }

    let auth = ChatGptAuth::load()?;
    Ok(request
        .bearer_auth(auth.access_token)
        .header("ChatGPT-Account-ID", auth.account_id)
        .header("originator", "cake_cli_rs"))
}

impl ChatGptAuth {
    /// Loads the current `ChatGPT` access token from `CODEX_HOME/auth.json` or `~/.codex/auth.json`.
    pub(crate) fn load() -> anyhow::Result<Self> {
        Self::load_from_path(&codex_auth_path()?)
    }

    fn load_from_path(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read Codex auth file {}", path.display()))?;
        let auth: AuthFile = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse Codex auth file {}", path.display()))?;
        let tokens = auth
            .tokens
            .context("Codex auth file does not contain ChatGPT tokens")?;
        let access_token = non_empty(tokens.access_token, "access token")?;
        let account_id = tokens
            .account_id
            .filter(|value| !value.trim().is_empty())
            .or_else(|| tokens.id_token.as_deref().and_then(account_id_from_jwt))
            .context("Codex auth file does not contain a ChatGPT account ID")?;

        Ok(Self {
            access_token,
            account_id,
        })
    }
}

/// Returns whether requests for this base URL should use the Codex subscription credential.
pub fn is_chatgpt_codex_backend(base_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str() == Some("chatgpt.com")
        && url.path().trim_end_matches('/') == "/backend-api/codex"
}

fn codex_auth_path() -> anyhow::Result<PathBuf> {
    if let Ok(codex_home) = std::env::var("CODEX_HOME")
        && !codex_home.trim().is_empty()
    {
        return Ok(PathBuf::from(codex_home).join("auth.json"));
    }

    dirs::home_dir()
        .map(|home| home.join(".codex/auth.json"))
        .context("could not determine the home directory for Codex auth")
}

fn non_empty(value: String, label: &str) -> anyhow::Result<String> {
    if value.trim().is_empty() {
        bail!("Codex auth file contains an empty {label}");
    }
    Ok(value)
}

fn account_id_from_jwt(jwt: &str) -> Option<String> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    claims
        .get("https://api.openai.com/auth")?
        .get("chatgpt_account_id")?
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
