use super::*;
use temp_env::with_var;
use tempfile::TempDir;

fn create_home_dir() -> TempDir {
    let home = TempDir::new().unwrap();
    std::fs::create_dir_all(home.path().join(".config")).unwrap();
    home
}

fn write_global_settings(home: &Path, content: &str) {
    let xdg_dir = home.join(".config").join("cake");
    std::fs::create_dir_all(&xdg_dir).unwrap();
    std::fs::write(xdg_dir.join("settings.toml"), content).unwrap();
}

/// Create a temp directory with .cake/settings.toml (for project settings)
fn create_project_settings(content: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    let cake_dir = dir.path().join(".cake");
    std::fs::create_dir_all(&cake_dir).unwrap();
    let path = cake_dir.join("settings.toml");
    std::fs::write(&path, content).unwrap();
    dir
}

#[test]
fn test_load_single_file() {
    let dir = create_project_settings(
        r#"
[[models]]
name = "test-model"
model = "test/model"
base_url = "https://example.com"
api_key_env = "MY_KEY"
"#,
    );

    let home = create_home_dir();
    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(dir.path()))
    })
    .unwrap();

    assert_eq!(loaded.models.len(), 1);
    assert!(loaded.models.contains_key("test-model"));
    assert_eq!(loaded.models.get("test-model").unwrap().model, "test/model");
}

#[test]
fn test_load_merges_with_override() {
    let home = create_home_dir();
    // Global has "model-a" and "model-b"
    write_global_settings(
        home.path(),
        r#"
[[models]]
name = "model-a"
model = "global/model-a"
base_url = "https://global.example.com"
api_key_env = "GLOBAL_KEY"

[[models]]
name = "model-b"
model = "global/model-b"
base_url = "https://global.example.com"
api_key_env = "GLOBAL_KEY"
"#,
    );

    // Project has "model-b" (override) and "model-c" (new)
    let project_dir = create_project_settings(
        r#"
[[models]]
name = "model-b"
model = "project/model-b"
base_url = "https://project.example.com"
api_key_env = "PROJECT_KEY"

[[models]]
name = "model-c"
model = "project/model-c"
base_url = "https://project.example.com"
api_key_env = "PROJECT_KEY"
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(project_dir.path()))
    })
    .unwrap();

    assert_eq!(loaded.models.len(), 3);
    // model-a from global
    assert_eq!(
        loaded.models.get("model-a").unwrap().model,
        "global/model-a"
    );
    // model-b overridden by project
    assert_eq!(
        loaded.models.get("model-b").unwrap().model,
        "project/model-b"
    );
    // model-c from project
    assert_eq!(
        loaded.models.get("model-c").unwrap().model,
        "project/model-c"
    );
}

#[test]
fn test_load_reads_xdg_global_settings() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
[[models]]
name = "xdg-model"
model = "xdg/model"
base_url = "https://example.com"
api_key_env = "XDG_KEY"
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || SettingsLoader::load(None)).unwrap();

    assert_eq!(loaded.models.len(), 1);
    assert_eq!(loaded.models.get("xdg-model").unwrap().model, "xdg/model");
}

#[test]
fn test_project_overrides_xdg_global() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
[[models]]
name = "shared"
model = "xdg/model"
base_url = "https://global.example.com"
api_key_env = "GLOBAL_KEY"
"#,
    );
    let project_dir = create_project_settings(
        r#"
[[models]]
name = "shared"
model = "project/model"
base_url = "https://project.example.com"
api_key_env = "PROJECT_KEY"
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(project_dir.path()))
    })
    .unwrap();

    assert_eq!(loaded.models.get("shared").unwrap().model, "project/model");
}

#[test]
fn test_load_missing_file_succeeds() {
    let home = create_home_dir();
    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(Path::new("/nonexistent")))
    });
    assert!(loaded.is_ok());
    assert!(loaded.unwrap().models.is_empty());
}

#[test]
fn test_duplicate_name_in_file() {
    let dir = create_project_settings(
        r#"
[[models]]
name = "dup"
model = "first"
base_url = "https://example.com"
api_key_env = "MY_KEY"

[[models]]
name = "dup"
model = "second"
base_url = "https://example.com"
api_key_env = "MY_KEY"
"#,
    );

    let home = create_home_dir();
    let result = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(dir.path()))
    });
    assert!(matches!(result, Err(SettingsError::DuplicateModelName { name }) if name == "dup"));
}

#[test]
fn test_invalid_name_format() {
    let dir = create_project_settings(
        r#"
[[models]]
name = "Invalid Name!"
model = "test"
base_url = "https://example.com"
api_key_env = "MY_KEY"
"#,
    );

    let home = create_home_dir();
    let result = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(dir.path()))
    });
    assert!(matches!(
        result,
        Err(SettingsError::InvalidModelName { name, .. }) if name == "Invalid Name!"
    ));
}

