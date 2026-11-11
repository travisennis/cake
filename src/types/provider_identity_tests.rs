use super::*;

fn env_identity() -> ProviderIdentity {
    ProviderIdentity {
        model_config: "zen".to_string(),
        model: "glm-5.1".to_string(),
        base_url: "https://opencode.ai/zen/go/v1/".to_string(),
        base_url_query_digest: None,
        api_type: "chat_completions".to_string(),
        provider: Some("opencode".to_string()),
        routing: Vec::new(),
        credential: ProviderCredentialSource::Env {
            name: "OPENCODE_ZEN_API_TOKEN".to_string(),
        },
        request_shape: ProviderRequestShape::default(),
    }
}

#[test]
fn identity_serializes_only_present_optional_fields() {
    let json = serde_json::to_value(env_identity()).unwrap();
    assert_eq!(json["credential"]["kind"], "env");
    assert_eq!(json["credential"]["name"], "OPENCODE_ZEN_API_TOKEN");
    assert!(json.get("routing").is_none());
    assert!(json.get("request_shape").is_none());
    assert!(json.get("base_url_query_digest").is_none());
    assert_eq!(json["api_type"], "chat_completions");
    assert_eq!(json["provider"], "opencode");

    let round_trip: ProviderIdentity = serde_json::from_value(json).unwrap();
    assert_eq!(round_trip, env_identity());
}

#[test]
fn codex_identity_serializes_account_digest_without_a_token_field() {
    let identity = ProviderIdentity {
        credential: ProviderCredentialSource::CodexFile {
            account: "a".repeat(64),
        },
        ..env_identity()
    };
    let json = serde_json::to_string(&identity).unwrap();
    assert!(json.contains("\"kind\":\"codex_file\""));
    assert!(json.contains("\"account\""));
    for forbidden in ["access_token", "api_key", "authorization", "bearer"] {
        assert!(!json.contains(forbidden), "{forbidden}: {json}");
    }
}

#[test]
fn differences_name_every_changed_non_secret_field() {
    let stored = ProviderIdentity {
        base_url_query_digest: Some("query-a".to_string()),
        request_shape: ProviderRequestShape {
            temperature: Some(0.7),
            max_output_tokens: Some(8_000),
            provider_headers_digest: Some("digest-a".to_string()),
            ..ProviderRequestShape::default()
        },
        ..env_identity()
    };
    let current = ProviderIdentity {
        base_url: "https://api.example.invalid/v1".to_string(),
        base_url_query_digest: Some("query-b".to_string()),
        api_type: "responses".to_string(),
        provider: None,
        credential: ProviderCredentialSource::Env {
            name: "OTHER_KEY".to_string(),
        },
        request_shape: ProviderRequestShape {
            temperature: Some(1.0),
            max_output_tokens: Some(4_000),
            provider_headers_digest: Some("digest-b".to_string()),
            ..ProviderRequestShape::default()
        },
        ..env_identity()
    };

    let differences = stored.differences(&current);
    let fields = differences
        .iter()
        .map(|difference| difference.field)
        .collect::<Vec<_>>();
    assert_eq!(
        fields,
        vec![
            "base_url",
            "base_url_query_digest",
            "api_type",
            "provider",
            "credential",
            "request_shape.temperature",
            "request_shape.max_output_tokens",
            "request_shape.provider_headers_digest",
        ]
    );
    let credential = differences
        .iter()
        .find(|difference| difference.field == "credential")
        .unwrap();
    assert!(credential.describe().contains("env:OPENCODE_ZEN_API_TOKEN"));
    assert!(credential.describe().contains("env:OTHER_KEY"));
}

#[test]
fn identical_identities_have_no_differences() {
    assert!(env_identity().differences(&env_identity()).is_empty());
}

#[test]
fn credential_kind_and_description_never_expose_a_value() {
    let env = ProviderCredentialSource::Env {
        name: "SOME_KEY".to_string(),
    };
    assert_eq!(env.kind(), "env");
    assert_eq!(env.to_string(), "env:SOME_KEY");

    let codex = ProviderCredentialSource::CodexFile {
        account: "deadbeef".to_string(),
    };
    assert_eq!(codex.kind(), "codex_file");
    assert_eq!(codex.to_string(), "codex_file:deadbeef");
}
