use super::*;
use crate::clients::judge::{JudgeDecision, JudgeError, judge_is_enabled};
use crate::config::ModelDefinition;
use crate::config::model::ApiType;
use crate::config::settings::{
    JudgeSettings, ResolvedLimits, SandboxSettings, SkillSettings, TypeSafeMode, TypeSafeSettings,
};
use clap::CommandFactory;
use std::collections::HashMap;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn test_config(base_url: String) -> ResolvedModelConfig {
    let model_config = crate::config::model::ModelConfig {
        model: "agent/model".to_string(),
        api_type: ApiType::ChatCompletions,
        base_url,
        api_key_env: "JUDGE_TEST_KEY".to_string(),
        provider: None,
        provider_headers: None,
        temperature: Some(0.0),
        top_p: None,
        max_output_tokens: Some(128),
        context_window: None,
        reasoning_effort: None,
        reasoning_summary: None,
        reasoning_max_tokens: None,
        providers: vec![],
    };
    ResolvedModelConfig {
        model_config,
        api_key: "test-key".to_string(),
    }
}

fn judge_client(mock_server: &MockServer) -> JudgeClient {
    JudgeClient::new(
        test_config(mock_server.uri()),
        Duration::from_secs(5),
        Duration::ZERO,
    )
}

fn chat_response(content: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-judge",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": "stop"
        }]
    })
}

fn model_definition(name: &str, base_url: &str) -> ModelDefinition {
    ModelDefinition {
        name: name.to_string(),
        model: format!("provider/{name}"),
        base_url: base_url.to_string(),
        api_key_env: "JUDGE_TEST_KEY".to_string(),
        provider: None,
        provider_headers: None,
        api_type: ApiType::ChatCompletions,
        temperature: None,
        top_p: None,
        max_output_tokens: None,
        context_window: None,
        reasoning_effort: None,
        reasoning_summary: None,
        reasoning_max_tokens: None,
        providers: vec![],
    }
}

fn loaded_settings(base_url: &str) -> LoadedSettings {
    let mut models = HashMap::new();
    models.insert("zen".to_string(), model_definition("zen", base_url));
    LoadedSettings {
        models,
        default_model: Some("zen".to_string()),
        directories: vec![],
        sandbox: SandboxSettings::default(),
        skills: SkillSettings::default(),
        tools_enabled: None,
        system_prompt: None,
        judge: JudgeSettings::default(),
        limits: ResolvedLimits::default(),
        warnings: Vec::new(),
    }
}

// =============================================================================
// CLI parsing and help
// =============================================================================

#[test]
fn cli_parses_bash_check_with_double_dash() {
    let args = crate::CodingAssistant::parse_from(["cake", "bash", "check", "--", "git status"]);
    match args.command {
        Some(crate::cli::Commands::Bash(cmd)) => match cmd.command {
            BashSubcommand::Check(check) => assert_eq!(check.command, "git status"),
        },
        other => panic!("expected bash check, got {other:?}"),
    }
}

#[test]
fn cli_parses_bash_check_without_double_dash() {
    let args = crate::CodingAssistant::parse_from(["cake", "bash", "check", "git status"]);
    match args.command {
        Some(crate::cli::Commands::Bash(cmd)) => match cmd.command {
            BashSubcommand::Check(check) => assert_eq!(check.command, "git status"),
        },
        other => panic!("expected bash check, got {other:?}"),
    }
}

#[test]
fn cli_parses_bash_check_diagnostic_flag() {
    let args = crate::CodingAssistant::parse_from([
        "cake",
        "bash",
        "check",
        "--diagnostic",
        "--",
        "printf test-key",
    ]);
    match args.command {
        Some(crate::cli::Commands::Bash(cmd)) => match cmd.command {
            BashSubcommand::Check(check) => {
                assert!(check.diagnostic);
                assert_eq!(check.command, "printf test-key");
            },
        },
        other => panic!("expected bash check, got {other:?}"),
    }
}

#[test]
fn cli_parses_bash_check_json_flag() {
    let args = crate::CodingAssistant::parse_from([
        "cake",
        "bash",
        "check",
        "--json",
        "--",
        "git push --force",
    ]);
    match args.command {
        Some(crate::cli::Commands::Bash(cmd)) => match cmd.command {
            BashSubcommand::Check(check) => {
                assert!(check.json);
                assert!(!check.diagnostic);
                assert_eq!(check.command, "git push --force");
            },
        },
        other => panic!("expected bash check, got {other:?}"),
    }
}