#[test]
fn test_model_definition_all_fields() {
    let dir = create_project_settings(
        r#"
[[models]]
name = "minimal"
model = "test/model"
base_url = "https://example.com"
api_key_env = "MY_KEY"
provider = "openrouter"
provider_headers = { http_referer = "https://example.com/cake", x_title = "cake-test" }
context_window = 200000
"#,
    );

    let home = create_home_dir();
    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(dir.path()))
    })
    .unwrap();
    let def = loaded.models.get("minimal").unwrap();

    assert_eq!(def.model, "test/model");
    assert_eq!(def.base_url, "https://example.com");
    assert_eq!(def.api_key_env, "MY_KEY");
    assert_eq!(def.provider, Some(ModelProvider::OpenRouter));
    assert_eq!(
        def.provider_headers,
        Some(ProviderHeaders {
            http_referer: Some("https://example.com/cake".to_string()),
            x_title: Some("cake-test".to_string()),
        })
    );
    assert_eq!(def.context_window, Some(200_000));
    assert_eq!(def.api_type, ApiType::ChatCompletions);
    assert!(def.providers.is_empty());
    assert_eq!(def.reasoning_effort, None);
    assert_eq!(def.reasoning_summary, None);
    assert_eq!(def.reasoning_max_tokens, None);
}

#[test]
fn test_validate_name_valid() {
    assert!(ModelDefinition::validate_name("simple").is_ok());
    assert!(ModelDefinition::validate_name("my-model").is_ok());
    assert!(ModelDefinition::validate_name("model-123").is_ok());
    assert!(ModelDefinition::validate_name("a").is_ok());
    assert!(ModelDefinition::validate_name("a1b2c3").is_ok());
}

#[test]
fn test_validate_name_invalid() {
    assert!(ModelDefinition::validate_name("").is_err());
    assert!(ModelDefinition::validate_name("Invalid").is_err());
    assert!(ModelDefinition::validate_name("my_model").is_err());
    assert!(ModelDefinition::validate_name("model.123").is_err());
    assert!(ModelDefinition::validate_name("model 123").is_err());
}

#[test]
fn test_to_model_config() {
    let def = ModelDefinition {
        name: "test".to_string(),
        model: "test/model".to_string(),
        base_url: "https://example.com".to_string(),
        api_key_env: "MY_KEY".to_string(),
        provider: Some(ModelProvider::OpenRouter),
        provider_headers: Some(ProviderHeaders {
            http_referer: Some("https://example.com/cake".to_string()),
            x_title: Some("cake-test".to_string()),
        }),
        api_type: ApiType::Responses,
        temperature: Some(0.5),
        top_p: Some(0.9),
        max_output_tokens: Some(4000),
        context_window: Some(200_000),
        reasoning_effort: Some(ReasoningEffort::High),
        reasoning_summary: Some("concise".to_string()),
        reasoning_max_tokens: Some(8000),
        providers: vec!["Provider1".to_string()],
    };

    let config = def.to_model_config();

    assert_eq!(config.model, "test/model");
    assert_eq!(config.base_url, "https://example.com");
    assert_eq!(config.api_key_env, "MY_KEY");
    assert_eq!(config.provider, Some(ModelProvider::OpenRouter));
    assert_eq!(
        config.provider_headers,
        Some(ProviderHeaders {
            http_referer: Some("https://example.com/cake".to_string()),
            x_title: Some("cake-test".to_string()),
        })
    );
    assert_eq!(config.api_type, ApiType::Responses);
    assert_eq!(config.temperature, Some(0.5));
    assert_eq!(config.top_p, Some(0.9));
    assert_eq!(config.max_output_tokens, Some(4000));
    assert_eq!(config.context_window, Some(200_000));
    assert_eq!(config.reasoning_effort, Some(ReasoningEffort::High));
    assert_eq!(config.reasoning_summary, Some("concise".to_string()));
    assert_eq!(config.reasoning_max_tokens, Some(8000));
    assert_eq!(config.providers, vec!["Provider1"]);
}

// --- LoadedSettings and default_model tests ---

#[test]
fn test_default_model_valid() {
    let dir = create_project_settings(
        r#"
default_model = "zen"

[[models]]
name = "zen"
model = "glm-5.1"
base_url = "https://opencode.ai/zen/go/v1/"
api_key_env = "OPENCODE_ZEN_API_TOKEN"
"#,
    );

    let home = create_home_dir();
    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(dir.path()))
    })
    .unwrap();

    assert_eq!(loaded.default_model, Some("zen".to_string()));
    assert!(loaded.models.contains_key("zen"));
}

#[test]
fn test_default_model_not_found() {
    let dir = create_project_settings(
        r#"
default_model = "nonexistent"

[[models]]
name = "zen"
model = "glm-5.1"
base_url = "https://example.com"
api_key_env = "KEY"
"#,
    );

    let home = create_home_dir();
    let result = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(dir.path()))
    });
    assert!(matches!(
        result,
        Err(SettingsError::DefaultModelNotFound { name }) if name == "nonexistent"
    ));
}

#[test]
fn test_no_default_model() {
    let dir = create_project_settings(
        r#"
[[models]]
name = "zen"
model = "glm-5.1"
base_url = "https://example.com"
api_key_env = "KEY"
"#,
    );

    let home = create_home_dir();
    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(dir.path()))
    })
    .unwrap();

    assert_eq!(loaded.default_model, None);
}

#[test]
fn test_project_overrides_default_model() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
default_model = "global-model"

[[models]]
name = "global-model"
model = "global/model"
base_url = "https://global.example.com"
api_key_env = "GLOBAL_KEY"
"#,
    );

    let project_dir = create_project_settings(
        r#"
default_model = "project-model"

[[models]]
name = "project-model"
model = "project/model"
base_url = "https://project.example.com"
api_key_env = "PROJECT_KEY"
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(project_dir.path()))
    })
    .unwrap();

    assert_eq!(loaded.default_model, Some("project-model".to_string()));
}

