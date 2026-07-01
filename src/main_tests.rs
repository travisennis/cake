use super::*;
use crate::types::Role;

use crate::config::model::ApiType;
use crate::types::session::FunctionCallOutputData;

fn test_resolved_model_config() -> ResolvedModelConfig {
    ResolvedModelConfig {
        model_config: ModelConfig {
            model: "test-model".to_string(),
            api_type: ApiType::ChatCompletions,
            base_url: "https://api.example.com".to_string(),
            api_key_env: "TEST_API_KEY".to_string(),
            provider: None,
            provider_headers: None,
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            reasoning_effort: None,
            reasoning_summary: None,
            reasoning_max_tokens: None,
            providers: vec![],
        },
        api_key: "test-key".to_string(),
    }
}

fn session_with_skill_records() -> Session {
    let mut session = Session::new(
        uuid::uuid!("550e8400-e29b-41d4-a716-446655440000"),
        PathBuf::from("/work"),
    );
    session.records = vec![
        SessionRecord::FunctionCallOutput(FunctionCallOutputData {
            call_id: "call-1".to_string(),
            output: "echoed text: Skill 'fake-skill' activated".to_string(),
            timestamp: None,
        }),
        SessionRecord::SkillActivated {
            session_id: session.id.to_string(),
            task_id: "task-1".to_string(),
            timestamp: chrono::Utc::now(),
            name: "real-skill".to_string(),
            path: PathBuf::from("/work/.agents/skills/real-skill/SKILL.md"),
        },
    ];
    session
}

#[test]
fn test_cli_parsing_positional_prompt() {
    let args = CodingAssistant::parse_from(["cake", "test prompt"]);
    assert_eq!(args.prompt, Some("test prompt".to_string()));
}

#[test]
fn test_cli_parsing_dash_for_stdin() {
    let args = CodingAssistant::parse_from(["cake", "-"]);
    assert_eq!(args.prompt, Some("-".to_string()));
}

#[test]
fn test_cli_parsing_no_prompt() {
    let args = CodingAssistant::parse_from(["cake"]);
    assert_eq!(args.prompt, None);
}

#[test]
fn test_cli_parsing_model_flag() {
    let args = CodingAssistant::parse_from(["cake", "--model", "claude", "test prompt"]);
    assert_eq!(args.model, Some("claude".to_string()));
    assert_eq!(args.prompt, Some("test prompt".to_string()));
}

#[test]
fn test_cli_parsing_model_flag_without_prompt() {
    let args = CodingAssistant::parse_from(["cake", "--model", "deepseek"]);
    assert_eq!(args.model, Some("deepseek".to_string()));
    assert_eq!(args.prompt, None);
}

#[test]
fn test_cli_parsing_no_model_flag() {
    let args = CodingAssistant::parse_from(["cake", "test prompt"]);
    assert_eq!(args.model, None);
}

#[test]
fn test_cli_parsing_reasoning_effort() {
    let args = CodingAssistant::parse_from(["cake", "--reasoning-effort", "xhigh", "test prompt"]);
    assert_eq!(args.reasoning_effort, Some(ReasoningEffort::Xhigh));
}

#[test]
fn test_cli_rejects_invalid_reasoning_effort() {
    let result = CodingAssistant::try_parse_from(["cake", "--reasoning-effort", "maximum", "test"]);
    assert!(result.is_err());
}

#[test]
fn test_cli_parsing_profile_flag() {
    let args = CodingAssistant::parse_from(["cake", "--profile", "review", "test prompt"]);
    assert_eq!(args.profile, Some("review".to_string()));
    assert_eq!(args.prompt, Some("test prompt".to_string()));
}

#[test]
fn test_cli_parsing_no_session() {
    let args = CodingAssistant::parse_from(["cake", "--no-session", "test prompt"]);
    assert!(args.no_session);
}