#[test]
fn cli_rejects_json_with_diagnostic() {
    // `--diagnostic` prints a raw sensitive report; `--json` prints a document.
    // Asking for both is a usage error rather than a silent choice.
    let Err(error) = crate::CodingAssistant::try_parse_from([
        "cake",
        "bash",
        "check",
        "--json",
        "--diagnostic",
        "--",
        "git status",
    ]) else {
        panic!("both output modes must conflict");
    };

    assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn bash_help_documents_check_without_executing() {
    let help = BashCommand::command().render_help().to_string();
    assert!(
        help.contains("check"),
        "help should list the check subcommand:\n{help}"
    );
    assert!(
        help.contains("without executing"),
        "help should state check never executes:\n{help}"
    );
}

#[tokio::test]
async fn bash_check_diagnostic_shows_exact_sensitive_request_without_credentials() {
    let mock_server = MockServer::start().await;
    let mut body = chat_response(r#"{"verdict":"allow","message":"Safe"}"#);
    body["usage"] = serde_json::json!({
        "prompt_tokens": 12,
        "completion_tokens": 3,
        "total_tokens": 15
    });
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&mock_server)
        .await;

    let report = evaluate_with_client_diagnostic(
        judge_client(&mock_server),
        &JudgeSettings::default(),
        None,
        std::path::Path::new("/work"),
        "printf test-key",
    )
    .await
    .unwrap();

    let output = &report.report;
    assert!(report.error.is_none());
    assert!(output.contains("WARNING: raw judge diagnostics"));
    assert!(output.contains("System prompt:"));
    assert!(output.contains("User prompt:"));
    assert!(output.contains("Transformed request JSON:"));
    assert!(output.contains("Parsed response:"));
    assert!(output.contains("Attempt metadata:"));
    assert!(output.contains("Tool count: 0"));
    assert!(output.contains("Verdict: allow"));
    assert!(output.contains("printf <redacted>"));
    assert!(!output.contains("test-key"));
    assert!(!output.contains("Authorization"));
}

#[tokio::test]
async fn bash_check_diagnostic_retains_request_on_malformed_verdict() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response("not json")))
        .mount(&mock_server)
        .await;

    let report = evaluate_with_client_diagnostic(
        judge_client(&mock_server),
        &JudgeSettings::default(),
        None,
        std::path::Path::new("/work"),
        "printf test-key",
    )
    .await
    .unwrap();

    let rendered = &report.report;
    assert!(rendered.contains("WARNING: raw judge diagnostics"));
    assert!(rendered.contains("Transformed request JSON:"));
    assert!(rendered.contains("Attempt metadata:"));
    assert!(rendered.contains("malformed_verdict"));
    assert!(rendered.contains("Judge error:"));
    assert!(rendered.contains("printf <redacted>"));
    assert!(!rendered.contains("test-key"));
    assert!(
        matches!(report.error, Some(JudgeError::Malformed(_))),
        "malformed verdict must carry a typed JudgeError for exit classification"
    );
}

#[tokio::test]
async fn bash_check_diagnostic_redacts_verdict_echoing_api_key() {
    let mock_server = MockServer::start().await;
    let content = serde_json::json!({
        "verdict": "allow",
        "message": "authorize with test-key and retry",
    })
    .to_string();
    let body = chat_response(&content);
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&mock_server)
        .await;

    let report = evaluate_with_client_diagnostic(
        judge_client(&mock_server),
        &JudgeSettings::default(),
        None,
        std::path::Path::new("/work"),
        "ls",
    )
    .await
    .unwrap();

    let output = &report.report;
    assert!(report.error.is_none());
    assert!(
        !output.contains("test-key"),
        "diagnostic verdict output must redact the API key:\n{output}"
    );
    assert!(output.contains("Verdict: allow"));
    assert!(output.contains("Message: authorize with <redacted> and retry"));
}

#[tokio::test]
async fn bash_check_diagnostic_redacts_transport_error_echoing_api_key() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_string("invalid api key test-key"))
        .mount(&mock_server)
        .await;

    let report = evaluate_with_client_diagnostic(
        judge_client(&mock_server),
        &JudgeSettings::default(),
        None,
        std::path::Path::new("/work"),
        "ls",
    )
    .await
    .unwrap();

    let rendered = &report.report;
    assert!(
        !rendered.contains("test-key"),
        "diagnostic transport report must redact the API key:\n{rendered}"
    );
    assert!(
        rendered.contains("HTTP 401 Unauthorized: invalid api key <redacted>"),
        "expected redacted transport detail, got:\n{rendered}"
    );
    assert!(
        matches!(
            report.error,
            Some(JudgeError::Transport {
                status: Some(401),
                ..
            })
        ),
        "transport failure must carry a typed JudgeError for exit classification"
    );
}

#[tokio::test]
async fn bash_check_diagnostic_redacts_api_key_embedded_in_model_name() {
    // A custom/local provider may put the API key inside the model identifier;
    // every rendering of that identifier (metadata lines, attempt metadata,
    // request JSON) must still omit the key.
    let mock_server = MockServer::start().await;
    let body = chat_response(r#"{"verdict":"allow","message":"Safe"}"#);
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&mock_server)
        .await;

    let mut config = test_config(mock_server.uri());
    config.model_config.model = "local/sk-test-key-123-model".to_string();
    let client = JudgeClient::new(config, Duration::from_secs(5), Duration::ZERO);

    let report = evaluate_with_client_diagnostic(
        client,
        &JudgeSettings::default(),
        None,
        std::path::Path::new("/work"),
        "ls",
    )
    .await
    .unwrap();

    assert!(report.error.is_none());
    assert!(
        !report.report.contains("test-key"),
        "API key embedded in the model name must be redacted from the report:\n{}",
        report.report
    );
}

#[tokio::test]
async fn bash_check_diagnostic_redacts_configured_provider_headers() {
    // Configured provider header values (for example `OpenRouter`'s
    // `HTTP-Referer`/`X-Title`) must be redacted from the report and the
    // propagated error when an endpoint echoes them back.
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_string("bad key for cake-app referral"))
        .mount(&mock_server)
        .await;

    let mut config = test_config(mock_server.uri());
    config.model_config.provider_headers = Some(crate::config::model::ProviderHeaders {
        http_referer: Some("https://cake.example".to_string()),
        x_title: Some("cake-app".to_string()),
    });
    let client = JudgeClient::new(config, Duration::from_secs(5), Duration::ZERO);

    let report = evaluate_with_client_diagnostic(
        client,
        &JudgeSettings::default(),
        None,
        std::path::Path::new("/work"),
        "ls",
    )
    .await
    .unwrap();

    let rendered = &report.report;
    assert!(
        !rendered.contains("cake-app"),
        "configured provider header value must be redacted from the report:\n{rendered}"
    );
    assert!(rendered.contains("bad key for <redacted> referral"));
    let error = report.error.expect("401 must carry a judge error");
    assert!(matches!(
        error,
        JudgeError::Transport {
            status: Some(401),
            ..
        }
    ));
    assert!(
        !error.to_string().contains("cake-app"),
        "propagated error must redact the provider header value"
    );
}

// =============================================================================
// Judge verdict rendering (allow / block / warn / error)
// =============================================================================