#[test]
fn test_directories_merge_global_and_project() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
directories = ["/global/dir1", "/global/dir2"]

[[models]]
name = "zen"
model = "glm-5.1"
base_url = "https://example.com"
api_key_env = "KEY"
"#,
    );

    let project_dir = create_project_settings(
        r#"
directories = ["/project/dir1", "/global/dir2"]

[[models]]
name = "proj"
model = "proj/model"
base_url = "https://project.example.com"
api_key_env = "PROJ_KEY"
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(project_dir.path()))
    })
    .unwrap();

    // Directories are merged without duplicates
    assert_eq!(loaded.directories.len(), 3);
    assert!(loaded.directories.contains(&"/global/dir1".to_string()));
    assert!(loaded.directories.contains(&"/global/dir2".to_string()));
    assert!(loaded.directories.contains(&"/project/dir1".to_string()));
}

#[test]
fn test_directories_only_global() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
directories = ["/global/dir"]

[[models]]
name = "zen"
model = "glm-5.1"
base_url = "https://example.com"
api_key_env = "KEY"
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || SettingsLoader::load(None)).unwrap();

    assert_eq!(loaded.directories, vec!["/global/dir".to_string()]);
}

#[test]
fn test_directories_empty_by_default() {
    let dir = create_project_settings(
        r#"
[[models]]
name = "zen"
model = "glm-5.1"
base_url = "https://example.com"
api_key_env = "KEY"
"#,
    );

    let home = create_home_dir();
    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(dir.path()))
    })
    .unwrap();

    assert!(loaded.directories.is_empty());
}

#[test]
fn test_project_explicitly_clears_default_model() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
default_model = "global-model"

[[models]]
name = "global-model"
model = "global/model"
base_url = "https://global.example.com"
api_key_env = "GLOBAL_KEY"
"#,
    );

    // Project file has no default_model line at all — global should persist.
    let project_dir = create_project_settings(
        r#"
[[models]]
name = "project-model"
model = "project/model"
base_url = "https://project.example.com"
api_key_env = "PROJECT_KEY"
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(project_dir.path()))
    })
    .unwrap();

    // Project didn't set default_model, so global persists
    assert_eq!(loaded.default_model, Some("global-model".to_string()));
}

#[test]
fn test_project_without_skills_preserves_global_skills() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
[skills]
only = ["global-skill"]

[[models]]
name = "global-model"
model = "global/model"
base_url = "https://global.example.com"
api_key_env = "GLOBAL_KEY"
"#,
    );

    let project_dir = create_project_settings(
        r#"
[[models]]
name = "project-model"
model = "project/model"
base_url = "https://project.example.com"
api_key_env = "PROJECT_KEY"
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(project_dir.path()))
    })
    .unwrap();

    assert_eq!(loaded.skills.only, vec!["global-skill"]);
}

#[test]
fn test_skills_path_loads_from_settings() {
    let dir = create_project_settings(
        r#"
[skills]
path = "~/my-skills:/shared/team-skills"

[[models]]
name = "zen"
model = "glm-5.1"
base_url = "https://example.com"
api_key_env = "KEY"
"#,
    );

    let home = create_home_dir();
    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(dir.path()))
    })
    .unwrap();

    assert_eq!(
        loaded.skills.path,
        Some("~/my-skills:/shared/team-skills".to_string())
    );
}

#[test]
fn test_project_skills_overrides_global_skills_path() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
[skills]
path = "/global/skills"

[[models]]
name = "global-model"
model = "global/model"
base_url = "https://global.example.com"
api_key_env = "GLOBAL_KEY"
"#,
    );

    let project_dir = create_project_settings(
        r#"
[skills]
path = "/project/skills"

[[models]]
name = "project-model"
model = "project/model"
base_url = "https://project.example.com"
api_key_env = "PROJECT_KEY"
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(project_dir.path()))
    })
    .unwrap();

    assert_eq!(loaded.skills.path, Some("/project/skills".to_string()));
}

#[test]
fn test_profile_skills_path_overrides_top_level() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
[skills]
path = "/base/skills"

[[models]]
name = "base"
model = "base/model"
base_url = "https://example.com"
api_key_env = "KEY"

[profiles.expanded.skills]
path = "/profile/skills"
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load_with_profile(None, Some("expanded"))
    })
    .unwrap();

    assert_eq!(loaded.skills.path, Some("/profile/skills".to_string()));
}

#[test]
fn test_global_profile_applies_when_selected() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
default_model = "base"

[[models]]
name = "base"
model = "base/model"
base_url = "https://example.com"
api_key_env = "KEY"

[[models]]
name = "fast"
model = "fast/model"
base_url = "https://example.com"
api_key_env = "KEY"

[profiles.fast]
default_model = "fast"
directories = ["/profile/dir"]

[profiles.fast.skills]
only = ["debugging-cake"]
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load_with_profile(None, Some("fast"))
    })
    .unwrap();

    assert_eq!(loaded.default_model, Some("fast".to_string()));
    assert!(loaded.directories.contains(&"/profile/dir".to_string()));
    assert_eq!(loaded.skills.only, vec!["debugging-cake"]);
}

#[test]
fn test_project_profile_overrides_global_profile() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
default_model = "base"

[[models]]
name = "base"
model = "base/model"
base_url = "https://global.example.com"
api_key_env = "KEY"

