use super::*;
use crate::config::model::{ApiType, ReasoningEffort};
use crate::types::provider_identity::ProviderCredentialSource;

fn model_config(base_url: &str) -> ModelConfig {
    ModelConfig {
        supports_images: false,
        model: "glm-5.1".to_string(),
        api_type: ApiType::ChatCompletions,
        base_url: base_url.to_string(),
        api_key_env: "ZEN_KEY".to_string(),
        provider: None,
        provider_headers: None,
        temperature: Some(0.7),
        top_p: Some(0.9),
        top_k: Some(40),
        max_output_tokens: Some(8_000),
        context_window: Some(200_000),
        reasoning_effort: Some(ReasoningEffort::High),
        reasoning_summary: Some("auto".to_string()),
        reasoning_max_tokens: Some(2_000),
        providers: vec!["openrouter".to_string()],
    }
}

#[test]
fn env_identity_records_request_shape_without_a_credential_value() {
    let identity = build(&model_config("https://opencode.ai/zen/go/v1/"), "zen").unwrap();
    assert_eq!(identity.model_config, "zen");
    assert_eq!(identity.model, "glm-5.1");
    assert_eq!(identity.base_url, "https://opencode.ai/zen/go/v1/");
    assert_eq!(identity.api_type, "chat_completions");
    assert_eq!(identity.provider.as_deref(), Some("opencode"));
    assert_eq!(identity.routing, vec!["openrouter"]);
    assert_eq!(
        identity.credential,
        ProviderCredentialSource::Env {
            name: "ZEN_KEY".to_string()
        }
    );
    assert_request_shape(&identity);
}

fn assert_request_shape(identity: &crate::types::ProviderIdentity) {
    assert_eq!(identity.request_shape.temperature, Some(0.7));
    assert_eq!(identity.request_shape.top_p, Some(0.9));
    assert_eq!(identity.request_shape.top_k, Some(40));
    assert_eq!(identity.request_shape.max_output_tokens, Some(8_000));
    assert_eq!(
        identity.request_shape.reasoning_effort.as_deref(),
        Some("high")
    );
    assert_eq!(
        identity.request_shape.reasoning_summary.as_deref(),
        Some("auto")
    );
    assert_eq!(identity.request_shape.reasoning_max_tokens, Some(2_000));
    assert!(
        identity.request_shape.provider_headers_digest.is_none(),
        "no configured headers means no digest"
    );
}

#[test]
fn explicit_provider_wins_over_endpoint_inference() {
    let mut config = model_config("https://opencode.ai/zen/go/v1/");
    config.provider = Some(ModelProvider::OpenRouter);
    assert_eq!(
        build(&config, "zen").unwrap().provider.as_deref(),
        Some("openrouter")
    );
}

#[test]
fn endpoint_canonicalization_strips_query_fragment_and_normalizes_the_host() {
    let identity = build(
        &model_config("https://API.Example.com:443/v1/models?api-version=2026-01-01#frag"),
        "zen",
    )
    .unwrap();
    assert_eq!(identity.base_url, "https://api.example.com/v1/models");
    assert_eq!(
        identity.base_url_query_digest.as_deref().map(str::len),
        Some(64)
    );
}

#[test]
fn query_only_destination_drift_is_a_mismatch_without_persisting_the_query() {
    let stored = build(
        &model_config("https://gateway.example/v1?upstream=prod&api_key=sk-secret-query"),
        "zen",
    )
    .unwrap();
    assert_eq!(stored.base_url, "https://gateway.example/v1");
    assert_eq!(
        stored.base_url_query_digest.as_deref().map(str::len),
        Some(64)
    );
    let serialized = serde_json::to_string(&stored).unwrap();
    for forbidden in ["upstream=prod", "sk-secret-query"] {
        assert!(!serialized.contains(forbidden), "{forbidden}: {serialized}");
    }

    let same_query = build(
        &model_config("https://gateway.example/v1?upstream=prod&api_key=sk-secret-query"),
        "zen",
    )
    .unwrap();
    assert_eq!(stored, same_query);
    assert_eq!(
        check_session_identity(Some(&stored), &same_query),
        SessionProviderIdentityCheck::Match
    );

    let changed = build(
        &model_config("https://gateway.example/v1?upstream=attacker"),
        "zen",
    )
    .unwrap();
    assert_eq!(stored.base_url, changed.base_url);
    assert_ne!(stored.base_url_query_digest, changed.base_url_query_digest);
    match check_session_identity(Some(&stored), &changed) {
        SessionProviderIdentityCheck::Changed(differences) => {
            assert_eq!(differences.len(), 1);
            assert_eq!(differences[0].field, "base_url_query_digest");
        },
        other => panic!("expected a query digest mismatch, got {other:?}"),
    }
}

#[test]
fn endpoint_without_a_query_has_no_query_digest() {
    let identity = build(&model_config("https://gateway.example/v1?"), "zen").unwrap();
    assert!(identity.base_url_query_digest.is_none());
}