#[tokio::test]
async fn bash_check_renders_allow_verdict() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(
            r#"{"verdict":"allow","message":"Safe","confidence":0.9}"#,
        )))
        .mount(&mock_server)
        .await;

    let output = evaluate_with_client(
        judge_client(&mock_server),
        &JudgeSettings::default(),
        None,
        std::path::Path::new("/work"),
        "git status",
    )
    .await
    .unwrap()
    .render_text();

    assert!(output.contains("Verdict: allow"));
    assert!(output.contains("Confidence: 0.9"));
    assert!(output.contains("Message: Safe"));
    assert!(
        output.contains("Latency:"),
        "output must include latency:\n{output}"
    );
    assert!(!output.contains("Code:"), "allow needs no code:\n{output}");
}

#[tokio::test]
async fn bash_check_shares_judge_retry_semantics() {
    // `cake bash check` runs the same observed driver as the Bash preflight:
    // a timeout-then-allow script must recover within the operation deadline
    // and render the allow with cumulative latency.
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(chat_response(r#"{"verdict":"allow","message":"Safe"}"#))
                .set_delay(Duration::from_millis(500)),
        )
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(chat_response(r#"{"verdict":"allow","message":"Safe"}"#)),
        )
        .mount(&mock_server)
        .await;

    let client = JudgeClient::new(
        test_config(mock_server.uri()),
        Duration::from_millis(100),
        Duration::from_secs(1),
    );
    let output = evaluate_with_client(
        client,
        &JudgeSettings::default(),
        None,
        std::path::Path::new("/work"),
        "git status",
    )
    .await
    .unwrap()
    .render_text();

    assert!(
        output.contains("Verdict: allow"),
        "recovered check must render the allow verdict:\n{output}"
    );
    assert!(
        output.contains("Latency:"),
        "check must report cumulative latency after recovery:\n{output}"
    );
}

#[tokio::test]
async fn bash_check_renders_block_verdict() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(
            r#"{"verdict":"block","code":"git-force-push","message":"Prefer push --force-with-lease.","confidence":0.93}"#,
        )))
        .mount(&mock_server)
        .await;

    let output = evaluate_with_client(
        judge_client(&mock_server),
        &JudgeSettings::default(),
        None,
        std::path::Path::new("/work"),
        "git push --force",
    )
    .await
    .unwrap()
    .render_text();

    assert!(output.contains("Verdict: block"));
    assert!(output.contains("Code: git-force-push"));
    assert!(output.contains("Message: Prefer push --force-with-lease."));
}

#[tokio::test]
async fn bash_check_renders_warn_verdict() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(
            r#"{"verdict":"warn","code":"rg-replace-footgun","message":"Prefer rg -n foo.","confidence":0.7}"#,
        )))
        .mount(&mock_server)
        .await;

    let output = evaluate_with_client(
        judge_client(&mock_server),
        &JudgeSettings::default(),
        None,
        std::path::Path::new("/work"),
        "rg -rn foo",
    )
    .await
    .unwrap()
    .render_text();

    assert!(output.contains("Verdict: warn"));
    assert!(output.contains("Code: rg-replace-footgun"));
}

#[tokio::test]
async fn bash_check_judge_error_is_an_error() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("provider exploded"))
        .mount(&mock_server)
        .await;

    let result = evaluate_with_client(
        judge_client(&mock_server),
        &JudgeSettings::default(),
        None,
        std::path::Path::new("/work"),
        "git status",
    )
    .await;
    assert!(
        result.is_err(),
        "a judge failure must surface as an error (nonzero exit), not a verdict"
    );
}

// =============================================================================
// Allowlist and emergency bypass (Milestone 4 of the LLM-judge ExecPlan)
// =============================================================================

#[tokio::test]
async fn bash_check_allowlist_overrides_block_and_still_judges() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(
            r#"{"verdict":"block","code":"git-force-push","message":"Prefer push --force-with-lease.","confidence":0.93}"#,
        )))
        .expect(1) // an allowlisted command is still judged
        .mount(&mock_server)
        .await;

    let settings = JudgeSettings {
        allowlist: vec!["git push --force".to_string()],
        ..JudgeSettings::default()
    };
    let output = evaluate_with_client(
        judge_client(&mock_server),
        &settings,
        None,
        std::path::Path::new("/work"),
        "git push --force",
    )
    .await
    .unwrap()
    .render_text();

    // The original block verdict and the override flag are both visible.
    assert!(output.contains("Verdict: block"));
    assert!(output.contains("Code: git-force-push"));
    assert!(
        output.contains("Overridden: allowlist"),
        "expected an override note:\n{output}"
    );
    mock_server.verify().await;
}

#[tokio::test]
async fn bash_check_allowlisted_benign_verdict_is_unaffected() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(
            r#"{"verdict":"allow","message":"Safe","confidence":0.9}"#,
        )))
        .mount(&mock_server)
        .await;

    let settings = JudgeSettings {
        allowlist: vec!["git status".to_string()],
        ..JudgeSettings::default()
    };
    let output = evaluate_with_client(
        judge_client(&mock_server),
        &settings,
        None,
        std::path::Path::new("/work"),
        "git status",
    )
    .await
    .unwrap()
    .render_text();

    assert!(output.contains("Verdict: allow"));
    assert!(
        !output.contains("Overridden"),
        "a non-block verdict must not be marked overridden:\n{output}"
    );
}

#[tokio::test]
async fn bash_check_block_without_allowlist_match_is_not_overridden() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(
            r#"{"verdict":"block","code":"git-force-push","message":"Prefer push --force-with-lease."}"#,
        )))
        .mount(&mock_server)
        .await;

    let settings = JudgeSettings {
        allowlist: vec!["git status".to_string()],
        ..JudgeSettings::default()
    };
    let output = evaluate_with_client(
        judge_client(&mock_server),
        &settings,
        None,
        std::path::Path::new("/work"),
        "git push --force",
    )
    .await
    .unwrap()
    .render_text();

    assert!(output.contains("Verdict: block"));
    assert!(
        !output.contains("Overridden"),
        "a non-matching block must not be overridden:\n{output}"
    );
}