[[models]]
name = "global-fast"
model = "global-fast/model"
base_url = "https://global.example.com"
api_key_env = "KEY"

[profiles.fast]
default_model = "global-fast"
directories = ["/global/profile"]

[profiles.fast.skills]
disabled = true
"#,
    );
    let project_dir = create_project_settings(
        r#"
[[models]]
name = "project-fast"
model = "project-fast/model"
base_url = "https://project.example.com"
api_key_env = "KEY"

[profiles.fast]
default_model = "project-fast"
directories = ["/project/profile"]

[profiles.fast.skills]
only = ["review"]
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load_with_profile(Some(project_dir.path()), Some("fast"))
    })
    .unwrap();

    assert_eq!(loaded.default_model, Some("project-fast".to_string()));
    assert!(loaded.directories.contains(&"/global/profile".to_string()));
    assert!(loaded.directories.contains(&"/project/profile".to_string()));
    assert!(loaded.skills.disabled);
    assert_eq!(loaded.skills.only, vec!["review"]);
}

#[test]
fn test_profile_omitted_fields_preserve_top_level_settings() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
default_model = "base"
directories = ["/base/dir"]

[skills]
only = ["base-skill"]

[[models]]
name = "base"
model = "base/model"
base_url = "https://example.com"
api_key_env = "KEY"

[profiles.review]
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load_with_profile(None, Some("review"))
    })
    .unwrap();

    assert_eq!(loaded.default_model, Some("base".to_string()));
    assert!(loaded.directories.contains(&"/base/dir".to_string()));
    assert_eq!(loaded.skills.only, vec!["base-skill"]);
}

#[test]
fn test_profile_directories_merge_and_deduplicate() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
directories = ["/shared", "/global"]

[[models]]
name = "base"
model = "base/model"
base_url = "https://example.com"
api_key_env = "KEY"

[profiles.expanded]
directories = ["/shared", "/profile"]
"#,
    );
    let project_dir = create_project_settings(
        r#"
directories = ["/project"]

[profiles.expanded]
directories = ["/profile", "/project-profile"]
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load_with_profile(Some(project_dir.path()), Some("expanded"))
    })
    .unwrap();

    assert_eq!(loaded.directories.len(), 5);
    assert!(loaded.directories.contains(&"/shared".to_string()));
    assert!(loaded.directories.contains(&"/global".to_string()));
    assert!(loaded.directories.contains(&"/project".to_string()));
    assert!(loaded.directories.contains(&"/profile".to_string()));
    assert!(loaded.directories.contains(&"/project-profile".to_string()));
}

#[test]
fn test_unknown_profile_errors_with_available_names() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
[[models]]
name = "base"
model = "base/model"
base_url = "https://example.com"
api_key_env = "KEY"

[profiles.fast]
"#,
    );

    let result = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load_with_profile(None, Some("missing"))
    });

    assert!(matches!(
        result,
        Err(SettingsError::UnknownProfile { name, available })
            if name == "missing" && available.contains("fast")
    ));
}

#[test]
fn test_invalid_profile_name_errors() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
[[models]]
name = "base"
model = "base/model"
base_url = "https://example.com"
api_key_env = "KEY"

[profiles."Bad_Profile"]
"#,
    );

    let result = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load_with_profile(None, Some("Bad_Profile"))
    });

    assert!(matches!(
        result,
        Err(SettingsError::InvalidProfileName { name, .. }) if name == "Bad_Profile"
    ));
}

#[test]
fn test_profile_default_model_not_found_errors() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
[[models]]
name = "base"
model = "base/model"
base_url = "https://example.com"
api_key_env = "KEY"

[profiles.fast]
default_model = "missing"
"#,
    );

    let result = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load_with_profile(None, Some("fast"))
    });

    assert!(matches!(
        result,
        Err(SettingsError::DefaultModelNotFound { name }) if name == "missing"
    ));
}

#[test]
fn test_models_inside_profile_are_rejected() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
[[models]]
name = "base"
model = "base/model"
base_url = "https://example.com"
api_key_env = "KEY"

[[profiles.fast.models]]
name = "nested"
model = "nested/model"
base_url = "https://example.com"
api_key_env = "KEY"
"#,
    );

    let result = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load_with_profile(None, Some("fast"))
    });

    assert!(matches!(
        result,
        Err(SettingsError::ProfileModelsUnsupported { name }) if name == "fast"
    ));
}

#[test]
fn test_empty_models_key_inside_profile_is_rejected() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
[[models]]
name = "base"
model = "base/model"
base_url = "https://example.com"
api_key_env = "KEY"

[profiles.fast]
models = []
"#,
    );

    let result = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load_with_profile(None, Some("fast"))
    });

    assert!(matches!(
        result,
        Err(SettingsError::ProfileModelsUnsupported { name }) if name == "fast"
    ));
}

#[test]
fn test_sandbox_merge_global_and_project() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
[sandbox]
read_only = ["/global/bin", "/shared/bin"]
writable = ["/global/state"]
"#,
    );

    let project_dir = create_project_settings(
        r#"
[sandbox]
read_only = ["/project/bin", "/shared/bin"]
writable = ["/project/state"]
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(project_dir.path()))
    })
    .unwrap();

    // Both keys merge as a union without duplicates.
    assert_eq!(loaded.sandbox.read_only.len(), 3);
    assert!(
        loaded
            .sandbox
            .read_only
            .contains(&"/global/bin".to_string())
    );
    assert!(
        loaded
            .sandbox
            .read_only
            .contains(&"/shared/bin".to_string())
    );
    assert!(
        loaded
            .sandbox
            .read_only
            .contains(&"/project/bin".to_string())
    );

    assert_eq!(loaded.sandbox.writable.len(), 2);
    assert!(
        loaded
            .sandbox
            .writable
            .contains(&"/global/state".to_string())
    );
    assert!(
        loaded
            .sandbox
            .writable
            .contains(&"/project/state".to_string())
    );
}