#[test]
fn test_cli_parsing_no_session_defaults_false() {
    let args = CodingAssistant::parse_from(["cake", "test prompt"]);
    assert!(!args.no_session);
}

#[test]
fn test_run_mode_defaults_to_new_session() {
    let args = CodingAssistant::parse_from(["cake", "test prompt"]);
    assert_eq!(RunMode::from_cli(&args).unwrap(), RunMode::NewSession);
}

#[test]
fn test_run_mode_no_session_is_ephemeral() {
    let args = CodingAssistant::parse_from(["cake", "--no-session", "test prompt"]);
    assert_eq!(RunMode::from_cli(&args).unwrap(), RunMode::Ephemeral);
    assert!(!RunMode::from_cli(&args).unwrap().persists_session());
}

#[test]
fn test_run_mode_restore_flags() {
    let resume_id = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    let args = CodingAssistant::parse_from(["cake", "--continue", "test prompt"]);
    assert_eq!(RunMode::from_cli(&args).unwrap(), RunMode::ContinueLatest);

    let args = CodingAssistant::parse_from([
        "cake",
        "--resume",
        "550e8400-e29b-41d4-a716-446655440000",
        "test prompt",
    ]);
    assert_eq!(
        RunMode::from_cli(&args).unwrap(),
        RunMode::Resume {
            session_id: resume_id
        }
    );

    let args = CodingAssistant::parse_from(["cake", "--fork"]);
    assert_eq!(RunMode::from_cli(&args).unwrap(), RunMode::ForkLatest);

    let args = CodingAssistant::parse_from([
        "cake",
        "--fork",
        "550e8400-e29b-41d4-a716-446655440000",
        "test prompt",
    ]);
    assert_eq!(
        RunMode::from_cli(&args).unwrap(),
        RunMode::Fork {
            session_id: resume_id
        }
    );
}

#[test]
fn test_run_mode_rejects_non_uuid_session_references() {
    let args = CodingAssistant::parse_from(["cake", "--resume", "not-a-uuid", "test prompt"]);
    assert!(
        RunMode::from_cli(&args)
            .unwrap_err()
            .to_string()
            .contains("Invalid session UUID")
    );

    let args = CodingAssistant::parse_from(["cake", "--fork", "not-a-uuid", "test prompt"]);
    assert!(
        RunMode::from_cli(&args)
            .unwrap_err()
            .to_string()
            .contains("Invalid session UUID")
    );
}

#[test]
fn test_cli_parsing_add_dir_single() {
    let args = CodingAssistant::parse_from(["cake", "--add-dir", "/path/to/dir", "test prompt"]);
    assert_eq!(args.add_dir, vec!["/path/to/dir"]);
    assert_eq!(args.prompt, Some("test prompt".to_string()));
}

#[test]
fn test_cli_parsing_add_dir_multiple() {
    let args = CodingAssistant::parse_from([
        "cake",
        "--add-dir",
        "/path/to/dir1",
        "--add-dir",
        "/path/to/dir2",
        "test prompt",
    ]);
    assert_eq!(args.add_dir, vec!["/path/to/dir1", "/path/to/dir2"]);
}

#[test]
fn test_cli_parsing_add_dir_none() {
    let args = CodingAssistant::parse_from(["cake", "test prompt"]);
    assert!(args.add_dir.is_empty());
}

#[test]
fn test_resolve_additional_dirs_relative_becomes_absolute() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sub = dir.path().join("mydir");
    std::fs::create_dir(&sub).expect("create subdir");

    let assistant = CodingAssistant::parse_from(["cake", "--add-dir", "mydir", "test prompt"]);
    let resolved = assistant.resolve_additional_dirs(dir.path());
    assert_eq!(resolved.len(), 1);
    let expected = std::fs::canonicalize(&sub).expect("canonicalize");
    assert_eq!(resolved[0], expected);
}