#[tokio::test]
async fn bash_check_bypass_setting_skips_judge() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(
            r#"{"verdict":"block","code":"git-force-push","message":"Prefer push --force-with-lease."}"#,
        )))
        .expect(0) // bypassed: no judge call may be made
        .mount(&mock_server)
        .await;

    let settings = JudgeSettings {
        enabled: false,
        ..JudgeSettings::default()
    };
    let output = evaluate_with_client(
        judge_client(&mock_server),
        &settings,
        None,
        std::path::Path::new("/work"),
        "git push --force",
    )
    .await
    .unwrap()
    .render_text();

    assert!(
        output.contains("Verdict: bypassed"),
        "bypass must render as bypassed, got:\n{output}"
    );
    assert!(
        output.contains("Observation: absent (no TypeSafe request)"),
        "a bypassed judge makes no observation, got:\n{output}"
    );
    mock_server.verify().await;
}

#[tokio::test]
async fn bash_check_bypass_env_value_skips_judge() {
    // The `CAKE_JUDGE=off` value flows through the full judge path. The value
    // is passed in (not read from the process env), so the test is hermetic
    // and cannot race other judge-path tests.
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(
            r#"{"verdict":"block","code":"git-force-push","message":"Prefer push --force-with-lease."}"#,
        )))
        .expect(0) // bypassed: no judge call may be made
        .mount(&mock_server)
        .await;

    let settings = JudgeSettings::default();
    let output = evaluate_with_client(
        judge_client(&mock_server),
        &settings,
        Some("off"),
        std::path::Path::new("/work"),
        "git push --force",
    )
    .await
    .unwrap()
    .render_text();

    assert!(
        output.contains("Verdict: bypassed"),
        "bypass must render as bypassed, got:\n{output}"
    );
    mock_server.verify().await;
}

#[test]
fn judge_is_enabled_respects_setting_and_env_bypass() {
    let enabled = JudgeSettings::default();
    // The env value is passed in so no test mutates the process-global
    // `CAKE_JUDGE` while parallel judge-path tests read it.
    assert!(judge_is_enabled(&enabled, None), "judge is on by default");
    assert!(
        judge_is_enabled(&enabled, Some("on")),
        "only CAKE_JUDGE=off bypasses"
    );
    assert!(
        !judge_is_enabled(&enabled, Some("off")),
        "CAKE_JUDGE=off disables even when settings enable the judge"
    );

    let disabled = JudgeSettings {
        enabled: false,
        ..JudgeSettings::default()
    };
    assert!(
        !judge_is_enabled(&disabled, None),
        "enabled = false disables"
    );
    assert!(!judge_is_enabled(&disabled, Some("off")));
}

#[tokio::test]
async fn bash_check_bypass_short_circuits_broken_judge_config() {
    // A disabled judge must not fail on unusable judge configuration (here:
    // no default model at all): the bypass is the recovery path when the
    // judge itself is broken.
    let mut settings = loaded_settings("https://example.com");
    settings.default_model = None;
    settings.judge = JudgeSettings {
        enabled: false,
        ..JudgeSettings::default()
    };

    let output = run_bash_check(&settings, std::path::Path::new("/work"), "git status", None)
        .await
        .unwrap()
        .render_text();
    assert!(
        output.contains("Verdict: bypassed"),
        "bypass must win over broken judge setup, got:\n{output}"
    );
}

#[tokio::test]
async fn bash_check_diagnostic_bypass_short_circuits_broken_judge_config() {
    // The `--diagnostic` runner must keep the bypass contract: a disabled
    // judge returns a bypass report without attempting judge setup, even when
    // the model configuration is unusable.
    let mut settings = loaded_settings("https://example.com");
    settings.default_model = None;
    settings.judge = JudgeSettings {
        enabled: false,
        ..JudgeSettings::default()
    };

    let report =
        run_bash_check_diagnostic(&settings, std::path::Path::new("/work"), "git status", None)
            .await
            .unwrap();
    assert!(
        report.report.contains("Verdict: bypassed"),
        "bypass must win over broken judge setup, got:\n{}",
        report.report
    );
    assert!(report.error.is_none());
}

// =============================================================================
// The TypeSafe observation report (issue #616)
// =============================================================================

/// One fabricated `TypeSafe` observation, so the classifier can be driven
/// without a provider call.
fn typesafe_observation(
    probability: Option<f32>,
    failure_class: Option<&'static str>,
    elapsed_ms: u64,
) -> TypeSafeObservation {
    TypeSafeObservation {
        elapsed: Duration::from_millis(elapsed_ms),
        model: Some("jev-1.13.0".to_string()),
        probability,
        usage_input_tokens: None,
        usage_output_tokens: None,
        failure_class,
    }
}

/// The judge's own `allow` outcome, for the cases the observation did not
/// decide.
fn judge_allow_outcome() -> JudgeOutcome {
    JudgeOutcome::Verdict {
        verdict: JudgeVerdict {
            decision: JudgeDecision::Allow,
            code: None,
            message: "Safe".to_string(),
            confidence: Some(0.9),
        },
        overridden: false,
    }
}

/// Assert the document's `data.observation` object, which every verdict carries
/// whether or not the cascade was armed (issue #616).
///
/// The probability is compared as the decimal the document prints, which is what
/// `data.probability` asserts elsewhere, rather than as the `f32` the pipeline
/// holds.
fn assert_observation(
    parsed: &serde_json::Value,
    outcome: &str,
    probability: Option<f64>,
    failure_class: Option<&str>,
) {
    let observation = &parsed["data"]["observation"];
    assert_eq!(observation["outcome"], outcome);
    assert_eq!(observation["probability"], serde_json::json!(probability));
    assert_eq!(
        observation["failure_class"],
        serde_json::json!(failure_class)
    );
}