#[test]
fn test_sandbox_profile_merge_and_deduplicate() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
[sandbox]
read_only = ["/global/bin"]
writable = ["/global/state"]

[[models]]
name = "base"
model = "base/model"
base_url = "https://example.com"
api_key_env = "KEY"

[profiles.expanded]

[profiles.expanded.sandbox]
read_only = ["/profile/bin", "/global/bin"]
writable = ["/profile/state"]
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load_with_profile(None, Some("expanded"))
    })
    .unwrap();

    assert_eq!(loaded.sandbox.read_only.len(), 2);
    assert!(
        loaded
            .sandbox
            .read_only
            .contains(&"/global/bin".to_string())
    );
    assert!(
        loaded
            .sandbox
            .read_only
            .contains(&"/profile/bin".to_string())
    );

    assert_eq!(loaded.sandbox.writable.len(), 2);
    assert!(
        loaded
            .sandbox
            .writable
            .contains(&"/global/state".to_string())
    );
    assert!(
        loaded
            .sandbox
            .writable
            .contains(&"/profile/state".to_string())
    );
}

#[test]
fn test_sandbox_empty_by_default() {
    let dir = create_project_settings(
        r#"
[[models]]
name = "zen"
model = "glm-5.1"
base_url = "https://example.com"
api_key_env = "KEY"
"#,
    );

    let home = create_home_dir();
    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(dir.path()))
    })
    .unwrap();

    assert!(loaded.sandbox.read_only.is_empty());
    assert!(loaded.sandbox.writable.is_empty());
}

#[test]
fn test_directories_and_sandbox_expand_tilde() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
directories = ["~/shared", "~/.cache/global"]

[sandbox]
read_only = ["~/.local/bin/tool", "~/.cache/binaries"]
writable = ["~/.cache/claude"]
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || SettingsLoader::load(None)).unwrap();

    let home_str = home.path().to_string_lossy().into_owned();
    assert!(loaded.directories.contains(&format!("{home_str}/shared")));
    assert!(
        loaded
            .directories
            .contains(&format!("{home_str}/.cache/global"))
    );
    assert!(
        loaded
            .sandbox
            .read_only
            .contains(&format!("{home_str}/.local/bin/tool"))
    );
    assert!(
        loaded
            .sandbox
            .read_only
            .contains(&format!("{home_str}/.cache/binaries"))
    );
    assert!(
        loaded
            .sandbox
            .writable
            .contains(&format!("{home_str}/.cache/claude"))
    );
}

// =============================================================================
// LLM judge settings
// =============================================================================

#[test]
fn test_judge_settings_defaults() {
    let dir = create_project_settings(
        r#"
[[models]]
name = "zen"
model = "glm-5.1"
base_url = "https://example.com"
api_key_env = "KEY"
"#,
    );

    let home = create_home_dir();
    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(dir.path()))
    })
    .unwrap();

    assert_eq!(loaded.judge, JudgeSettings::default());
    assert_eq!(loaded.judge.model, None);
    assert_eq!(loaded.judge.timeout_secs, 30);
    assert_eq!(loaded.judge.retry_budget_secs, 15);
    assert!(loaded.judge.enabled, "the judge is enabled by default");
    assert!(loaded.judge.allowlist.is_empty());
}

#[test]
fn test_judge_settings_allowlist_and_enabled_merge() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
[[models]]
name = "global-judge"
model = "glm-5.1"
base_url = "https://example.com"
api_key_env = "KEY"

[tools.bash.judge]
allowlist = ["git status", "ls -la"]
"#,
    );
    let project = create_project_settings(
        r#"
[tools.bash.judge]
enabled = false
allowlist = ["git push --force-with-lease"]
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(project.path()))
    })
    .unwrap();

    // The allowlist is a union across settings files; an explicit `enabled`
    // in the project overrides the (absent) global value.
    assert!(!loaded.judge.enabled);
    assert_eq!(
        loaded.judge.allowlist,
        vec!["git status", "ls -la", "git push --force-with-lease"]
    );
}

#[test]
fn test_judge_settings_enabled_absent_defaults_to_true() {
    let home = create_home_dir();
    let project = create_project_settings(
        r#"
[tools.bash.judge]
allowlist = ["git status"]
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(project.path()))
    })
    .unwrap();

    assert!(loaded.judge.enabled, "absent `enabled` keeps the judge on");
    assert_eq!(loaded.judge.allowlist, vec!["git status"]);
}

#[test]
fn test_judge_settings_global_and_project_merge() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
[[models]]
name = "global-judge"
model = "glm-5.1"
base_url = "https://example.com"
api_key_env = "KEY"

[tools.bash.judge]
model = "global-judge"
timeout_secs = 45
"#,
    );
    let project = create_project_settings(
        r"
[tools.bash.judge]
timeout_secs = 90
",
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(project.path()))
    })
    .unwrap();

    // Project overrides the timeout; the global model survives because the
    // project's judge table did not set one.
    assert_eq!(loaded.judge.model.as_deref(), Some("global-judge"));
    assert_eq!(loaded.judge.timeout_secs, 90);
}

