use super::*;
use crate::config::model::{ApiType, ModelConfig, ResolvedModelConfig};
use reqwest::header::HeaderMap;

const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

const TOKEN_EXPIRED_BODY: &str = r#"{"error":{"message":"Provided authentication token is expired. Please try signing in again.","type":"invalid_request_error","code":"token_expired","param":null}}"#;

fn failure(status: u16, body: &str) -> HttpFailure {
    HttpFailure {
        status,
        headers: HeaderMap::new(),
        body: body.to_string(),
    }
}

fn config(base_url: &str) -> ResolvedModelConfig {
    ResolvedModelConfig {
        model_config: ModelConfig {
            supports_images: false,
            model: "test-model".to_string(),
            api_type: ApiType::Responses,
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
            providers: vec![],
        },
        api_key: "test-key".to_string(),
    }
}

#[test]
fn codex_token_expired_error_adds_the_refresh_remedy() {
    let error = api_error_from_failure(&config(CODEX_BASE_URL), &failure(401, TOKEN_EXPIRED_BODY));

    assert_eq!(error.status, 401);
    assert!(error.body.contains("token_expired"));
    assert!(
        error.body.contains(
            "The ChatGPT access token in ~/.codex/auth.json has expired. Run the Codex CLI to refresh it (for example `codex login`), then retry."
        ),
        "unexpected body: {}",
        error.body
    );
}

#[test]
fn codex_401_without_the_expired_code_still_names_the_credential() {
    let error = api_error_from_failure(
        &config(CODEX_BASE_URL),
        &failure(401, r#"{"error":{"message":"Unauthorized"}}"#),
    );

    assert!(
        error.body.contains(
            "The Codex backend rejected the stored ChatGPT credential in ~/.codex/auth.json"
        ),
        "unexpected body: {}",
        error.body
    );
}

#[test]
fn non_codex_401_keeps_the_plain_provider_body() {
    let error = api_error_from_failure(
        &config("https://api.openai.com/v1"),
        &failure(401, TOKEN_EXPIRED_BODY),
    );

    assert!(error.body.starts_with("test-model"));
    assert!(error.body.contains("token_expired"));
    assert!(
        !error.body.contains("auth.json"),
        "unexpected body: {}",
        error.body
    );
}

#[test]
fn codex_non_401_keeps_the_plain_provider_body() {
    let error = api_error_from_failure(&config(CODEX_BASE_URL), &failure(500, "upstream boom"));

    assert_eq!(error.status, 500);
    assert!(error.body.starts_with("test-model"));
    assert!(error.body.contains("upstream boom"));
    assert!(
        !error.body.contains("auth.json"),
        "unexpected body: {}",
        error.body
    );
}

#[test]
fn remedy_is_absent_for_other_backends_and_statuses() {
    let openai = "https://api.openai.com/v1";
    assert!(codex_auth_remedy(openai, &failure(401, TOKEN_EXPIRED_BODY)).is_none());
    assert!(codex_auth_remedy(CODEX_BASE_URL, &failure(400, TOKEN_EXPIRED_BODY)).is_none());
    assert!(codex_auth_remedy(CODEX_BASE_URL, &failure(401, TOKEN_EXPIRED_BODY)).is_some());
}
