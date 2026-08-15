use super::*;
use crate::cli::Commands;
use clap::CommandFactory;

// =============================================================================
// CLI parsing and help
// =============================================================================

#[test]
fn cli_parses_init() {
    let args = crate::CodingAssistant::parse_from(["cake", "init"]);
    match args.command {
        Some(Commands::Init(cmd)) => assert!(!cmd.hooks),
        other => panic!("expected init, got {other:?}"),
    }
}

#[test]
fn cli_parses_init_with_hooks() {
    let args = crate::CodingAssistant::parse_from(["cake", "init", "--hooks"]);
    match args.command {
        Some(Commands::Init(cmd)) => assert!(cmd.hooks),
        other => panic!("expected init --hooks, got {other:?}"),
    }
}

#[test]
fn init_help_documents_explicit_scaffolding() {
    let help = crate::CodingAssistant::command().render_help().to_string();
    assert!(
        help.contains("init"),
        "help should list the init subcommand:\n{help}"
    );

    let init_help = InitCommand::command().render_help().to_string();
    assert!(
        init_help.contains("hooks.json.example"),
        "init --help should document the hooks example:\n{init_help}"
    );
    assert!(
        init_help.contains("behavior-preserving"),
        "init --help should state the settings file is behavior-preserving:\n{init_help}"
    );
}

// =============================================================================
// initialize() behavior
// =============================================================================

#[test]
fn initialize_creates_behavior_preserving_settings() {
    let tmp = tempfile::TempDir::new().unwrap();

    let outcome = initialize(tmp.path(), false).unwrap();

    assert_eq!(outcome.settings, ".cake/settings.toml");
    assert!(outcome.hooks_example.is_none());

    let settings_path = tmp.path().join(".cake").join("settings.toml");
    assert!(settings_path.is_file());
    assert!(!tmp.path().join(".cake").join("hooks.json.example").exists());

    let content = std::fs::read_to_string(&settings_path).unwrap();
    // The file must parse as valid TOML and activate no settings keys, so it
    // cannot disable the judge, add allowlist entries, or select a model.
    let parsed: toml::Value =
        toml::from_str(&content).expect("generated settings must be valid TOML");
    let table = parsed
        .as_table()
        .expect("generated settings must be a table");
    assert!(
        table.is_empty(),
        "generated settings must not activate any keys"
    );
    assert!(
        content.contains("[tools.bash.judge]"),
        "generated settings must reference the judge vocabulary"
    );
    assert!(
        content.contains("allowlist"),
        "generated settings must document the judge allowlist key"
    );
    assert!(
        content.contains("[limits]"),
        "generated settings must reference the limits vocabulary"
    );
}

#[test]
fn initialize_with_hooks_creates_inert_example() {
    let tmp = tempfile::TempDir::new().unwrap();

    let outcome = initialize(tmp.path(), true).unwrap();

    assert_eq!(
        outcome.hooks_example.as_deref(),
        Some(".cake/hooks.json.example")
    );

    let example_path = tmp.path().join(".cake").join("hooks.json.example");
    let content = std::fs::read_to_string(&example_path).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&content).expect("generated hooks example must be valid JSON");
    assert_eq!(parsed["version"], 1);
    assert!(
        content.contains("outside the model tool sandbox"),
        "the example must explain the trust boundary:\n{content}"
    );
    // Cake's loader only reads hooks.json and hooks.local.json, never the
    // .example file, so the example is inert by construction.
    assert!(!tmp.path().join(".cake").join("hooks.json").exists());
    assert!(!tmp.path().join(".cake").join("hooks.local.json").exists());
}

#[test]
fn initialize_refuses_existing_settings_without_partial_writes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let settings_path = tmp.path().join(".cake").join("settings.toml");
    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    std::fs::write(&settings_path, "# existing\n").unwrap();

    let err = initialize(tmp.path(), true).unwrap_err();

    let conflict = err
        .downcast_ref::<InitError>()
        .expect("existing target must be reported as InitError::Conflict");
    assert!(
        conflict.to_string().contains(".cake/settings.toml"),
        "conflict must identify the existing target: {conflict}"
    );
    assert_eq!(
        std::fs::read_to_string(&settings_path).unwrap(),
        "# existing\n",
        "existing content must be preserved"
    );
    assert!(
        !tmp.path().join(".cake").join("hooks.json.example").exists(),
        "no planned target may be written when any target exists"
    );
}

#[test]
fn initialize_refuses_existing_hooks_example_without_partial_writes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let example_path = tmp.path().join(".cake").join("hooks.json.example");
    std::fs::create_dir_all(example_path.parent().unwrap()).unwrap();
    std::fs::write(&example_path, "{}").unwrap();

    let err = initialize(tmp.path(), true).unwrap_err();

    let conflict = err
        .downcast_ref::<InitError>()
        .expect("conflict error expected");
    assert!(
        conflict.to_string().contains(".cake/hooks.json.example"),
        "conflict must identify the existing hooks example: {conflict}"
    );
    assert!(
        !tmp.path().join(".cake").join("settings.toml").exists(),
        "settings must not be written when the hooks example already exists"
    );
}

#[test]
fn initialize_rerun_is_a_safe_refusal() {
    let tmp = tempfile::TempDir::new().unwrap();
    initialize(tmp.path(), true).unwrap();
    let original = std::fs::read_to_string(tmp.path().join(".cake").join("settings.toml")).unwrap();

    let err = initialize(tmp.path(), true).unwrap_err();

    assert!(
        err.downcast_ref::<InitError>().is_some(),
        "re-running after success must refuse"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join(".cake").join("settings.toml")).unwrap(),
        original,
        "re-running must not change the generated settings"
    );
}