#[test]
fn test_judge_settings_retry_budget_resolution() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r"
[tools.bash.judge]
retry_budget_secs = 30
",
    );
    let project = create_project_settings(
        r"
[tools.bash.judge]
retry_budget_secs = 0
",
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(project.path()))
    })
    .unwrap();

    // The project value wins; 0 is a legal explicit value that disables
    // recovery, not a "absent" marker.
    assert_eq!(loaded.judge.retry_budget_secs, 0);

    let project = create_project_settings(
        r#"
[tools.bash.judge]
allowlist = ["git status"]
"#,
    );
    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(project.path()))
    })
    .unwrap();

    // An absent project key keeps the lower-precedence global value.
    assert_eq!(loaded.judge.retry_budget_secs, 30);
}

#[test]
fn test_judge_settings_project_only() {
    let home = create_home_dir();
    let project = create_project_settings(
        r#"
[[models]]
name = "project-judge"
model = "glm-5.1"
base_url = "https://example.com"
api_key_env = "KEY"

[tools.bash.judge]
model = "project-judge"
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(project.path()))
    })
    .unwrap();

    // The judge model is a [[models]] name, resolved to the full entry.
    assert_eq!(loaded.judge.model.as_deref(), Some("project-judge"));
    // Default timeout applies when the file does not set one.
    assert_eq!(loaded.judge.timeout_secs, 30);
}

#[test]
fn test_judge_settings_unknown_model_errors() {
    let home = create_home_dir();
    let project = create_project_settings(
        r#"
[tools.bash.judge]
model = "no-such-model"
"#,
    );

    // An unknown judge model fails at load time, like default_model.
    // `CAKE_JUDGE=off` is unset so the load validates even under an ambient
    // bypass env in the shell.
    let err = with_var("CAKE_JUDGE", None::<&str>, || {
        with_var("HOME", Some(home.path()), || {
            SettingsLoader::load(Some(project.path()))
        })
    })
    .unwrap_err();
    assert!(matches!(
        err,
        SettingsError::JudgeModelNotFound { ref name } if name == "no-such-model"
    ));
}

#[test]
fn test_judge_settings_unknown_model_ignored_when_judge_disabled() {
    let home = create_home_dir();
    let project = create_project_settings(
        r#"
[tools.bash.judge]
model = "no-such-model"
enabled = false
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(project.path()))
    })
    .unwrap();

    // A bypassed judge's model config is inert: `enabled = false` is the
    // recovery path even for a misconfigured judge, so the unknown name does
    // not fail the load.
    assert!(!loaded.judge.enabled);
    assert_eq!(loaded.judge.model.as_deref(), Some("no-such-model"));
}

#[test]
fn test_judge_settings_zero_timeout_is_clamped() {
    let home = create_home_dir();
    let project = create_project_settings(
        r"
[tools.bash.judge]
timeout_secs = 0
",
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(project.path()))
    })
    .unwrap();

    // A zero timeout would expire before the judge request is polled and fail
    // every command closed; it is raised to the floor instead.
    assert_eq!(loaded.judge.timeout_secs, 1);
}

// --- [limits] ---

#[test]
fn test_limits_load_from_settings() {
    let dir = create_project_settings(
        r#"
[[models]]
name = "test-model"
model = "test/model"
base_url = "https://example.com"
api_key_env = "MY_KEY"

[limits]
max_turns = 10
max_tool_calls = 50
"#,
    );

    let home = create_home_dir();
    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(dir.path()))
    })
    .unwrap();

    assert_eq!(loaded.limits.max_turns, Some(Limit::max(10)));
    assert_eq!(loaded.limits.max_tool_calls, Some(Limit::max(50)));
    assert!(loaded.warnings.is_empty(), "{:#?}", loaded.warnings);
}

#[test]
fn test_limits_default_to_unlimited() {
    let dir = create_project_settings(
        r#"
[[models]]
name = "test-model"
model = "test/model"
base_url = "https://example.com"
api_key_env = "MY_KEY"
"#,
    );

    let home = create_home_dir();
    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(dir.path()))
    })
    .unwrap();

    assert_eq!(loaded.limits.max_turns, None);
    assert_eq!(loaded.limits.max_tool_calls, None);
}

#[test]
fn test_limits_project_overrides_global_per_key() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
[[models]]
name = "test-model"
model = "test/model"
base_url = "https://example.com"
api_key_env = "MY_KEY"

[limits]
max_turns = 5
max_tool_calls = 50
"#,
    );
    // The project section overrides only the keys it sets.
    let project = create_project_settings(
        r#"
[[models]]
name = "test-model"
model = "test/model"
base_url = "https://project.example.com"
api_key_env = "MY_KEY"

[limits]
max_turns = 10
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(project.path()))
    })
    .unwrap();

    assert_eq!(loaded.limits.max_turns, Some(Limit::max(10)));
    assert_eq!(loaded.limits.max_tool_calls, Some(Limit::max(50)));
}

