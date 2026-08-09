use super::*;
use crate::clients::judge::JudgeDecision;
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
        std::path::Path::new("/work"),
        "git status",
    )
    .await;
    assert!(
        result.is_err(),
        "a judge failure must surface as an error (nonzero exit), not a verdict"
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
    let output = render_verdict(&verdict, Duration::from_millis(1234));
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
