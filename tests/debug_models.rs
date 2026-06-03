//! Integration tests for `cake debug models`.

#![expect(clippy::expect_used, reason = "test code uses expect for assertions")]

mod support;

use std::process::Stdio;

use support::TestEnv;

fn cake_env() -> TestEnv {
    TestEnv::new("cake-debug-models-test")
}

#[test]
fn debug_models_prints_project_models_without_api_key_value() {
    let env = cake_env();
    env.write_project_settings(
        r#"
[[models]]
name = "zen"
model = "glm-5.1"
base_url = "https://example.com/v1"
api_key_env = "SECRET_TOKEN"
api_type = "responses"
"#,
    );

    let output = env
        .command()
        .args(["debug", "models"])
        .env("SECRET_TOKEN", "actual-secret-value")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to execute command");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Configured Models"));
    assert!(stdout.contains("zen"));
    assert!(stdout.contains("glm-5.1"));
    assert!(stdout.contains("https://example.com/v1"));
    assert!(stdout.contains("responses"));
    assert!(stdout.contains("SECRET_TOKEN"));
    assert!(!stdout.contains("actual-secret-value"));
}

#[test]
fn debug_models_uses_global_settings_without_project_settings() {
    let env = cake_env();
    let home = tempfile::tempdir().expect("failed to create temp home");
    let settings_dir = home.path().join(".config").join("cake");
    std::fs::create_dir_all(&settings_dir).expect("failed to create global cake config directory");
    std::fs::write(
        settings_dir.join("settings.toml"),
        r#"
[[models]]
name = "global"
model = "provider/global"
base_url = "https://global.example.com/v1"
api_key_env = "GLOBAL_TOKEN"
"#,
    )
    .expect("failed to write global settings.toml");

    let output = env
        .command()
        .args(["debug", "models"])
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to execute command");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("global"));
    assert!(stdout.contains("provider/global"));
    assert!(stdout.contains("chat_completions"));
}

#[test]
fn debug_models_prints_helpful_message_when_no_models_are_configured() {
    let env = cake_env();
    let output = env
        .command()
        .args(["debug", "models"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to execute command");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "No models configured.\n");
}

#[test]
fn debug_models_reports_settings_syntax_errors() {
    let env = cake_env();
    env.write_project_settings("not valid toml = [");

    let output = env
        .command()
        .args(["debug", "models"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to execute command");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Failed to parse settings file"));
}

#[test]
fn debug_help_lists_models_subcommand() {
    let env = cake_env();
    let output = env
        .command()
        .args(["debug", "--help"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to execute command");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("models"));
}

#[test]
fn top_level_help_lists_debug_subcommand() {
    let env = cake_env();
    let output = env
        .command()
        .arg("--help")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("Failed to execute command");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("debug"));
}