#[test]
fn test_limits_accept_unlimited_string() {
    let dir = create_project_settings(
        r#"
[[models]]
name = "test-model"
model = "test/model"
base_url = "https://example.com"
api_key_env = "MY_KEY"

[limits]
max_turns = "unlimited"
max_tool_calls = "UNLIMITED"
"#,
    );

    let home = create_home_dir();
    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(dir.path()))
    })
    .unwrap();

    // An explicit "unlimited" is accepted and resolves to no cap in the
    // effective settings. The key-stays-present distinction is what lets a
    // project override a global cap, covered by the merge test below.
    assert_eq!(loaded.limits.max_turns, None);
    assert_eq!(loaded.limits.max_tool_calls, None);
}

#[test]
fn test_limits_reject_zero() {
    let dir = create_project_settings(
        r#"
[[models]]
name = "test-model"
model = "test/model"
base_url = "https://example.com"
api_key_env = "MY_KEY"

[limits]
max_turns = 0
"#,
    );

    let home = create_home_dir();
    let err = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(dir.path()))
    })
    .unwrap_err();

    assert!(
        err.to_string().contains("not valid limits"),
        "expected a clear rejection of 0, got: {err}"
    );
}

#[test]
fn test_limits_project_can_clear_global_limit() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
[[models]]
name = "test-model"
model = "test/model"
base_url = "https://example.com"
api_key_env = "MY_KEY"

[limits]
max_turns = 10
"#,
    );
    // The project explicitly opts back to unlimited, overriding the global cap.
    let project = create_project_settings(
        r#"
[[models]]
name = "test-model"
model = "test/model"
base_url = "https://project.example.com"
api_key_env = "MY_KEY"

[limits]
max_turns = "unlimited"
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(project.path()))
    })
    .unwrap();

    // The project's explicit "unlimited" overrides the global cap of 10 back
    // to uncapped; the untouched key stays absent.
    assert_eq!(loaded.limits.max_turns, None);
    assert_eq!(loaded.limits.max_tool_calls, None);
}

// --- [limits] tool output budgets ---

#[test]
fn test_limits_output_budgets_load_from_settings() {
    let dir = create_project_settings(
        r#"
[[models]]
name = "test-model"
model = "test/model"
base_url = "https://example.com"
api_key_env = "MY_KEY"

[limits]
bash_output_max_bytes = 5000
bash_read_cap = 10000
read_default_end_line = 50
read_max_output_bytes = 20000
hook_output_limit = 1024
"#,
    );

    let home = create_home_dir();
    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(dir.path()))
    })
    .unwrap();

    assert_eq!(loaded.limits.bash_output_max_bytes, Some(Limit::max(5000)));
    assert_eq!(loaded.limits.bash_read_cap, Some(Limit::max(10000)));
    assert_eq!(loaded.limits.read_default_end_line, Some(Limit::max(50)));
    assert_eq!(loaded.limits.read_max_output_bytes, Some(Limit::max(20000)));
    assert_eq!(loaded.limits.hook_output_limit, Some(Limit::max(1024)));
    assert!(loaded.warnings.is_empty(), "{:#?}", loaded.warnings);

    let tool = loaded.limits.tool_limits();
    assert_eq!(tool.bash_output_max_bytes, Some(5000));
    assert_eq!(tool.bash_read_cap, Some(10000));
    assert_eq!(tool.read_default_end_line, Some(50));
    assert_eq!(tool.read_max_output_bytes, Some(20000));
    assert_eq!(tool.hook_output_limit, Some(1024));
}

#[test]
fn test_limits_output_budgets_default_to_compiled_values() {
    let dir = create_project_settings(
        r#"
[[models]]
name = "test-model"
model = "test/model"
base_url = "https://example.com"
api_key_env = "MY_KEY"
"#,
    );

    let home = create_home_dir();
    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(dir.path()))
    })
    .unwrap();

    // Absent keys resolve to the compiled defaults, so out-of-the-box tool
    // behavior is unchanged.
    let tool = loaded.limits.tool_limits();
    assert_eq!(tool, ToolLimits::defaults());
    assert_eq!(
        tool.bash_output_max_bytes,
        Some(DEFAULT_BASH_OUTPUT_MAX_BYTES as usize)
    );
    assert_eq!(tool.bash_read_cap, Some(DEFAULT_BASH_READ_CAP as usize));
    assert_eq!(
        tool.read_default_end_line,
        Some(DEFAULT_READ_DEFAULT_END_LINE as usize)
    );
    assert_eq!(
        tool.read_max_output_bytes,
        Some(DEFAULT_READ_MAX_OUTPUT_BYTES as usize)
    );
    assert_eq!(
        tool.hook_output_limit,
        Some(DEFAULT_HOOK_OUTPUT_LIMIT as usize)
    );
}

#[test]
fn test_limits_output_budget_unlimited_disables_cap() {
    let dir = create_project_settings(
        r#"
[[models]]
name = "test-model"
model = "test/model"
base_url = "https://example.com"
api_key_env = "MY_KEY"

[limits]
bash_output_max_bytes = "unlimited"
read_max_output_bytes = "unlimited"
"#,
    );

    let home = create_home_dir();
    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(dir.path()))
    })
    .unwrap();

    let tool = loaded.limits.tool_limits();
    assert_eq!(tool.bash_output_max_bytes, None);
    assert_eq!(tool.read_max_output_bytes, None);
    // Untouched keys keep their compiled defaults.
    assert_eq!(tool.bash_read_cap, Some(DEFAULT_BASH_READ_CAP as usize));
}

#[test]
fn test_limits_output_budget_project_overrides_global_per_key() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
[[models]]
name = "test-model"
model = "test/model"
base_url = "https://example.com"
api_key_env = "MY_KEY"