#[test]
fn test_resolve_additional_dirs_absolute_stays_absolute() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).expect("create subdir");

    // Use the canonicalized absolute path as the --add-dir argument
    let abs_path = std::fs::canonicalize(&sub).expect("canonicalize");
    let assistant = CodingAssistant::parse_from([
        "cake",
        "--add-dir",
        &abs_path.to_string_lossy(),
        "test prompt",
    ]);
    let resolved = assistant.resolve_additional_dirs(dir.path());
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0], abs_path);
}

#[test]
fn test_resolve_additional_dirs_non_existent_filtered() {
    let dir = tempfile::tempdir().expect("tempdir");

    let assistant =
        CodingAssistant::parse_from(["cake", "--add-dir", "nonexistent", "test prompt"]);
    let resolved = assistant.resolve_additional_dirs(dir.path());
    assert!(resolved.is_empty());
}

#[test]
fn test_cli_parsing_no_skills() {
    let args = CodingAssistant::parse_from(["cake", "--no-skills", "test prompt"]);
    assert!(args.no_skills);
    assert!(args.skills.is_none());
}

#[test]
fn test_cli_parsing_skills_filter() {
    let args = CodingAssistant::parse_from(["cake", "--skills", "debugging,review", "test prompt"]);
    assert!(!args.no_skills);
    assert_eq!(args.skills, Some("debugging,review".to_string()));
}

#[test]
fn test_cli_parsing_skills_defaults() {
    let args = CodingAssistant::parse_from(["cake", "test prompt"]);
    assert!(!args.no_skills);
    assert!(args.skills.is_none());
}

#[test]
fn test_resolve_model_config_no_model_configured() {
    let args = CodingAssistant::parse_from(["cake", "test prompt"]);
    let models = HashMap::new();
    let result = args.resolve_model_config(&models, None);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("No model specified"));
    assert!(err.contains("settings.toml"));
}

#[test]
fn test_resolve_model_config_default_model() {
    let args = CodingAssistant::parse_from(["cake", "test prompt"]);
    let mut models = HashMap::new();
    models.insert(
        "zen".to_string(),
        ModelDefinition {
            name: "zen".to_string(),
            model: "glm-5.1".to_string(),
            base_url: "https://opencode.ai/zen/go/v1/".to_string(),
            api_key_env: "OPENCODE_ZEN_API_TOKEN".to_string(),
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
        },
    );

    let config = args.resolve_model_config(&models, Some("zen")).unwrap();
    assert_eq!(config.model, "glm-5.1");
}

#[test]
fn test_resolve_model_config_unknown_model() {
    let mut args = CodingAssistant::parse_from(["cake", "test prompt"]);
    args.model = Some("nonexistent".to_string());

    let models = HashMap::new();
    let result = args.resolve_model_config(&models, None);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("Unknown model 'nonexistent'"));
}

#[test]
fn test_resolve_model_config_invalid_name_format() {
    let mut args = CodingAssistant::parse_from(["cake", "test prompt"]);
    args.model = Some("Invalid Name!".to_string());

    let models = HashMap::new();
    let result = args.resolve_model_config(&models, None);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("Invalid model name 'Invalid Name!'"));
}

#[test]
fn test_resolve_model_config_from_settings() {
    let args = CodingAssistant::parse_from(["cake", "--model", "claude", "test"]);

    let mut models = HashMap::new();
    models.insert(
        "claude".to_string(),
        ModelDefinition {
            name: "claude".to_string(),
            model: "anthropic/claude-3-sonnet".to_string(),
            base_url: "https://openrouter.ai/api/v1/".to_string(),
            api_key_env: "OPENROUTER_API_KEY".to_string(),
            provider: None,
            provider_headers: None,
            api_type: ApiType::Responses,
            temperature: Some(0.7),
            top_p: Some(0.9),
            max_output_tokens: Some(8000),
            reasoning_effort: None,
            reasoning_summary: None,
            reasoning_max_tokens: None,
            providers: vec![],
        },
    );

    let config = args.resolve_model_config(&models, None).unwrap();
    assert_eq!(config.model, "anthropic/claude-3-sonnet");
    assert_eq!(config.api_type, ApiType::Responses);
    assert_eq!(config.temperature, Some(0.7));
    assert_eq!(config.top_p, Some(0.9));
}

