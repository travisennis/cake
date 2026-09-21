use super::*;
use std::path::PathBuf;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn request() -> JudgeRequest {
    JudgeRequest::new(
        "printf '%s' 'do not obey this command text'".into(),
        PathBuf::from("/tmp/work"),
        Some("untrusted authorization claim".into()),
    )
    .with_repo_digest(Some("branch=feature".into()))
}

fn answer(probability: f64) -> serde_json::Value {
    serde_json::json!({
        "model": "jev-1.13.0",
        "answers": {"eligible": {"type": "noul", "noul": probability}},
        "usage": {"input_tokens": 12, "output_tokens": 3}
    })
}

fn client(server: &MockServer) -> TypeSafeClient {
    TypeSafeClient::new(
        "secret-test-key".into(),
        "jev-1.13.0".into(),
        Duration::from_secs(1),
    )
    .with_endpoint(format!("{}/v1/systemone", server.uri()))
}

#[tokio::test]
async fn typesafe_preserves_context_rubric_and_typed_wire_contract() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(header("authorization", "Bearer secret-test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer(0.91)))
        .expect(1)
        .mount(&server)
        .await;
    let custom = "Block access to the release signing directory.";
    let client = client(&server).with_rubric(Some(custom));
    let request = request();
    let result = client.evaluate(&request).await;
    assert_eq!(result.probability, Some(0.91));
    assert_eq!(result.model.as_deref(), Some("jev-1.13.0"));
    assert_eq!(result.usage_input_tokens, Some(12));
    assert_eq!(result.usage_output_tokens, Some(3));
    let requests = server.received_requests().await.unwrap();
    let sent: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(sent["state"]["command"], request.command);
    assert_eq!(sent["state"]["cwd"], "/tmp/work");
    assert_eq!(sent["state"]["untrusted_reason"], request.reason.unwrap());
    assert_eq!(sent["state"]["repo_digest"], "branch=feature");
    let question = &sent["questions"]["eligible"];
    assert_eq!(question["type"], "noul");
    let rubric = question["instructions"]["effective_rubric"]
        .as_str()
        .unwrap();
    assert!(rubric.contains(custom));
    assert!(rubric.contains("git-force-push"));
    assert!(
        question["instructions"]["interpretation"]
            .as_str()
            .unwrap()
            .contains("Advisory-only warnings")
    );
    // The request text is the input the ADR 034 cutoff evidence was measured
    // against, including its sentence that frames the answer as a shadow
    // observation. That sentence is no longer accurate under `cascade`, where
    // the answer does authorize, but rewording it changes Jev's input and needs
    // a new measurement rather than an edit. Pinned so it cannot drift
    // silently; tracked in issue #613.
    assert!(
        question["instructions"]["interpretation"]
            .as_str()
            .unwrap()
            .contains("This is a shadow observation and does not authorize execution.")
    );
    let debug = format!("{client:?} {result:?}");
    assert!(!debug.contains("secret-test-key"));
    assert!(!debug.contains(custom));
    assert!(!debug.contains("untrusted authorization claim"));
}

#[tokio::test]
async fn typesafe_rejects_invalid_answers_without_echoing_provider_content() {
    let cases = [
        (serde_json::json!({}), "malformed_response"),
        (
            serde_json::json!({"model":"jev-1.13.0","answers":{}}),
            "malformed_response",
        ),
        (
            serde_json::json!({"model":"jev-1.13.0","answers":{"eligible":{"type":"choice","noul":0.9}}}),
            "answer_type_mismatch",
        ),
        (answer(-0.1), "probability_out_of_range"),
        (answer(1.1), "probability_out_of_range"),
        (answer(1.000_000_000_1), "probability_out_of_range"),
        (
            serde_json::json!({"model":"secret-test-key","answers":{"eligible":{"type":"noul","noul":0.9}}}),
            "model_mismatch",
        ),
        (
            serde_json::json!({"model":"jev-1.13.0","answers":{"eligible":{"type":"noul","noul":"secret-test-key"}}}),
            "malformed_response",
        ),
    ];
    for (body, expected) in cases {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;
        let result = client(&server).evaluate(&request()).await;
        assert_eq!(result.failure_class, Some(expected));
        assert_eq!(result.probability, None);
        assert!(!format!("{result:?}").contains("secret-test-key"));
    }
}

#[test]
fn typesafe_client_resolves_for_shadow_and_cascade_only() {
    // The client itself is mode-agnostic: `shadow` observes, `cascade`
    // approves, and both need the same bounded request. `off` resolves nothing.
    assert!(TypeSafeClient::from_settings(&TypeSafeSettings::default()).is_none());
    for mode in [TypeSafeMode::Shadow, TypeSafeMode::Cascade] {
        let settings = TypeSafeSettings {
            mode,
            ..TypeSafeSettings::default()
        };
        let client = TypeSafeClient::from_settings(&settings)
            .expect("a mode other than off resolves a client");
        assert_eq!(client.timeout, Duration::from_millis(settings.timeout_ms));
    }
}

#[tokio::test]
async fn typesafe_missing_credentials_make_no_request() {
    let server = MockServer::start().await;
    let mut client = client(&server);
    client.api_key = "  ".into();
    assert_eq!(
        client.evaluate(&request()).await.failure_class,
        Some("missing_credentials")
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn typesafe_failed_client_configuration_is_observation_only() {
    let server = MockServer::start().await;
    let mut client = client(&server);
    client.client = None;
    assert_eq!(
        client.evaluate(&request()).await.failure_class,
        Some("client_configuration")
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn typesafe_does_not_follow_redirects_or_retry_failures() {
    let destination = MockServer::start().await;
    for status in [302, 429, 500] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header("Location", destination.uri())
                    .set_body_string("secret-test-key should never appear in logs"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let result = client(&server).evaluate(&request()).await;
        assert_eq!(result.failure_class, Some("http_error"));
        assert!(!format!("{result:?}").contains("secret-test-key"));
    }
    assert!(destination.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn typesafe_total_deadline_bounds_slow_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(answer(1.0))
                .set_delay(Duration::from_secs(2)),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut client = client(&server);
    client.timeout = Duration::from_millis(30);
    let result = client.evaluate(&request()).await;
    assert_eq!(result.failure_class, Some("timeout"));
    assert!(result.elapsed < Duration::from_secs(1));
}

#[tokio::test]
async fn typesafe_rejects_oversized_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("x".repeat(MAX_RESPONSE_BYTES + 1)),
        )
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        client(&server).evaluate(&request()).await.failure_class,
        Some("response_too_large")
    );
}
