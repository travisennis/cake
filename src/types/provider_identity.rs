//! Sanitized provider identity pinned to a persisted session.
//!
//! The snapshot distinguishes a credential destination without storing a
//! credential. It records the configured `[[models]]` entry, endpoint, wire
//! format, provider strategy, routing hints, credential kind, and the
//! request-shape controls that affect cache and wire behavior. A non-empty
//! endpoint query is recorded only as a one-way digest. Credential values,
//! bearer tokens, authorization headers, raw provider-header values, and raw
//! queries never appear here.

use std::fmt::{self, Display};

use serde::{Deserialize, Serialize};

/// Where the provider credential for a session came from.
///
/// An environment-backed credential is identified by its variable name, so
/// rotating the value under the same name does not change the identity. A
/// file-backed Codex credential is identified by a one-way digest of its
/// account ID, so changing the subscription account does.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderCredentialSource {
    /// The credential is read from this environment variable.
    Env { name: String },
    /// The credential is the Codex CLI's `ChatGPT` auth file for this account.
    CodexFile { account: String },
}

impl ProviderCredentialSource {
    /// Stable credential kind for telemetry and diagnostics.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Env { .. } => "env",
            Self::CodexFile { .. } => "codex_file",
        }
    }
}

impl Display for ProviderCredentialSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Env { name } => write!(formatter, "env:{name}"),
            Self::CodexFile { account } => write!(formatter, "codex_file:{account}"),
        }
    }
}

/// Non-secret request-shape controls that affect cache and wire behavior.
///
/// Every field is optional and omitted when unset. `provider_headers_digest`
/// is the SHA-256 digest of the configured provider header values; the raw
/// values are never persisted. Unknown fields are rejected so a binary cannot
/// silently compare a partial request-shape snapshot.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProviderRequestShape {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_headers_digest: Option<String>,
}

impl ProviderRequestShape {
    /// Whether no request-shape control was configured.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.temperature.is_none()
            && self.top_p.is_none()
            && self.top_k.is_none()
            && self.max_output_tokens.is_none()
            && self.reasoning_effort.is_none()
            && self.reasoning_summary.is_none()
            && self.reasoning_max_tokens.is_none()
            && self.provider_headers_digest.is_none()
    }
}

/// Sanitized identity of the provider destination a session was created for.
///
/// Additive to session format version 4: the whole object is optional on
/// `session_meta`, and every nested field is optional or omitted when absent.
/// Unknown fields are rejected so a binary that does not understand a newer
/// identity shape fails closed instead of comparing a partial snapshot.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProviderIdentity {
    /// The `[[models]]` entry name the session was created with.
    pub model_config: String,
    /// The provider's model identifier.
    pub model: String,
    /// Canonical endpoint with query, fragment, and userinfo removed.
    pub base_url: String,
    /// One-way SHA-256 digest of the exact configured query, when non-empty.
    ///
    /// The raw query can affect upstream routing or carry a credential, so it
    /// is never persisted; the digest makes a query-only change a mismatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url_query_digest: Option<String>,
    /// Wire format, using the same vocabulary as `[[models]].api_type`.
    pub api_type: String,
    /// Resolved provider strategy, when one applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Provider routing hints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routing: Vec<String>,
    /// Where the credential comes from, never the credential itself.
    pub credential: ProviderCredentialSource,
    /// Request-shape controls recorded for cache attribution.
    #[serde(default, skip_serializing_if = "ProviderRequestShape::is_empty")]
    pub request_shape: ProviderRequestShape,
}

/// One non-secret field that differs between a stored and a current identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderIdentityDifference {
    /// Stable field name for diagnostics.
    pub field: &'static str,
    /// The value stored in the session.
    pub stored: String,
    /// The value resolved from current settings.
    pub current: String,
}

impl ProviderIdentityDifference {
    /// One human-readable line naming the stored and current values.
    #[must_use]
    pub fn describe(&self) -> String {
        format!("{}: {} -> {}", self.field, self.stored, self.current)
    }
}