#[test]
fn test_resolve_model_config_model_flag_overrides_default_model() {
    let args = CodingAssistant::parse_from(["cake", "--model", "claude", "test"]);

    let mut models = HashMap::new();
    models.insert(
        "zen".to_string(),
        ModelDefinition {
            name: "zen".to_string(),
            model: "glm-5.1".to_string(),
            base_url: "https://example.com".to_string(),
            api_key_env: "KEY".to_string(),
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
        },
    );
    models.insert(
        "claude".to_string(),
        ModelDefinition {
            name: "claude".to_string(),
            model: "anthropic/claude-3-sonnet".to_string(),
            base_url: "https://openrouter.ai/api/v1/".to_string(),
            api_key_env: "OPENROUTER_API_KEY".to_string(),
            provider: None,
            provider_headers: None,
            api_type: ApiType::Responses,
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            reasoning_effort: None,
            reasoning_summary: None,
            reasoning_max_tokens: None,
            providers: vec![],
        },
    );

    let config = args.resolve_model_config(&models, Some("zen")).unwrap();
    assert_eq!(config.model, "anthropic/claude-3-sonnet");
}

#[test]
fn test_build_content_prompt_only() {
    let result = CodingAssistant::build_content(Some("hello"), None);
    assert_eq!(result.unwrap(), "hello");
}

#[test]
fn test_build_content_stdin_only() {
    let result = CodingAssistant::build_content(None, Some("stdin content".to_string()));
    assert_eq!(result.unwrap(), "stdin content");
}

#[test]
fn test_build_content_dash_with_stdin() {
    let result = CodingAssistant::build_content(Some("-"), Some("stdin content".to_string()));
    assert_eq!(result.unwrap(), "stdin content");
}

#[test]
fn test_build_content_dash_without_stdin() {
    let result = CodingAssistant::build_content(Some("-"), None);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("No input provided via stdin")
    );
}

#[test]
fn test_build_content_prompt_and_stdin() {
    let result =
        CodingAssistant::build_content(Some("instructions"), Some("file content".to_string()));
    assert_eq!(
        result.unwrap(),
        "User request:\ninstructions\n\nStdin:\nfile content"
    );
}

#[test]
fn restored_session_seeds_skills_from_structured_records_only() {
    let run_session = CodingAssistant::restored_client_and_session(
        session_with_skill_records(),
        test_resolved_model_config(),
        &[(Role::System, "system".to_string())],
        &HashMap::new(),
        Arc::new(ToolContext::from_current_process()),
        uuid::uuid!("550e8400-e29b-41d4-a716-446655440001"),
    )
    .unwrap();

    let activated = run_session.agent.test_active_skills();
    assert!(activated.contains("real-skill"));
    assert!(!activated.contains("fake-skill"));
}

#[test]
fn forked_session_seeds_skills_from_structured_records() {
    let restored = session_with_skill_records();
    let run_session = CodingAssistant::forked_client_and_session(
        &restored,
        test_resolved_model_config(),
        PathBuf::from("/work"),
        &[(Role::System, "system".to_string())],
        HashMap::new(),
        Arc::new(ToolContext::from_current_process()),
        uuid::uuid!("550e8400-e29b-41d4-a716-446655440001"),
    )
    .unwrap();

    assert!(
        run_session
            .agent
            .test_active_skills()
            .contains("real-skill")
    );
    assert!(matches!(run_session.storage, SessionStorage::New));
    let seed_records = run_session
        .seed_records
        .as_ref()
        .expect("fork should produce seed records");
    assert!(seed_records.iter().any(|record| matches!(
        record,
        SessionRecord::SkillActivated { name, .. }
            if name == "real-skill"
    )));
}