[limits]
bash_output_max_bytes = 5000
read_default_end_line = 10
"#,
    );
    // The project section overrides only the keys it sets.
    let project = create_project_settings(
        r#"
[[models]]
name = "test-model"
model = "test/model"
base_url = "https://project.example.com"
api_key_env = "MY_KEY"

[limits]
bash_output_max_bytes = 9000
"#,
    );

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(project.path()))
    })
    .unwrap();

    let tool = loaded.limits.tool_limits();
    assert_eq!(tool.bash_output_max_bytes, Some(9000));
    assert_eq!(tool.read_default_end_line, Some(10));
}

// --- Unknown-key warnings ---

#[test]
fn test_unknown_model_key_warns() {
    let dir = create_project_settings(
        r#"
[[models]]
name = "zen"
model = "glm-5.1"
base_url = "https://example.com"
api_key_env = "KEY"
temparature = 0.7
"#,
    );

    let home = create_home_dir();
    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(dir.path()))
    })
    .unwrap();

    // The misspelled key is reported with its file and location instead of
    // being silently dropped.
    assert_eq!(loaded.warnings.len(), 1);
    let warning = &loaded.warnings[0];
    assert!(warning.contains("temparature"), "{warning}");
    assert!(warning.contains("[[models]] entry 'zen'"), "{warning}");
    assert!(warning.contains("settings.toml"), "{warning}");

    // Behavior is unchanged: the typo never reaches the model config.
    assert_eq!(loaded.models.get("zen").unwrap().temperature, None);
}

#[test]
fn test_unknown_keys_warn_across_sections() {
    let dir = create_project_settings(
        r#"
temparature = 0.5

[[models]]
name = "zen"
model = "glm-5.1"
base_url = "https://example.com"
api_key_env = "KEY"

[skills]
only = ["review"]
disabledd = true

[sandbox]
read_only = ["/tmp"]
writable_paths = ["/var"]

[profiles.review]
default_model = "zen"
top_p = 0.9

[profiles.review.skills]
onnly = ["review"]

[tools.bash.judge]
model = "zen"
timeout_sec = 25
"#,
    );

    let home = create_home_dir();
    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(dir.path()))
    })
    .unwrap();

    let warnings = &loaded.warnings;
    assert_eq!(warnings.len(), 6, "{warnings:?}");
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("temparature") && w.contains("top-level settings")),
        "{warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("disabledd") && w.contains("[skills]")),
        "{warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("writable_paths") && w.contains("[sandbox]")),
        "{warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("top_p") && w.contains("[profiles.review]")),
        "{warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("onnly") && w.contains("[profiles.review.skills]")),
        "{warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("timeout_sec") && w.contains("[tools.bash.judge]")),
        "{warnings:?}"
    );
}

#[test]
fn test_warnings_accumulate_from_global_and_project() {
    let home = create_home_dir();
    write_global_settings(
        home.path(),
        r#"
temparature = 0.5

[[models]]
name = "zen"
model = "glm-5.1"
base_url = "https://example.com"
api_key_env = "KEY"
"#,
    );
    let project_dir = create_project_settings("temparature = 0.6\n");

    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(project_dir.path()))
    })
    .unwrap();

    assert_eq!(loaded.warnings.len(), 2, "{:?}", loaded.warnings);
    // Global settings are loaded first, so its warning comes first.
    assert!(
        loaded.warnings[0].contains("cake/settings.toml"),
        "{}",
        loaded.warnings[0]
    );
    assert!(
        loaded.warnings[1].contains(".cake/settings.toml"),
        "{}",
        loaded.warnings[1]
    );
}

#[test]
fn test_valid_settings_produce_no_warnings() {
    // Every documented field, so a field serde round-trips differently than
    // its settings key would surface here as a false-positive warning.
    let dir = create_project_settings(
        r#"
default_model = "zen"
system_prompt = "prompts/coding.md"
directories = ["../shared"]

[[models]]
name = "zen"
model = "openai/gpt-5"
base_url = "https://openrouter.ai/api/v1/"
api_key_env = "OPENROUTER_API_KEY"
api_type = "responses"
provider = "openrouter"
provider_headers = { http_referer = "https://example.com", x_title = "cake" }
temperature = 0.7
top_p = 0.9
max_output_tokens = 8000
reasoning_effort = "high"
reasoning_summary = "concise"
reasoning_max_tokens = 2000
providers = ["Provider1", "Provider2"]

[skills]
disabled = false
only = ["review", "debug"]
path = "~/my-skills:/shared/team-skills"

[sandbox]
read_only = ["~/.local/bin/claude"]
writable = ["~/.claude"]

[profiles.review]
default_model = "zen"
directories = ["../standards"]
system_prompt = "prompts/review.md"
skills = { disabled = true, only = ["review"], path = "~/review-skills" }
sandbox = { read_only = ["~/.local/bin/other"], writable = ["~/.cache"] }

[tools.bash.judge]
model = "zen"
timeout_secs = 30
rubric_file = ".cake/judge-rubric.md"
enabled = true
allowlist = ["git status", "git diff"]
"#,
    );

    let home = create_home_dir();
    let loaded = with_var("HOME", Some(home.path()), || {
        SettingsLoader::load(Some(dir.path()))
    })
    .unwrap();

    assert_eq!(
        loaded.warnings,
        Vec::<String>::new(),
        "{:?}",
        loaded.warnings
    );
}
