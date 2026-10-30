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
            top_k: None,
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
    let error = api_error_from_failure(
        &config(CODEX_BASE_URL),
        &failure(401, TOKEN_EXPIRED_BODY),
        None,
    );

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
        None,
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
        None,
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
    let error = api_error_from_failure(
        &config(CODEX_BASE_URL),
        &failure(500, "upstream boom"),
        None,
    );

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
fn incomplete_body_suppresses_the_codex_auth_remedy() {
    let failure = failure(401, TOKEN_EXPIRED_BODY);
    let stall = ResponseStall::new(
        "test-model",
        StallPhase::FirstByte,
        Duration::from_secs(1),
        0,
    );
    let stalled = BodyReadFailure::stalled(stall);

    assert!(codex_auth_remedy(CODEX_BASE_URL, &failure, Some(&stalled)).is_none());
    let error = api_error_from_failure(&config(CODEX_BASE_URL), &failure, Some(stalled));
    assert_eq!(error.status, 401);
    assert!(error.body_read_failure.is_some());
    assert!(
        !error.body.contains("auth.json"),
        "a stalled 401 must not select a credential remedy: {}",
        error.body
    );

    let failed = BodyReadFailure::failed(anyhow::anyhow!("connection reset"));
    assert!(codex_auth_remedy(CODEX_BASE_URL, &failure, Some(&failed)).is_none());
}

#[test]
fn parse_error_telemetry_detail_drops_decode_preview() {
    let body = br#"{"choices":"assistant text"#;
    let source = serde_json::from_slice::<serde_json::Value>(body).unwrap_err();
    let error = anyhow::Error::new(ResponseDecodeError::new(
        "Chat Completions",
        body,
        None,
        source,
    ));

    let human = format!("{error:#}");
    assert!(human.contains("assistant text"), "{human}");
    assert!(human.contains("first"), "{human}");

    let telemetry = parse_error_telemetry_detail(&error);
    assert!(
        telemetry.contains("failed to decode Chat Completions response body"),
        "{telemetry}"
    );
    assert!(!telemetry.contains("assistant text"), "{telemetry}");
    assert!(!telemetry.contains("first"), "{telemetry}");
}

#[test]
fn parse_error_telemetry_detail_drops_parse_preview() {
    let error = anyhow::Error::new(ResponseParseError::new_with_telemetry_detail(
        anyhow::anyhow!("malformed streamed item; first 12 bytes: \"assistant text\""),
        None,
        "malformed streamed item: missing field".to_string(),
    ));

    let human = format!("{error:#}");
    assert!(human.contains("assistant text"), "{human}");

    let telemetry = parse_error_telemetry_detail(&error);
    assert_eq!(telemetry, "malformed streamed item: missing field");
}

#[test]
fn remedy_is_absent_for_other_backends_and_statuses() {
    let openai = "https://api.openai.com/v1";
    assert!(codex_auth_remedy(openai, &failure(401, TOKEN_EXPIRED_BODY), None).is_none());
    assert!(codex_auth_remedy(CODEX_BASE_URL, &failure(400, TOKEN_EXPIRED_BODY), None).is_none());
    assert!(codex_auth_remedy(CODEX_BASE_URL, &failure(401, TOKEN_EXPIRED_BODY), None).is_some());
}