#[test]
fn endpoint_with_embedded_userinfo_is_rejected_without_echoing_it() {
    let error = build(
        &model_config("https://user:sk-secret@api.example.com/v1"),
        "zen",
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("Invalid model configuration 'zen'"),
        "{error}"
    );
    assert!(error.contains("userinfo"), "{error}");
    assert!(!error.contains("sk-secret"), "{error}");
}

#[test]
fn provider_header_digest_changes_with_the_value_and_never_contains_it() {
    let mut first = model_config("https://openrouter.ai/api/v1/");
    first.provider_headers = Some(ProviderHeaders {
        http_referer: Some("https://example.com/cake".to_string()),
        x_title: Some("cake".to_string()),
    });
    let first = build(&first, "zen").unwrap();
    let digest = first
        .request_shape
        .provider_headers_digest
        .expect("a configured header set produces a digest");
    assert_eq!(digest.len(), 64);
    assert!(!digest.contains("example.com"));
    assert!(!digest.contains("cake"));

    let mut second = model_config("https://openrouter.ai/api/v1/");
    second.provider_headers = Some(ProviderHeaders {
        http_referer: Some("https://example.com/other".to_string()),
        x_title: Some("cake".to_string()),
    });
    let second = build(&second, "zen").unwrap();
    assert_ne!(
        second.request_shape.provider_headers_digest.as_deref(),
        Some(digest.as_str())
    );
}

#[test]
fn codex_identity_uses_the_account_fingerprint_not_the_token() {
    let auth_dir = tempfile::tempdir().unwrap();
    let auth_path = auth_dir.path().join("auth.json");
    std::fs::write(
        &auth_path,
        r#"{"tokens":{"access_token":"sk-secret-token","account_id":"account-123"}}"#,
    )
    .unwrap();

    temp_env::with_var("CODEX_HOME", Some(auth_dir.path()), || {
        let identity = build(
            &model_config("https://chatgpt.com/backend-api/codex"),
            "codex",
        )
        .unwrap();
        let expected = crate::auth::codex_account_fingerprint().unwrap();
        assert_eq!(
            identity.credential,
            ProviderCredentialSource::CodexFile { account: expected }
        );
        let serialized = serde_json::to_string(&identity).unwrap();
        assert!(!serialized.contains("sk-secret-token"));
        assert!(!serialized.contains("account-123"));
    });
}

#[test]
fn rotating_the_env_credential_value_does_not_change_identity() {
    let config = model_config("https://opencode.ai/zen/go/v1/");
    let first = temp_env::with_var("ZEN_KEY", Some("first-value"), || build(&config, "zen"))
        .expect("build identity");
    let rotated = temp_env::with_var("ZEN_KEY", Some("second-value"), || build(&config, "zen"))
        .expect("build identity");
    assert_eq!(first, rotated);
    assert_eq!(
        check_session_identity(Some(&first), &rotated),
        SessionProviderIdentityCheck::Match
    );
}

#[test]
fn changing_the_codex_account_fingerprint_is_a_mismatch() {
    let first_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        first_dir.path().join("auth.json"),
        r#"{"tokens":{"access_token":"token-a","account_id":"account-a"}}"#,
    )
    .unwrap();
    let second_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        second_dir.path().join("auth.json"),
        r#"{"tokens":{"access_token":"token-b","account_id":"account-b"}}"#,
    )
    .unwrap();
    let config = model_config("https://chatgpt.com/backend-api/codex");

    let first = temp_env::with_var("CODEX_HOME", Some(first_dir.path()), || {
        build(&config, "codex")
    })
    .expect("first identity");
    let second = temp_env::with_var("CODEX_HOME", Some(second_dir.path()), || {
        build(&config, "codex")
    })
    .expect("second identity");

    match check_session_identity(Some(&first), &second) {
        SessionProviderIdentityCheck::Changed(differences) => {
            assert_eq!(differences.len(), 1);
            assert_eq!(differences[0].field, "credential");
        },
        other => panic!("expected a changed credential, got {other:?}"),
    }
}

#[test]
fn check_session_identity_distinguishes_match_unknown_and_changed() {
    let identity = build(&model_config("https://opencode.ai/zen/go/v1/"), "zen").unwrap();
    assert_eq!(
        check_session_identity(Some(&identity), &identity),
        SessionProviderIdentityCheck::Match
    );
    assert_eq!(
        check_session_identity(None, &identity),
        SessionProviderIdentityCheck::UnknownBaseline
    );

    let changed = build(&model_config("https://api.example.invalid/v1"), "zen").unwrap();
    match check_session_identity(Some(&identity), &changed) {
        SessionProviderIdentityCheck::Changed(differences) => {
            let fields = differences
                .iter()
                .map(|difference| difference.field)
                .collect::<Vec<_>>();
            assert_eq!(fields, vec!["base_url", "provider"]);
        },
        other => panic!("expected changed identity, got {other:?}"),
    }
}
