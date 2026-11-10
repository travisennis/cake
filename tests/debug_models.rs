//! Integration tests for `cake debug` inspection commands.

#![expect(clippy::expect_used, reason = "test code uses expect for assertions")]

mod support;
#[path = "support/trust.rs"]
mod trust_helpers;

use std::process::Stdio;

use support::TestEnv;

fn cake_env() -> TestEnv {
    TestEnv::new("cake-debug-models-test")
}

#[cfg(unix)]
#[test]
fn debug_inspection_deleted_current_directory_is_an_input_error() {
    for subcommand in ["skills", "models", "grants", "config"] {
        let env = cake_env();
        let output = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "rmdir \"$1\"; exec \"$2\" debug \"$3\"",
                "deleted-cwd",
            ])
            .arg(&env.workspace_dir)
            .arg(env.command().get_program())
            .arg(subcommand)
            .current_dir(&env.workspace_dir)
            .env("CAKE_DATA_DIR", &env.data_dir)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(3),
            "{subcommand}: {:?}",
            output.stderr
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("Failed to get current directory")
        );
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn debug_skills_respects_profiles_and_cli_filters_without_a_model() {
    let env = cake_env();
    for name in ["short", "long"] {
        let dir = env.workspace_dir.join(".agents/skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: A routing description\n---\nBody\n"),
        )
        .unwrap();
    }
    env.write_project_settings("[profiles.review.skills]\nonly = [\"short\"]\n");
    let output = env
        .command()
        .args([
            "--profile",
            "review",
            "debug",
            "skills",
            "--catalog-budget",
            "1",
            "--description-budget",
            "1",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(report.contains("Selected: 1 skill"));
    assert!(report.contains("short"));
    assert!(report.contains("project"));
    assert!(report.contains("Descriptions over budget: 1"));
    assert!(report.contains("over"));
    assert!(report.contains("WARNING: rendered catalog still exceeds"));
    assert!(report.contains("Skills omitted: 1"));
    assert!(report.contains("Omissions assume SkillSearch is available."));
    assert!(!report.contains("long"));
    assert_eq!(
        std::fs::read_dir(env.data_dir.join("sessions"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn debug_skills_no_skills_prints_explicit_empty_state() {
    let env = cake_env();
    let output = env
        .command()
        .args(["debug", "skills", "--no-skills"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(report.contains("No skills selected."));
    assert!(report.contains("Diagnostics: none"));
}

#[test]
fn debug_skills_missing_requested_name_prints_diagnostic() {
    let env = cake_env();
    let output = env
        .command()
        .args(["debug", "skills", "--skills=absent"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(report.contains("No skills selected."));
    assert!(report.contains("Requested skill 'absent' was not found among discovered skills"));
}

#[test]
fn debug_skills_reports_scope_and_location_in_name_order() {
    let env = cake_env();
    let project_dir = env.workspace_dir.join(".agents/skills/alpha-project");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("SKILL.md"),
        "---\nname: alpha-project\ndescription: A project skill\n---\n",
    )
    .unwrap();

    let user_dir = env.home_dir.join(".agents/skills/beta-user");
    std::fs::create_dir_all(&user_dir).unwrap();
    std::fs::write(
        user_dir.join("SKILL.md"),
        "---\nname: beta-user\ndescription: A user skill\n---\n",
    )
    .unwrap();

    let configured_dir = env.workspace_dir.join("configured-skills/gamma-configured");
    std::fs::create_dir_all(&configured_dir).unwrap();
    std::fs::write(
        configured_dir.join("SKILL.md"),
        "---\nname: gamma-configured\ndescription: A configured skill\n---\n",
    )
    .unwrap();
    env.write_global_settings(&format!(
        "[skills]\npath = \"{}\"\n",
        configured_dir
            .parent()
            .expect("configured skill directory")
            .display()
    ));

    let output = env.command().args(["debug", "skills"]).output().unwrap();

    assert!(output.status.success(), "{:?}", output.stderr);
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(report.contains("Selected: 3 skills"));
    assert!(report.contains("project"));
    assert!(report.contains("configured"));
    assert!(report.contains("user"));
    assert!(report.contains(&project_dir.join("SKILL.md").display().to_string()));
    assert!(report.contains(&configured_dir.join("SKILL.md").display().to_string()));
    assert!(report.contains(&user_dir.join("SKILL.md").display().to_string()));
    assert!(
        report.find("alpha-project").expect("project row")
            < report.find("beta-user").expect("user row")
    );
    assert!(
        report.find("beta-user").expect("user row")
            < report.find("gamma-configured").expect("configured row")
    );
}

#[test]
fn debug_skills_reports_malformed_skill_diagnostics_on_stdout() {
    let env = cake_env();
    let broken_dir = env.workspace_dir.join(".agents/skills/broken");
    std::fs::create_dir_all(&broken_dir).unwrap();
    let broken = broken_dir.join("SKILL.md");
    std::fs::write(&broken, "missing frontmatter\n").unwrap();

    let output = env.command().args(["debug", "skills"]).output().unwrap();

    assert!(output.status.success(), "{:?}", output.stderr);
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(report.contains("No skills selected."));
    assert!(report.contains("Diagnostics\n"));
    assert!(report.contains("error:"));
    assert!(report.contains(&broken.display().to_string()));
    assert!(report.contains("missing YAML frontmatter"));
    assert!(!report.contains("Skill diagnostic ("));
}

#[test]
fn debug_skills_help_lists_json_output() {
    let env = cake_env();
    let output = env
        .command()
        .args(["debug", "skills", "--help"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--json"));
    assert!(stdout.contains("cake debug skills --json"));
}

#[test]
fn debug_models_prints_project_models_without_api_key_value() {
    let env = cake_env();
    env.write_global_settings(
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
fn debug_models_no_gate_accepts_invalid_gate_configuration() {
    let env = cake_env();
    env.write_project_settings(
        r#"
[tools.bash]
gates = ["missing"]
"#,
    );
    env.trust_project();

    let output = env.command().args(["debug", "models"]).output().unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("selected gate 'missing' has no [gates.missing] table")
    );

    let output = env
        .command()
        .args(["--no-gate", "debug", "models"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "No models configured.\n"
    );
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

    assert_eq!(output.status.code(), Some(3));
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