#[test]
fn test_build_content_no_input() {
    let result = CodingAssistant::build_content(None, None);
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("No input provided"));
    assert!(err_msg.contains("cake -"));
}

#[test]
fn test_build_content_empty_prompt() {
    let result = CodingAssistant::build_content(Some(""), None);
    assert_eq!(result.unwrap(), "");
}

#[test]
fn test_build_content_empty_stdin() {
    let result = CodingAssistant::build_content(None, Some(String::new()));
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("No input provided")
    );
}

#[test]
fn test_build_content_prompt_with_empty_stdin() {
    let result = CodingAssistant::build_content(Some("my prompt"), Some(String::new()));
    assert_eq!(result.unwrap(), "my prompt");
}

#[test]
fn test_build_content_multiline_prompt() {
    let result = CodingAssistant::build_content(Some("line 1\nline 2"), None);
    assert_eq!(result.unwrap(), "line 1\nline 2");
}

#[test]
fn test_build_content_multiline_stdin() {
    let result =
        CodingAssistant::build_content(None, Some("stdin line 1\nstdin line 2".to_string()));
    assert_eq!(result.unwrap(), "stdin line 1\nstdin line 2");
}

#[test]
fn test_build_content_multiline_both() {
    let result = CodingAssistant::build_content(
        Some("prompt line 1\nprompt line 2"),
        Some("stdin line 1\nstdin line 2".to_string()),
    );
    assert_eq!(
        result.unwrap(),
        "User request:\nprompt line 1\nprompt line 2\n\nStdin:\nstdin line 1\nstdin line 2"
    );
}

#[test]
fn output_sink_builds_success_json() {
    temp_env::with_var("CAKE_TEST_VALID_KEY", Some("sk-test-123"), || {
        let agent = Agent::new(
            test_resolved_model_config(),
            &[(Role::System, "test system prompt".to_string())],
        )
        .with_session_id(uuid::uuid!("550e8400-e29b-41d4-a716-446655440000"))
        .with_turn_count(2)
        .with_total_usage(crate::types::Usage {
            input_tokens: 12,
            output_tokens: 8,
            ..Default::default()
        });
        let session = Session::new(agent.session_id(), PathBuf::from("/work"));
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(err) => panic!("temp dir should be created: {err}"),
        };
        let data_dir = match temp_env::with_var("CAKE_DATA_DIR", Some(dir.path()), DataDir::new) {
            Ok(data_dir) => data_dir,
            Err(err) => panic!("data dir should be created: {err}"),
        };
        let result = Ok("done".to_string());

        let json = CliOutputSink::turn_result_json(
            &result,
            1500,
            &agent,
            Path::new("/work"),
            &data_dir,
            &session,
            true,
        );

        assert_eq!(json["result"], "done");
        assert_eq!(json["session_id"], agent.session_id().to_string());
        assert_eq!(json["turns"], 2);
        assert_eq!(json["elapsed_time"], 1500);
        assert_eq!(json["usage"]["input_tokens"], 12);
        assert!(json.get("error").is_none());
        assert!(json["session_file"].is_string());
    });
}

#[test]
fn output_sink_builds_error_json() {
    temp_env::with_var("CAKE_TEST_VALID_KEY", Some("sk-test-123"), || {
        let agent = Agent::new(
            test_resolved_model_config(),
            &[(Role::System, "test system prompt".to_string())],
        )
        .with_session_id(uuid::uuid!("550e8400-e29b-41d4-a716-446655440000"));
        let session = Session::new(agent.session_id(), PathBuf::from("/work"));
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(err) => panic!("temp dir should be created: {err}"),
        };
        let data_dir = match temp_env::with_var("CAKE_DATA_DIR", Some(dir.path()), DataDir::new) {
            Ok(data_dir) => data_dir,
            Err(err) => panic!("data dir should be created: {err}"),
        };
        let result = Err(anyhow::anyhow!("provider failed"));

        let json = CliOutputSink::turn_result_json(
            &result,
            250,
            &agent,
            Path::new("/work"),
            &data_dir,
            &session,
            true,
        );

        assert_eq!(json["result"], serde_json::Value::Null);
        assert_eq!(json["error"], "provider failed");
        assert_eq!(json["elapsed_time"], 250);
        assert!(json["session_file"].is_string());
    });
}

