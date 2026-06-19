//! Reusable test fixtures for integration tests.
//!
//! - [`responses`] — shared wiremock response builders (success, rate-limit, auth error)
//! - [`settings`] — settings TOML template helpers (Responses API, Chat Completions API)

pub mod responses;
pub mod settings;
