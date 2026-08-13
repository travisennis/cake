use super::*;
use crate::clients::judge::{JudgeDecision, JudgeError, judge_is_enabled};
use crate::config::ModelDefinition;
use crate::config::model::ApiType;
use crate::config::settings::{JudgeSettings, SandboxSettings, SkillSettings};
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
    JudgeClient::new(test_config(mock_server.uri()), Duration::from_secs(5))
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
        system_prompt: None,
        judge: JudgeSettings::default(),
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
    let client = JudgeClient::new(config, Duration::from_secs(5));

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
    let client = JudgeClient::new(config, Duration::from_secs(5));

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
    .unwrap();

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
    .unwrap();

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
    .unwrap();

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
    .unwrap();

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
    .unwrap();

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
    .unwrap();

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
    .unwrap();

    assert!(
        output.contains("Verdict: bypassed"),
        "bypass must render as bypassed, got:\n{output}"
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
    .unwrap();

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
        .unwrap();
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

#[test]
fn render_verdict_omits_optional_lines_for_allow() {
    let verdict = JudgeVerdict {
        decision: JudgeDecision::Allow,
        code: None,
        message: "Safe".to_string(),
        confidence: None,
    };
    let output = render_verdict(&verdict, false, Duration::from_millis(1234));
    assert_eq!(
        output, "Verdict: allow\nMessage: Safe\nLatency: 1.23s\n",
        "unexpected output shape:\n{output}"
    );
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