#[test]
fn output_sink_no_session_suppresses_session_file() {
    temp_env::with_var("CAKE_TEST_VALID_KEY", Some("sk-test-123"), || {
        let agent = Agent::new(
            test_resolved_model_config(),
            &[(Role::System, "test system prompt".to_string())],
        )
        .with_session_id(uuid::uuid!("550e8400-e29b-41d4-a716-446655440000"));
        let session = Session::new(agent.session_id(), PathBuf::from("/work"));
        let dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(err) => panic!("temp dir should be created: {err}"),
        };
        let data_dir = match temp_env::with_var("CAKE_DATA_DIR", Some(dir.path()), DataDir::new) {
            Ok(data_dir) => data_dir,
            Err(err) => panic!("data dir should be created: {err}"),
        };
        let result = Ok("ephemeral result".to_string());

        let json = CliOutputSink::turn_result_json(
            &result,
            300,
            &agent,
            Path::new("/work"),
            &data_dir,
            &session,
            false,
        );

        assert_eq!(json["result"], "ephemeral result");
        assert_eq!(json["session_id"], agent.session_id().to_string());
        assert!(
            json["session_file"].is_null(),
            "session_file should be null when session persistence is disabled"
        );
        assert_eq!(json["elapsed_time"], 300);
    });
}

#[test]
fn test_resolve_model_for_session_by_model_field() {
    // Session stores the API model identifier, not the config name.
    // This test verifies that --continue works when the session model
    // matches a definition's `model` field even if the `name` differs.
    temp_env::with_var("CAKE_TEST_VALID_KEY", Some("sk-test-123"), || {
        let args = CodingAssistant::parse_from(["cake", "test prompt"]);

        let mut models = HashMap::new();
        models.insert(
            "my-alias".to_string(),
            ModelDefinition {
                name: "my-alias".to_string(),
                model: "deepseek-v4-pro".to_string(),
                base_url: "https://api.example.com".to_string(),
                api_key_env: "CAKE_TEST_VALID_KEY".to_string(),
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
            },
        );

        let resolved = args
            .resolve_model_for_session(&models, None, Some("deepseek-v4-pro"))
            .unwrap();
        assert_eq!(resolved.model_config.model, "deepseek-v4-pro");
    });
}

// -------------------------------------------------------------------------
// handle_agent_turn_result — post-send success/error/hook branching
// -------------------------------------------------------------------------

fn test_agent_for_turn() -> Agent {
    Agent::new(
        test_resolved_model_config(),
        &[(Role::System, "test system prompt".to_string())],
    )
}

fn test_hook_runner() -> std::sync::Arc<HookRunner> {
    std::sync::Arc::new(HookRunner::new(
        crate::config::hooks::LoadedHooks::default(),
        HookContext {
            session_id: uuid::Uuid::new_v4(),
            task_id: uuid::Uuid::new_v4(),
            transcript_path: None,
            session_writer: None,
            hook_event_sink: None,
            cwd: std::env::temp_dir(),
            model: "test-model".to_string(),
        },
    ))
}