/// Assert the document reports no `TypeSafe` request at all.
fn assert_absent_observation(parsed: &serde_json::Value) {
    assert_observation(parsed, "absent", None, None);
    assert_eq!(
        parsed["data"]["observation"]["elapsed_ms"],
        serde_json::Value::Null
    );
}

#[test]
fn classify_names_each_observation_outcome() {
    let clean = typesafe_observation(Some(0.82), None, 132);
    let failed = typesafe_observation(None, Some("timeout"), 100);

    let cases = [
        // No request at all: the mode is off, or the judge never ran.
        (
            TypeSafeMode::Off,
            judge_allow_outcome(),
            None,
            ObservationOutcome::Absent,
        ),
        (
            TypeSafeMode::Cascade,
            JudgeOutcome::Bypassed,
            None,
            ObservationOutcome::Absent,
        ),
        // An observation that carried no authority, and the two cascade
        // fallbacks the judge decided.
        (
            TypeSafeMode::Shadow,
            judge_allow_outcome(),
            Some(clean.clone()),
            ObservationOutcome::Shadow,
        ),
        (
            TypeSafeMode::Cascade,
            judge_allow_outcome(),
            Some(clean.clone()),
            ObservationOutcome::BelowCutoff,
        ),
        (
            TypeSafeMode::Cascade,
            judge_allow_outcome(),
            Some(failed),
            ObservationOutcome::Failed,
        ),
        (
            TypeSafeMode::Cascade,
            JudgeOutcome::FastApproved {
                probability: 0.91,
                elapsed: Duration::from_millis(250),
            },
            Some(clean),
            ObservationOutcome::Approved,
        ),
    ];

    for (mode, outcome, observation, expected) in cases {
        let report =
            ObservationReport::classify(observation.as_ref(), mode, is_fast_approval(&outcome));
        assert_eq!(report.outcome, expected, "{mode:?} {outcome:?}");
        assert_eq!(
            report.elapsed_ms.is_some(),
            observation.is_some(),
            "an elapsed time is reported exactly when a request was made"
        );
    }
}

#[test]
fn classify_reports_what_the_observation_measured() {
    assert_eq!(
        ObservationReport::classify(
            Some(&typesafe_observation(Some(0.82), None, 132)),
            TypeSafeMode::Cascade,
            false,
        ),
        ObservationReport {
            outcome: ObservationOutcome::BelowCutoff,
            probability: Some(0.82),
            failure_class: None,
            elapsed_ms: Some(132),
        }
    );
    assert_eq!(
        ObservationReport::classify(
            Some(&typesafe_observation(None, Some("timeout"), 100)),
            TypeSafeMode::Cascade,
            false,
        ),
        ObservationReport {
            outcome: ObservationOutcome::Failed,
            probability: None,
            failure_class: Some("timeout"),
            elapsed_ms: Some(100),
        }
    );
}

#[test]
fn render_verdict_omits_optional_lines_for_allow() {
    let verdict = JudgeVerdict {
        decision: JudgeDecision::Allow,
        code: None,
        message: "Safe".to_string(),
        confidence: None,
    };
    let output = render_verdict(
        &verdict,
        false,
        Duration::from_millis(1234),
        &ObservationReport::absent(),
    );
    assert_eq!(
        output,
        "Verdict: allow\nObservation: absent (no TypeSafe request)\nMessage: Safe\nLatency: 1.23s\n",
        "unexpected output shape:\n{output}"
    );
}

#[test]
fn render_json_reports_a_block_verdict_document() {
    let outcome = CheckOutcome {
        outcome: JudgeOutcome::Verdict {
            verdict: JudgeVerdict {
                decision: JudgeDecision::Block,
                code: Some("git-force-push".to_string()),
                message: "Prefer push --force-with-lease.".to_string(),
                confidence: Some(0.93),
            },
            overridden: false,
        },
        latency: Duration::from_millis(1234),
        observation: ObservationReport::absent(),
    };

    let rendered = outcome.render_json(&[]).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();

    assert_eq!(parsed["schema_version"], 1);
    assert_eq!(parsed["command"], "bash check");
    // The inspection completed, so a block verdict is not an error status.
    assert_eq!(parsed["status"], "ok");
    assert_eq!(parsed["summary"]["verdict"], "block");
    assert_eq!(parsed["checks"], serde_json::json!([]));
    assert_eq!(parsed["data"]["verdict"], "block");
    assert_eq!(parsed["data"]["code"], "git-force-push");
    assert_eq!(parsed["data"]["message"], "Prefer push --force-with-lease.");
    assert_eq!(parsed["data"]["confidence"], 0.93);
    assert_eq!(parsed["data"]["latency_ms"], 1234);
    assert_eq!(parsed["data"]["overridden"], false);
    assert_eq!(parsed["data"]["bypassed"], false);
    // The provenance fields are additive: a judge-decided document names the
    // judge stage and carries no fast-approval probability (ADR 034).
    assert_eq!(parsed["data"]["stage"], "judge");
    assert_eq!(parsed["data"]["probability"], serde_json::Value::Null);
    // A judge that decided without arming the cascade reports an absent
    // observation, which is what separates it from a fallback (issue #616).
    assert_absent_observation(&parsed);
}

#[test]
fn render_json_marks_an_allowlist_override() {
    let outcome = CheckOutcome {
        outcome: JudgeOutcome::Verdict {
            verdict: JudgeVerdict {
                decision: JudgeDecision::Block,
                code: Some("git-force-push".to_string()),
                message: "Prefer push --force-with-lease.".to_string(),
                confidence: None,
            },
            overridden: true,
        },
        latency: Duration::ZERO,
        observation: ObservationReport::absent(),
    };

    let rendered = outcome.render_json(&[]).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();

    // The document keeps the original verdict alongside the override flag, the
    // same way the text rendering does.
    assert_eq!(parsed["data"]["verdict"], "block");
    assert_eq!(parsed["data"]["overridden"], true);
    assert_eq!(parsed["data"]["confidence"], serde_json::Value::Null);
}

