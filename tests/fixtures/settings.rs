//! Shared settings TOML templates for integration tests.
//!
//! These helpers produce project-level `settings.toml` content so tests
//! don't duplicate model/backend configuration strings. Not every template
//! is consumed by every test file — dead-code warnings are expected.

#![expect(
    dead_code,
    reason = "shared fixture library — not all templates are consumed by every test file"
)]

/// Generate a `.cake/settings.toml` fragment for a Responses API backend
/// pointed at the given `base_url` (typically a `wiremock::MockServer` URI).
pub fn responses_api(base_url: &str, api_key_env: &str) -> String {
    format!(
        r#"
default_model = "test"

[[models]]
name = "test"
model = "glm-5.1"
base_url = "{base_url}"
api_key_env = "{api_key_env}"
api_type = "responses"
"#
    )
}

/// Generate a `.cake/settings.toml` fragment for a Chat Completions API
/// backend pointed at the given `base_url`.
pub fn chat_completions_api(base_url: &str, api_key_env: &str) -> String {
    format!(
        r#"
default_model = "test"

[[models]]
name = "test"
model = "gpt-4.1"
base_url = "{base_url}"
api_key_env = "{api_key_env}"
api_type = "chat_completions"
"#
    )
}