#[tokio::test]
async fn handle_agent_turn_success_no_hooks() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let captured_clone = captured.clone();
    let mut agent = test_agent_for_turn().with_streaming_json(move |json| {
        *captured_clone.lock().unwrap() = json.to_string();
    });

    let result: Result<String, anyhow::Error> = Ok("test response".to_string());

    CodingAssistant::handle_agent_turn_result(&mut agent, None, &result, 100)
        .await
        .unwrap();

    let json: serde_json::Value = serde_json::from_str(&captured.lock().unwrap()).unwrap();
    assert_eq!(json["type"], "task_complete");
    assert_eq!(json["subtype"], "success");
    assert_eq!(json["duration_ms"], 100);
    assert_eq!(json["is_error"], false);
}

#[tokio::test]
async fn handle_agent_turn_success_with_hooks() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let captured_clone = captured.clone();
    let mut agent = test_agent_for_turn().with_streaming_json(move |json| {
        *captured_clone.lock().unwrap() = json.to_string();
    });

    let runner = test_hook_runner();
    let result: Result<String, anyhow::Error> = Ok("test response".to_string());

    CodingAssistant::handle_agent_turn_result(&mut agent, Some(&runner), &result, 100)
        .await
        .unwrap();

    let json: serde_json::Value = serde_json::from_str(&captured.lock().unwrap()).unwrap();
    assert_eq!(json["subtype"], "success");
}

#[tokio::test]
async fn handle_agent_turn_error_no_hooks() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let captured_clone = captured.clone();
    let mut agent = test_agent_for_turn().with_streaming_json(move |json| {
        *captured_clone.lock().unwrap() = json.to_string();
    });

    let result: Result<String, anyhow::Error> = Err(anyhow::anyhow!("test error"));

    CodingAssistant::handle_agent_turn_result(&mut agent, None, &result, 50)
        .await
        .unwrap();

    let json: serde_json::Value = serde_json::from_str(&captured.lock().unwrap()).unwrap();
    assert_eq!(json["subtype"], "error_during_execution");
    assert_eq!(json["error"], "test error");
    assert_eq!(json["is_error"], true);
}

#[tokio::test]
async fn handle_agent_turn_error_with_hooks() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let captured_clone = captured.clone();
    let mut agent = test_agent_for_turn().with_streaming_json(move |json| {
        *captured_clone.lock().unwrap() = json.to_string();
    });

    let runner = test_hook_runner();
    let result: Result<String, anyhow::Error> = Err(anyhow::anyhow!("test error"));

    CodingAssistant::handle_agent_turn_result(&mut agent, Some(&runner), &result, 50)
        .await
        .unwrap();

    let json: serde_json::Value = serde_json::from_str(&captured.lock().unwrap()).unwrap();
    assert_eq!(json["subtype"], "error_during_execution");
    assert_eq!(json["error"], "test error");
}

#[tokio::test]
#[cfg(unix)]
async fn handle_agent_turn_success_with_stop_context() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let captured_clone = captured.clone();
    let mut agent = test_agent_for_turn().with_streaming_json(move |json| {
        *captured_clone.lock().unwrap() = json.to_string();
    });

    let cwd = std::env::temp_dir();
    let command = crate::config::hooks::HookCommand {
        command: r#"printf '{"additional_context":"stop context"}'"#.to_string(),
        timeout: std::time::Duration::from_secs(2),
        fail_closed: false,
        status_message: None,
        source_path: cwd.join("hooks.json"),
    };
    let loaded = crate::config::hooks::LoadedHooks {
        groups: vec![crate::config::hooks::HookGroup {
            event: crate::config::hooks::HookEvent::Stop,
            matcher: crate::config::hooks::HookMatcher::All,
            hooks: vec![command],
        }],
    };
    let runner = std::sync::Arc::new(HookRunner::new(
        loaded,
        HookContext {
            session_id: uuid::Uuid::new_v4(),
            task_id: uuid::Uuid::new_v4(),
            transcript_path: None,
            session_writer: None,
            hook_event_sink: None,
            cwd,
            model: "test-model".to_string(),
        },
    ));

    let result: Result<String, anyhow::Error> = Ok("test response".to_string());

    CodingAssistant::handle_agent_turn_result(&mut agent, Some(&runner), &result, 100)
        .await
        .unwrap();

    let json: serde_json::Value = serde_json::from_str(&captured.lock().unwrap()).unwrap();
    assert_eq!(json["subtype"], "success");
}

