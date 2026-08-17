use base64::Engine;
use serde_json::json;

use super::{ChatGptAuth, account_id_from_jwt, is_chatgpt_codex_backend};

#[test]
fn recognizes_only_the_first_party_codex_backend() {
    assert!(is_chatgpt_codex_backend(
        "https://chatgpt.com/backend-api/codex"
    ));
    assert!(is_chatgpt_codex_backend(
        "https://chatgpt.com/backend-api/codex/"
    ));
    assert!(!is_chatgpt_codex_backend("https://api.openai.com/v1"));
    assert!(!is_chatgpt_codex_backend(
        "https://example.com/backend-api/codex"
    ));
}

#[test]
fn extracts_account_id_from_id_token() {
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "account-123"
            }
        })
        .to_string(),
    );
    let jwt = format!("header.{payload}.signature");

    assert_eq!(account_id_from_jwt(&jwt), Some("account-123".to_string()));
}

#[test]
fn loads_file_backed_auth() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("auth.json");
    std::fs::write(
        &path,
        r#"{"tokens":{"access_token":"access-123","account_id":"account-123"}}"#,
    )
    .expect("write auth file");

    assert_eq!(
        ChatGptAuth::load_from_path(&path).expect("load auth file"),
        ChatGptAuth {
            access_token: "access-123".to_string(),
            account_id: "account-123".to_string(),
        }
    );
}