#[test]
fn render_json_reports_the_bypass_without_a_judge_call() {
    let bypassed = CheckOutcome::bypassed();

    let rendered = bypassed.render_json(&[]).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();

    assert_eq!(parsed["status"], "ok");
    assert_eq!(parsed["summary"]["verdict"], "bypassed");
    assert_eq!(parsed["data"]["bypassed"], true);
    assert_eq!(parsed["data"]["verdict"], serde_json::Value::Null);
    assert_eq!(parsed["data"]["latency_ms"], 0);
    // No stage decided a bypassed command, and no observation was made:
    // `absent` is the state a `mode = "off"` judge also reports.
    assert_eq!(parsed["data"]["stage"], serde_json::Value::Null);
    assert_eq!(parsed["data"]["probability"], serde_json::Value::Null);
    assert_absent_observation(&parsed);
    // Both output modes state the same reason for making no call, and both
    // name the observation the decision rests on (issue #616).
    assert_eq!(parsed["data"]["message"], JUDGE_BYPASS_MESSAGE);
    let text = bypassed.render_text();
    assert!(text.contains(JUDGE_BYPASS_MESSAGE));
    assert!(text.contains("Observation: absent (no TypeSafe request)"));
}

#[test]
fn render_json_carries_settings_findings_in_checks() {
    // A `--json` run reports an unrecognized settings key in the document rather
    // than on stderr, so the finding survives the machine-mode stderr silence.
    let checks = settings_warnings(&["unknown key 'tempurature' in settings.toml".to_string()]);

    let rendered = CheckOutcome::bypassed().render_json(&checks).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();

    assert_eq!(parsed["status"], "warning");
    assert_eq!(parsed["checks"][0]["id"], "settings.unknown_key");
    assert_eq!(
        parsed["checks"][0]["message"],
        "unknown key 'tempurature' in settings.toml"
    );
}

// =============================================================================
// The ADR 034 cascade in `cake bash check`
// =============================================================================

/// Cascade settings that arm the observation as an approval stage.
fn cascade_check_settings() -> JudgeSettings {
    JudgeSettings {
        typesafe: TypeSafeSettings {
            mode: TypeSafeMode::Cascade,
            model: "jev-1.13.0".to_string(),
            timeout_ms: 100,
        },
        ..JudgeSettings::default()
    }
}

/// Shadow settings: the observation runs and carries no authority.
fn shadow_check_settings() -> JudgeSettings {
    JudgeSettings {
        typesafe: TypeSafeSettings {
            mode: TypeSafeMode::Shadow,
            model: "jev-1.13.0".to_string(),
            timeout_ms: 100,
        },
        ..JudgeSettings::default()
    }
}

/// A `TypeSafe` client answering one mock server.
fn typesafe_check_client(server: &MockServer) -> TypeSafeClient {
    TypeSafeClient::new(
        "test-typesafe-key".to_string(),
        "jev-1.13.0".to_string(),
        Duration::from_millis(100),
    )
    .with_endpoint(server.uri())
}

/// One `TypeSafe` answer with `probability`.
fn typesafe_answer(probability: f64) -> serde_json::Value {
    serde_json::json!({
        "model": "jev-1.13.0",
        "answers": {"eligible": {"type": "noul", "noul": probability}},
        "usage": {"input_tokens": 3, "output_tokens": 1}
    })
}

#[test]
fn render_json_reports_a_fast_approval_as_allow_with_provenance() {
    let outcome = JudgeOutcome::FastApproved {
        probability: 0.91,
        elapsed: Duration::from_millis(250),
    };
    let outcome = CheckOutcome {
        observation: ObservationReport::classify(
            Some(&typesafe_observation(Some(0.91), None, 250)),
            TypeSafeMode::Cascade,
            true,
        ),
        outcome,
        latency: Duration::from_millis(250),
    };

    let rendered = outcome.render_json(&[]).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();

    // Consumers switching on the verdict see the allow a judge would have
    // produced; the provenance is additive.
    assert_eq!(parsed["summary"]["verdict"], "allow");
    assert_eq!(parsed["data"]["verdict"], "allow");
    assert_eq!(parsed["data"]["stage"], "typesafe");
    assert_eq!(parsed["data"]["probability"], 0.91);
    assert_eq!(parsed["data"]["latency_ms"], 250);
    assert_eq!(parsed["data"]["overridden"], false);
    assert_eq!(parsed["data"]["bypassed"], false);
    assert_eq!(parsed["data"]["code"], serde_json::Value::Null);
    assert_eq!(parsed["data"]["message"], FAST_APPROVAL_MESSAGE);
    // The observation the approval rests on is reported in full, including the
    // probability that cleared the cutoff (issue #616).
    assert_observation(&parsed, "approved", Some(0.91), None);
    assert_eq!(parsed["data"]["observation"]["elapsed_ms"], 250);
}

#[test]
fn render_outcome_reports_a_fast_approval_as_allow_with_its_stage() {
    let outcome = JudgeOutcome::FastApproved {
        probability: 0.91,
        elapsed: Duration::from_millis(250),
    };
    let observation = ObservationReport::classify(
        Some(&typesafe_observation(Some(0.91), None, 250)),
        TypeSafeMode::Cascade,
        true,
    );

    assert_eq!(
        render_outcome(&outcome, Duration::from_millis(250), &observation),
        format!(
            "Verdict: allow\nStage: typesafe\nProbability: 0.91\nObservation: approved (probability 0.91, 250ms)\nMessage: {FAST_APPROVAL_MESSAGE}\nLatency: 0.25s\n"
        )
    );
}

