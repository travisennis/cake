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
