//! Shared wiremock response fixtures for integration tests.
//!
//! These builders produce JSON bodies for common API response scenarios
//! so tests don't duplicate mock response construction. Not every fixture
//! is consumed by every test file — dead-code warnings are expected.

#![expect(
    dead_code,
    reason = "shared fixture library — not all fixtures are consumed by every test file"
)]

use wiremock::ResponseTemplate;

/// A minimal success response from the Responses API.
pub fn success() -> serde_json::Value {
    serde_json::json!({
        "id": "resp-fixture",
        "output": [
            {
                "type": "message",
                "id": "msg-fixture",
                "status": "completed",
                "content": [
                    {
                        "type": "output_text",
                        "text": "Hello from fixture!"
                    }
                ]
            }
        ],
        "usage": {
            "input_tokens": 10,
            "output_tokens": 5,
            "total_tokens": 15
        }
    })
}

/// Build a 200 `ResponseTemplate` wrapping a success body.
pub fn success_template() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(success())
}

/// A rate-limit (429) response body with a retry-after header.
pub fn rate_limit() -> serde_json::Value {
    serde_json::json!({
        "error": {
            "message": "Rate limit exceeded. Try again later."
        }
    })
}

/// Build a 429 `ResponseTemplate` for rate-limit testing.
pub fn rate_limit_template() -> ResponseTemplate {
    ResponseTemplate::new(429)
        .insert_header("retry-after", "0")
        .set_body_json(rate_limit())
}

/// An authentication error (401) response body.
pub fn auth_error() -> serde_json::Value {
    serde_json::json!({
        "error": {
            "message": "Invalid API key."
        }
    })
}

/// Build a 401 `ResponseTemplate` for auth-failure testing.
pub fn auth_error_template() -> ResponseTemplate {
    ResponseTemplate::new(401).set_body_json(auth_error())
}