#[tokio::test]
async fn bash_check_fast_approves_without_a_judge_call() {
    let judge_server = MockServer::start().await;
    let typesafe_server = MockServer::start().await;
    // `expect(0)`: a fast approval must not reach the judge at all.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(
            r#"{"verdict":"block","code":"unknown-destructive","message":"No"}"#,
        )))
        .expect(0)
        .mount(&judge_server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(typesafe_answer(0.91)))
        .expect(1)
        .mount(&typesafe_server)
        .await;

    let outcome = evaluate_with_client(
        judge_client(&judge_server).with_typesafe(Some(typesafe_check_client(&typesafe_server))),
        &cascade_check_settings(),
        None,
        std::path::Path::new("/work"),
        "git status",
    )
    .await
    .unwrap();

    let rendered = outcome.render_json(&[]).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
    assert_eq!(parsed["status"], "ok");
    assert_eq!(parsed["summary"]["verdict"], "allow");
    assert_eq!(parsed["data"]["stage"], "typesafe");
    assert_eq!(parsed["data"]["probability"], 0.91);
    assert_eq!(parsed["data"]["observation"]["outcome"], "approved");
    let text = outcome.render_text();
    assert!(text.contains("Stage: typesafe"), "{text}");
    assert!(text.contains("Observation: approved"), "{text}");
    judge_server.verify().await;
    typesafe_server.verify().await;
}