#[test]
fn initialize_preserves_unrelated_cake_content_on_conflict() {
    let tmp = tempfile::TempDir::new().unwrap();
    let hooks_dir = tmp.path().join(".cake").join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    std::fs::write(hooks_dir.join("policy.sh"), "#!/bin/sh\n").unwrap();
    let settings_path = tmp.path().join(".cake").join("settings.toml");
    std::fs::write(&settings_path, "# existing\n").unwrap();

    let err = initialize(tmp.path(), true).unwrap_err();

    assert!(err.downcast_ref::<InitError>().is_some());
    assert!(
        hooks_dir.join("policy.sh").is_file(),
        "existing .cake content must survive"
    );
    assert_eq!(
        std::fs::read_to_string(&settings_path).unwrap(),
        "# existing\n",
        "existing settings must survive unchanged"
    );
    assert!(
        !tmp.path().join(".cake").join("hooks.json.example").exists(),
        "no planned target may be written on conflict"
    );
}

// =============================================================================
// Exit-code classification
// =============================================================================

#[test]
fn init_conflict_classifies_as_input_error() {
    let err: anyhow::Error =
        InitError::Conflict(".cake/settings.toml already exists".to_string()).into();
    assert_eq!(
        crate::exit_code::classify_to_u8(&err),
        crate::exit_code::code::INPUT_ERROR
    );
}

#[test]
fn init_conflict_message_identifies_each_target() {
    let single = InitError::Conflict(conflict_message(&[".cake/settings.toml".to_string()]));
    assert_eq!(
        single.to_string(),
        "refusing to initialize: .cake/settings.toml already exists"
    );

    let multiple = InitError::Conflict(conflict_message(&[
        ".cake/settings.toml".to_string(),
        ".cake/hooks.json.example".to_string(),
    ]));
    assert_eq!(
        multiple.to_string(),
        "refusing to initialize: .cake/settings.toml, .cake/hooks.json.example already exist"
    );
}

#[test]
fn hooks_example_parses_through_the_hook_loader() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("hooks.json");
    std::fs::write(&path, HOOKS_EXAMPLE).unwrap();

    // A user who copies the example to an active hooks file gets a valid,
    // benign file: the `_comment` field is ignored and the session-start hook
    // is accepted.
    let loaded = crate::config::HooksLoader::load_from_paths([path.as_path()])
        .expect("the example must parse as a valid hooks file");
    assert_eq!(loaded.groups.len(), 1);
}

// =============================================================================
// Path-safety hardening
// =============================================================================

#[test]
fn initialize_refuses_when_dot_cake_is_a_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join(".cake"), "not a directory").unwrap();

    let err = initialize(tmp.path(), false).unwrap_err();

    let conflict = err
        .downcast_ref::<InitError>()
        .expect("a file at .cake must be reported as a conflict");
    assert!(
        conflict.to_string().contains(".cake"),
        "conflict must name the .cake path: {conflict}"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join(".cake")).unwrap(),
        "not a directory",
        "the file must be preserved"
    );
    assert_eq!(
        crate::exit_code::classify_to_u8(&err),
        crate::exit_code::code::INPUT_ERROR,
        "a file at .cake must classify as an input error"
    );
}

#[cfg(unix)]
#[test]
fn initialize_refuses_when_dot_cake_is_a_symlink() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::TempDir::new().unwrap();
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    symlink(&outside, tmp.path().join(".cake")).unwrap();

    let err = initialize(tmp.path(), false).unwrap_err();

    assert!(
        err.downcast_ref::<InitError>().is_some(),
        "a symlink at .cake must be reported as a conflict"
    );
    assert!(
        !outside.join("settings.toml").exists(),
        "writes must not follow a .cake symlink outside the project"
    );
}

#[cfg(unix)]
#[test]
fn initialize_refuses_dangling_settings_symlink_without_following_it() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::create_dir(tmp.path().join(".cake")).unwrap();
    // A dangling symlink at the settings target: `exists()` is false, but the
    // exclusive create must refuse instead of writing through the link.
    symlink("nowhere", tmp.path().join(".cake").join("settings.toml")).unwrap();

    let err = initialize(tmp.path(), false).unwrap_err();

    assert!(
        err.downcast_ref::<InitError>().is_some(),
        "a dangling settings symlink must be reported as a conflict"
    );
}

#[cfg(unix)]
#[test]
fn initialize_refuses_dangling_hooks_symlink_without_partial_writes() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::create_dir(tmp.path().join(".cake")).unwrap();
    // A dangling symlink at the hooks target: `exists()` is false, but the
    // path is occupied, so the preflight must refuse before the settings
    // file is written (settings is the first write and would otherwise
    // survive the conflict).
    symlink(
        "nowhere",
        tmp.path().join(".cake").join("hooks.json.example"),
    )
    .unwrap();

    let err = initialize(tmp.path(), true).unwrap_err();

    assert!(
        err.downcast_ref::<InitError>().is_some(),
        "a dangling hooks symlink must be reported as a conflict"
    );
    assert!(
        !tmp.path().join(".cake").join("settings.toml").exists(),
        "no target may be written when the hooks target is a dangling symlink"
    );
}

// =============================================================================
// Created-target output
// =============================================================================

#[test]
fn init_outcome_display_names_created_targets() {
    let outcome = InitOutcome {
        settings: ".cake/settings.toml".to_string(),
        hooks_example: Some(".cake/hooks.json.example".to_string()),
    };
    assert_eq!(
        outcome.to_string(),
        "Created .cake/settings.toml\nCreated .cake/hooks.json.example\n"
    );
}