#[tokio::test]
#[cfg(unix)]
async fn handle_agent_turn_stop_hook_failure_does_not_discard_result() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let captured_clone = captured.clone();
    let mut agent = test_agent_for_turn().with_streaming_json(move |json| {
        *captured_clone.lock().unwrap() = json.to_string();
    });

    // A stop hook that always fails (fail_closed with an invalid exit)
    let cwd = std::env::temp_dir();
    let command = crate::config::hooks::HookCommand {
        command: "exit 1".to_string(),
        timeout: std::time::Duration::from_secs(2),
        fail_closed: true,
        status_message: None,
        source_path: cwd.join("hooks.json"),
    };
    let loaded = crate::config::hooks::LoadedHooks {
        groups: vec![crate::config::hooks::HookGroup {
            event: crate::config::hooks::HookEvent::Stop,
            matcher: crate::config::hooks::HookMatcher::All,
            hooks: vec![command],
        }],
    };
    let runner = std::sync::Arc::new(HookRunner::new(
        loaded,
        HookContext {
            session_id: uuid::Uuid::new_v4(),
            task_id: uuid::Uuid::new_v4(),
            transcript_path: None,
            session_writer: None,
            hook_event_sink: None,
            cwd,
            model: "test-model".to_string(),
        },
    ));

    let result: Result<String, anyhow::Error> = Ok("test response".to_string());

    // Should NOT propagate the hook error — should still emit completion record
    CodingAssistant::handle_agent_turn_result(&mut agent, Some(&runner), &result, 100)
        .await
        .unwrap();

    let json: serde_json::Value = serde_json::from_str(&captured.lock().unwrap()).unwrap();
    assert_eq!(json["subtype"], "success");
    assert_eq!(json["duration_ms"], 100);
}

#[tokio::test]
#[cfg(unix)]
async fn handle_agent_turn_error_occurred_hook_failure_does_not_mask_error() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let captured_clone = captured.clone();
    let mut agent = test_agent_for_turn().with_streaming_json(move |json| {
        *captured_clone.lock().unwrap() = json.to_string();
    });

    // An error_occurred hook that always fails (fail_closed with an invalid exit)
    let cwd = std::env::temp_dir();
    let command = crate::config::hooks::HookCommand {
        command: "exit 1".to_string(),
        timeout: std::time::Duration::from_secs(2),
        fail_closed: true,
        status_message: None,
        source_path: cwd.join("hooks.json"),
    };
    let loaded = crate::config::hooks::LoadedHooks {
        groups: vec![crate::config::hooks::HookGroup {
            event: crate::config::hooks::HookEvent::ErrorOccurred,
            matcher: crate::config::hooks::HookMatcher::All,
            hooks: vec![command],
        }],
    };
    let runner = std::sync::Arc::new(HookRunner::new(
        loaded,
        HookContext {
            session_id: uuid::Uuid::new_v4(),
            task_id: uuid::Uuid::new_v4(),
            transcript_path: None,
            session_writer: None,
            hook_event_sink: None,
            cwd,
            model: "test-model".to_string(),
        },
    ));

    let result: Result<String, anyhow::Error> = Err(anyhow::anyhow!("tool error"));

    // Should NOT propagate the hook error — should still emit the error completion record
    CodingAssistant::handle_agent_turn_result(&mut agent, Some(&runner), &result, 50)
        .await
        .unwrap();

    let json: serde_json::Value = serde_json::from_str(&captured.lock().unwrap()).unwrap();
    assert_eq!(json["subtype"], "error_during_execution");
    assert_eq!(json["error"], "tool error");
    assert_eq!(json["duration_ms"], 50);
}