impl ProviderIdentity {
    /// Compare this stored identity with the current resolution.
    ///
    /// Returns one entry per changed non-secret field, in a stable field order.
    #[must_use]
    pub fn differences(&self, current: &Self) -> Vec<ProviderIdentityDifference> {
        let mut differences = Vec::new();
        push_difference(
            &mut differences,
            "model_config",
            &self.model_config,
            &current.model_config,
        );
        push_difference(&mut differences, "model", &self.model, &current.model);
        push_difference(
            &mut differences,
            "base_url",
            &self.base_url,
            &current.base_url,
        );
        push_optional_difference(
            &mut differences,
            "base_url_query_digest",
            self.base_url_query_digest.as_ref(),
            current.base_url_query_digest.as_ref(),
        );
        push_difference(
            &mut differences,
            "api_type",
            &self.api_type,
            &current.api_type,
        );
        push_optional_difference(
            &mut differences,
            "provider",
            self.provider.as_ref(),
            current.provider.as_ref(),
        );
        if self.routing != current.routing {
            differences.push(ProviderIdentityDifference {
                field: "routing",
                stored: shown_list(&self.routing),
                current: shown_list(&current.routing),
            });
        }
        push_difference(
            &mut differences,
            "credential",
            &self.credential,
            &current.credential,
        );
        self.request_shape
            .differences_into(&mut differences, &current.request_shape);
        differences
    }
}

impl ProviderRequestShape {
    fn differences_into(&self, differences: &mut Vec<ProviderIdentityDifference>, current: &Self) {
        push_optional_difference(
            differences,
            "request_shape.temperature",
            self.temperature.as_ref(),
            current.temperature.as_ref(),
        );
        push_optional_difference(
            differences,
            "request_shape.top_p",
            self.top_p.as_ref(),
            current.top_p.as_ref(),
        );
        push_optional_difference(
            differences,
            "request_shape.top_k",
            self.top_k.as_ref(),
            current.top_k.as_ref(),
        );
        push_optional_difference(
            differences,
            "request_shape.max_output_tokens",
            self.max_output_tokens.as_ref(),
            current.max_output_tokens.as_ref(),
        );
        push_optional_difference(
            differences,
            "request_shape.reasoning_effort",
            self.reasoning_effort.as_ref(),
            current.reasoning_effort.as_ref(),
        );
        push_optional_difference(
            differences,
            "request_shape.reasoning_summary",
            self.reasoning_summary.as_ref(),
            current.reasoning_summary.as_ref(),
        );
        push_optional_difference(
            differences,
            "request_shape.reasoning_max_tokens",
            self.reasoning_max_tokens.as_ref(),
            current.reasoning_max_tokens.as_ref(),
        );
        push_optional_difference(
            differences,
            "request_shape.provider_headers_digest",
            self.provider_headers_digest.as_ref(),
            current.provider_headers_digest.as_ref(),
        );
    }
}

fn push_difference<T: Display + PartialEq>(
    differences: &mut Vec<ProviderIdentityDifference>,
    field: &'static str,
    stored: &T,
    current: &T,
) {
    if stored != current {
        differences.push(ProviderIdentityDifference {
            field,
            stored: stored.to_string(),
            current: current.to_string(),
        });
    }
}

fn push_optional_difference<T: Display + PartialEq>(
    differences: &mut Vec<ProviderIdentityDifference>,
    field: &'static str,
    stored: Option<&T>,
    current: Option<&T>,
) {
    if stored != current {
        differences.push(ProviderIdentityDifference {
            field,
            stored: shown_optional(stored),
            current: shown_optional(current),
        });
    }
}

fn shown_optional<T: Display>(value: Option<&T>) -> String {
    value.map_or_else(|| "none".to_string(), ToString::to_string)
}

fn shown_list(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_string()
    } else {
        values.join(",")
    }
}

#[cfg(test)]
#[path = "provider_identity_tests.rs"]
mod tests;