#[tokio::test]
async fn bash_check_falls_back_to_the_judge_below_the_cutoff() {
    let judge_server = MockServer::start().await;
    let typesafe_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(chat_response(r#"{"verdict":"allow","message":"Safe"}"#)),
        )
        .expect(1)
        .mount(&judge_server)
        .await;
    // The measured band for `cat ~/.npmrc` over `independent-v2`; it falls back.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(typesafe_answer(0.82)))
        .expect(1)
        .mount(&typesafe_server)
        .await;

    let outcome = evaluate_with_client(
        judge_client(&judge_server).with_typesafe(Some(typesafe_check_client(&typesafe_server))),
        &cascade_check_settings(),
        None,
        std::path::Path::new("/work"),
        "cat ~/.npmrc",
    )
    .await
    .unwrap();

    let rendered = outcome.render_json(&[]).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
    // The judge decided, so the document reports the judge's stage and message
    // and carries no fast-approval probability.
    assert_eq!(parsed["summary"]["verdict"], "allow");
    assert_eq!(parsed["data"]["stage"], "judge");
    assert_eq!(parsed["data"]["probability"], serde_json::Value::Null);
    assert_eq!(parsed["data"]["message"], "Safe");
    // The armed cascade fell back because the observation came in below the
    // cutoff, which is now distinguishable from a cascade that never ran
    // (issue #616).
    assert_observation(&parsed, "below_cutoff", Some(0.82), None);
    let text = outcome.render_text();
    assert!(
        text.contains("Observation: below_cutoff (probability 0.82, "),
        "{text}"
    );
    judge_server.verify().await;
    typesafe_server.verify().await;
}

#[tokio::test]
async fn bash_check_reports_a_failed_observation_and_falls_back() {
    // The cascade is armed and the observation fails (here an HTTP error). The
    // judge decides, exactly as it does below the cutoff, and the two fallbacks
    // are told apart by the observation outcome (issue #616).
    let judge_server = MockServer::start().await;
    let typesafe_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(chat_response(r#"{"verdict":"allow","message":"Safe"}"#)),
        )
        .expect(1)
        .mount(&judge_server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&typesafe_server)
        .await;

    let outcome = evaluate_with_client(
        judge_client(&judge_server).with_typesafe(Some(typesafe_check_client(&typesafe_server))),
        &cascade_check_settings(),
        None,
        std::path::Path::new("/work"),
        "git status",
    )
    .await
    .unwrap();

    let rendered = outcome.render_json(&[]).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
    assert_eq!(parsed["data"]["stage"], "judge");
    assert_observation(&parsed, "failed", None, Some("http_error"));
    assert!(
        parsed["data"]["observation"]["elapsed_ms"].is_u64(),
        "a failed observation still reports its elapsed time: {rendered}"
    );
    let text = outcome.render_text();
    assert!(
        text.contains("Observation: failed (failure http_error, "),
        "{text}"
    );
    judge_server.verify().await;
    typesafe_server.verify().await;
}

#[tokio::test]
async fn bash_check_reports_a_shadow_observation_as_unauthoritative() {
    // `mode = "shadow"` observes beside the judge and decides nothing, even
    // when the probability clears the cutoff: the judge keeps its authority,
    // and the outcome vocabulary says so instead of reporting a fast approval.
    let judge_server = MockServer::start().await;
    let typesafe_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(chat_response(r#"{"verdict":"allow","message":"Safe"}"#)),
        )
        .expect(1)
        .mount(&judge_server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(typesafe_answer(0.95)))
        .expect(1)
        .mount(&typesafe_server)
        .await;

    let outcome = evaluate_with_client(
        judge_client(&judge_server).with_typesafe(Some(typesafe_check_client(&typesafe_server))),
        &shadow_check_settings(),
        None,
        std::path::Path::new("/work"),
        "git status",
    )
    .await
    .unwrap();

    let rendered = outcome.render_json(&[]).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
    assert_eq!(parsed["data"]["stage"], "judge");
    assert_eq!(parsed["data"]["message"], "Safe");
    assert_observation(&parsed, "shadow", Some(0.95), None);
    let text = outcome.render_text();
    assert!(
        text.contains("Observation: shadow (probability 0.95, "),
        "{text}"
    );
    judge_server.verify().await;
    typesafe_server.verify().await;
}

#[tokio::test]
async fn bash_check_reports_an_absent_observation_when_the_cascade_is_not_armed() {
    // `mode` is unset, so it defaults to `off`: no `TypeSafe` request is made,
    // and the document says so rather than leaving the fallback to be inferred
    // from a null probability (issue #616).
    let judge_server = MockServer::start().await;
    let typesafe_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(chat_response(r#"{"verdict":"allow","message":"Safe"}"#)),
        )
        .expect(1)
        .mount(&judge_server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(typesafe_answer(0.95)))
        .expect(0) // off makes no request, even with a client configured
        .mount(&typesafe_server)
        .await;

    let outcome = evaluate_with_client(
        judge_client(&judge_server).with_typesafe(Some(typesafe_check_client(&typesafe_server))),
        &JudgeSettings::default(),
        None,
        std::path::Path::new("/work"),
        "git status",
    )
    .await
    .unwrap();

    let rendered = outcome.render_json(&[]).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
    assert_eq!(parsed["data"]["stage"], "judge");
    assert_absent_observation(&parsed);
    let text = outcome.render_text();
    assert!(
        text.contains("Observation: absent (no TypeSafe request)"),
        "{text}"
    );
    judge_server.verify().await;
    typesafe_server.verify().await;
}

#[tokio::test]
async fn bash_check_diagnostic_reports_a_failed_observation_beside_a_judge_error() {
    // The compound failure the report is read for: the observation failed and so
    // did the judge. `--diagnostic` renders both, because nothing else reports
    // the observation on a fail-closed path (issue #616).
    let judge_server = MockServer::start().await;
    let typesafe_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&judge_server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&typesafe_server)
        .await;

    let report = evaluate_with_client_diagnostic(
        judge_client(&judge_server).with_typesafe(Some(typesafe_check_client(&typesafe_server))),
        &cascade_check_settings(),
        None,
        std::path::Path::new("/work"),
        "git status",
    )
    .await
    .unwrap();

    assert!(report.error.is_some(), "the judge call failed closed");
    let observation = report
        .report
        .find("Observation: failed (failure http_error, ")
        .expect("the report must name the failed observation");
    let error = report
        .report
        .find("Judge error: ")
        .expect("the report must carry the judge error");
    assert!(
        observation < error,
        "the observation precedes the error:\n{}",
        report.report
    );
}

#[tokio::test]
async fn bash_check_diagnostic_reports_the_fast_leg_latency() {
    // A fast approval has no judge attempt, so `--diagnostic` has no raw request
    // to show. It must still render the outcome, with the fast leg's own elapsed
    // time instead of a zero latency.
    let judge_server = MockServer::start().await;
    let typesafe_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(chat_response(r#"{"verdict":"allow","message":"Safe"}"#)),
        )
        .expect(0)
        .mount(&judge_server)
        .await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(typesafe_answer(0.97))
                .set_delay(Duration::from_millis(60)),
        )
        .expect(1)
        .mount(&typesafe_server)
        .await;

    let report = evaluate_with_client_diagnostic(
        judge_client(&judge_server).with_typesafe(Some(typesafe_check_client(&typesafe_server))),
        &cascade_check_settings(),
        None,
        std::path::Path::new("/work"),
        "git status",
    )
    .await
    .unwrap();

    assert!(report.error.is_none());
    assert!(
        report.report.contains("Stage: typesafe"),
        "{}",
        report.report
    );
    assert!(
        !report.report.contains("Latency: 0.00s"),
        "the fast leg's elapsed time must be reported:\n{}",
        report.report
    );
    // `--diagnostic` reports the observation's bounded summary, not the raw
    // `TypeSafe` request or response (issue #616).
    assert!(
        report
            .report
            .contains("Observation: approved (probability 0.97, "),
        "{}",
        report.report
    );
    judge_server.verify().await;
}

// =============================================================================
// Judge model resolution
// =============================================================================

#[test]
fn resolve_judge_model_uses_judge_override_when_set() {
    let mut settings = loaded_settings("https://override.example.com/v1");
    settings.judge.model = Some("zen".to_string());
    settings.default_model = None;

    let resolved = temp_env::with_var("JUDGE_TEST_KEY", Some("test-key"), || {
        resolve_judge_model(&settings, None)
    })
    .unwrap();
    assert_eq!(resolved.model_config.model, "provider/zen");
    assert_eq!(
        resolved.model_config.base_url,
        "https://override.example.com/v1"
    );
}

#[test]
fn resolve_judge_model_honors_cli_model_flag() {
    let mut settings = loaded_settings("https://default.example.com/v1");
    settings.models.insert(
        "alt".to_string(),
        model_definition("alt", "https://alt.example.com/v1"),
    );
    let resolved = temp_env::with_var("JUDGE_TEST_KEY", Some("test-key"), || {
        resolve_judge_model(&settings, Some("alt"))
    })
    .unwrap();
    assert_eq!(resolved.model_config.model, "provider/alt");
}

#[test]
fn resolve_judge_model_judge_setting_beats_cli_model() {
    let mut settings = loaded_settings("https://default.example.com/v1");
    settings.judge.model = Some("zen".to_string());
    settings.models.insert(
        "alt".to_string(),
        model_definition("alt", "https://alt.example.com/v1"),
    );
    let resolved = temp_env::with_var("JUDGE_TEST_KEY", Some("test-key"), || {
        resolve_judge_model(&settings, Some("alt"))
    })
    .unwrap();
    assert_eq!(resolved.model_config.model, "provider/zen");
}

#[test]
fn resolve_judge_model_falls_back_to_default_model() {
    let settings = loaded_settings("https://default.example.com/v1");
    let resolved = temp_env::with_var("JUDGE_TEST_KEY", Some("test-key"), || {
        resolve_judge_model(&settings, None)
    })
    .unwrap();
    assert_eq!(resolved.model_config.model, "provider/zen");
}

#[test]
fn resolve_judge_model_errors_without_any_model() {
    let mut settings = loaded_settings("https://default.example.com/v1");
    settings.default_model = None;
    let error = resolve_judge_model(&settings, None).unwrap_err();
    assert!(error.to_string().contains("No model specified"));
}
