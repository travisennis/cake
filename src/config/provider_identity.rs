//! Build and compare the sanitized provider identity pinned to a session.
//!
//! This module is the only producer of [`ProviderIdentity`]. It resolves the
//! credential *kind* without resolving the credential value when the source is
//! an environment variable, and it compares a stored snapshot with the current
//! model configuration before any provider request.

use sha2::{Digest, Sha256};

use crate::config::model::{
    ModelConfig, ModelProvider, ProviderHeaders, canonicalize_base_url, infer_provider,
};
use crate::types::provider_identity::{
    ProviderCredentialSource, ProviderIdentity, ProviderIdentityDifference, ProviderRequestShape,
};

/// A provider identity could not be built from the configured model.
#[derive(Debug, thiserror::Error)]
pub enum ProviderIdentityError {
    #[error("Invalid model configuration '{name}': {reason}")]
    InvalidEndpoint { name: String, reason: String },
    #[error("Cannot resolve the Codex account identity for model '{name}': {source}")]
    CodexAccount { name: String, source: anyhow::Error },
}

/// Outcome of comparing a stored session identity with the current resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionProviderIdentityCheck {
    /// The stored snapshot matches the current configuration.
    Match,
    /// The session predates provider identity pinning.
    UnknownBaseline,
    /// One or more non-secret identity fields changed.
    Changed(Vec<ProviderIdentityDifference>),
}

/// Build the sanitized identity for one configured `[[models]]` entry.
///
/// `config` must be the configured entry before per-invocation CLI overrides;
/// the snapshot is a property of the settings entry, not of a transient flag.
///
/// # Errors
///
/// Returns [`ProviderIdentityError`] when the endpoint is unparsable or embeds
/// userinfo, or when a Codex-backed model's account fingerprint cannot be read.
pub fn build(config: &ModelConfig, model_config: &str) -> anyhow::Result<ProviderIdentity> {
    let base_url = canonicalize_base_url(&config.base_url).map_err(|reason| {
        ProviderIdentityError::InvalidEndpoint {
            name: model_config.to_string(),
            reason,
        }
    })?;
    let credential = credential_source(config, model_config)?;
    Ok(ProviderIdentity {
        model_config: model_config.to_string(),
        model: config.model.clone(),
        base_url,
        api_type: config.api_type.as_str().to_string(),
        provider: resolved_provider(config)
            .map(ModelProvider::as_str)
            .map(str::to_string),
        routing: config.providers.clone(),
        credential,
        request_shape: request_shape(config),
    })
}

/// Compare a stored session identity with the current resolution.
#[must_use]
pub fn check_session_identity(
    stored: Option<&ProviderIdentity>,
    current: &ProviderIdentity,
) -> SessionProviderIdentityCheck {
    stored.map_or(SessionProviderIdentityCheck::UnknownBaseline, |stored| {
        let differences = stored.differences(current);
        if differences.is_empty() {
            SessionProviderIdentityCheck::Match
        } else {
            SessionProviderIdentityCheck::Changed(differences)
        }
    })
}

fn credential_source(
    config: &ModelConfig,
    model_config: &str,
) -> anyhow::Result<ProviderCredentialSource> {
    if crate::auth::is_chatgpt_codex_backend(&config.base_url) {
        let account = crate::auth::codex_account_fingerprint().map_err(|source| {
            ProviderIdentityError::CodexAccount {
                name: model_config.to_string(),
                source,
            }
        })?;
        Ok(ProviderCredentialSource::CodexFile { account })
    } else {
        Ok(ProviderCredentialSource::Env {
            name: config.api_key_env.clone(),
        })
    }
}

fn resolved_provider(config: &ModelConfig) -> Option<ModelProvider> {
    config.provider.or_else(|| infer_provider(&config.base_url))
}

/// Render an `f32` setting in the shortest decimal form that round-trips it,
/// so a snapshot stores `0.7` rather than the `f32` expansion. The comparison
/// uses the same rendering on both sides.
fn f32_value(value: f32) -> f64 {
    value
        .to_string()
        .parse::<f64>()
        .unwrap_or_else(|_| f64::from(value))
}

fn request_shape(config: &ModelConfig) -> ProviderRequestShape {
    ProviderRequestShape {
        temperature: config.temperature.map(f32_value),
        top_p: config.top_p.map(f32_value),
        top_k: config.top_k,
        max_output_tokens: config.max_output_tokens,
        reasoning_effort: config
            .reasoning_effort
            .map(|effort| effort.as_str().to_string()),
        reasoning_summary: config.reasoning_summary.clone(),
        reasoning_max_tokens: config.reasoning_max_tokens,
        provider_headers_digest: config
            .provider_headers
            .as_ref()
            .map(provider_headers_digest),
    }
}

/// One-way digest of the configured provider header values.
///
/// The raw values never leave this function. Missing values hash as empty, so
/// the digest changes when any configured header value changes.
fn provider_headers_digest(headers: &ProviderHeaders) -> String {
    let mut hasher = Sha256::new();
    hasher.update(headers.http_referer.as_deref().unwrap_or("").as_bytes());
    hasher.update(b"\0");
    hasher.update(headers.x_title.as_deref().unwrap_or("").as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
#[path = "provider_identity_tests.rs"]
mod tests;
