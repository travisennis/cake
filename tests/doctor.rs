//! Integration tests for `cake doctor`.

#![expect(clippy::expect_used, reason = "test code uses expect for assertions")]

mod support;

use std::fs;
use std::process::Stdio;

use support::TestEnv;

const BASE_SETTINGS: &str = r#"default_model = "zen"

[[models]]
name = "zen"
model = "glm-5.1"
base_url = "https://example.invalid/v1"
api_key_env = "DOCTOR_TEST_KEY"
"#;

fn doctor_env() -> TestEnv {
    TestEnv::new("cake-doctor-test")
}

fn run(env: &TestEnv, args: &[&str]) -> std::process::Output {
    env.command()
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake")
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn parse_document(output: &std::process::Output) -> serde_json::Value {
    serde_json::from_str(&stdout(output)).expect("stdout must be one JSON document")
}

fn assert_sessions_not_created(env: &TestEnv) {
    assert_eq!(
        fs::read_dir(env.data_dir.join("sessions"))
            .expect("sessions directory")
            .count(),
        0,
        "a diagnostic command must not create a session"
    );
}

fn grant_paths(grants: &serde_json::Value, section: &str) -> Vec<String> {
    grants[section]
        .as_array()
        .expect("grant array")
        .iter()
        .map(|entry| entry["path"].as_str().expect("grant path").to_string())
        .collect()
}

#[test]
fn doctor_reports_health_and_merged_configured_grants() {
    let env = doctor_env();
    let writable = env.workspace_dir.join("cache");
    let read_only = env.workspace_dir.join("tools");
    let project_read_only = env.workspace_dir.join("project-tools");
    for path in [&writable, &read_only, &project_read_only] {
        fs::create_dir_all(path).expect("create grant directory");
    }
    env.write_global_settings(&format!(
        "{BASE_SETTINGS}\n[sandbox]\nwritable = [\"{}\"]\nread_only = [\"{}\"]\n",
        writable.display(),
        read_only.display()
    ));
    env.write_project_settings(&format!(
        "[sandbox]\nread_only = [\"{}\"]\n",
        project_read_only.display()
    ));

    let output = run(&env, &["--trust-project", "doctor"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    let report = stdout(&output);
    for expected in [
        "Settings health",
        "Model: zen",
        "Status: ok",
        "Configured grants",
        "Policy: workspace-write (from default)",
        "not the complete OS-level sandbox policy",
    ] {
        assert!(report.contains(expected), "{expected}: {report}");
    }
    for (source, path) in [
        ("sandbox_writable", &writable),
        ("sandbox_read_only", &read_only),
        ("sandbox_read_only", &project_read_only),
    ] {
        assert!(
            report.contains(&format!("[{source}] {} (granted)", path.display())),
            "{source} {}: {report}",
            path.display()
        );
    }
    assert_sessions_not_created(&env);
}

#[test]
fn doctor_json_is_one_deterministic_document() {
    let env = doctor_env();
    let writable = env.workspace_dir.join("cache");
    fs::create_dir_all(&writable).expect("create writable directory");
    env.write_global_settings(&format!(
        "{BASE_SETTINGS}\n[sandbox]\nwritable = [\"{}\"]\n",
        writable.display()
    ));

    let first = run(&env, &["doctor", "--json"]);
    let second = run(&env, &["doctor", "--json"]);

    assert_eq!(first.status.code(), Some(0), "stderr: {}", stderr(&first));
    assert_eq!(second.status.code(), Some(0), "stderr: {}", stderr(&second));
    assert_eq!(
        stdout(&first),
        stdout(&second),
        "output must be byte-stable"
    );
    assert_eq!(stderr(&first), "");

    let document = parse_document(&first);
    assert_json_document(&writable, &document);
    assert_sessions_not_created(&env);
}

fn assert_json_document(writable: &std::path::Path, document: &serde_json::Value) {
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["command"], "doctor");
    assert_eq!(document["status"], "ok");
    assert_eq!(document["checks"], serde_json::json!([]));
    assert_eq!(document["summary"]["model"], "zen");
    assert_eq!(document["summary"]["policy"], "workspace-write");
    assert_eq!(document["summary"]["configured_writable_count"], 1);
    assert_eq!(
        grant_paths(&document["data"]["grants"], "writable"),
        vec![writable.display().to_string()]
    );
    assert_eq!(
        document["data"]["grants"]["writable"][0]["status"],
        "granted"
    );
    assert_eq!(document["data"]["project_trust"]["state"], "not_required");
    assert_eq!(document["data"]["settings_sources"][0]["origin"], "global");
    assert_eq!(document["data"]["settings_sources"][0]["present"], true);
}

#[test]
fn doctor_reports_unknown_settings_keys_as_findings() {
    let env = doctor_env();
    env.write_global_settings(&format!(
        "{BASE_SETTINGS}\n[sandbox]\nread_only = []\ntypo_key = true\n"
    ));

    let output = run(&env, &["doctor", "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    let document = parse_document(&output);
    assert_eq!(document["status"], "warning");
    let check = document["checks"]
        .as_array()
        .expect("checks array")
        .iter()
        .find(|check| check["id"] == "settings.unknown_key")
        .expect("settings.unknown_key finding");
    let message = check["message"].as_str().expect("message");
    assert!(message.contains("typo_key"), "{check}");
    assert!(
        message.contains(
            &env.home_dir
                .join(".config/cake/settings.toml")
                .display()
                .to_string()
        ),
        "the finding must name the source file: {check}"
    );
}

#[test]
fn doctor_reports_malformed_settings_with_the_source_path() {
    let env = doctor_env();
    env.write_project_settings("not valid toml = [");

    let output = run(&env, &["doctor", "--json"]);

    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["command"], "doctor");
    assert_eq!(document["status"], "error");
    assert_eq!(document["checks"][0]["id"], "settings.load_failed");
    let failed = document["data"]["failed_source"]
        .as_str()
        .expect("failed source");
    assert!(failed.ends_with(".cake/settings.toml"), "{failed}");
    assert!(
        document["checks"][0]["message"]
            .as_str()
            .expect("message")
            .contains(failed),
        "{document}"
    );
    let source = &document["data"]["settings_sources"][1];
    assert_eq!(source["origin"], "project");
    assert_eq!(source["present"], true);

    let output = run(&env, &["doctor"]);
    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains(".cake/settings.toml"),
        "text mode must name the source file: {}",
        stderr(&output)
    );
}

#[test]
fn doctor_reports_an_invalid_default_model_reference() {
    let env = doctor_env();
    env.write_global_settings(
        r#"default_model = "missing"

[[models]]
name = "zen"
model = "glm-5.1"
base_url = "https://example.invalid/v1"
api_key_env = "DOCTOR_TEST_KEY"
"#,
    );

    let output = run(&env, &["doctor", "--json"]);

    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["checks"][0]["id"], "settings.load_failed");
    let message = document["checks"][0]["message"].as_str().expect("message");
    assert!(
        message.contains("Default model 'missing' not found"),
        "{message}"
    );
    assert!(message.contains("settings sources:"), "{message}");
    assert_eq!(document["data"]["failed_source"], serde_json::Value::Null);
}

#[test]
fn doctor_reports_an_unknown_profile_reference() {
    let env = doctor_env();
    env.write_global_settings(BASE_SETTINGS);

    let output = run(&env, &["--profile", "missing", "doctor", "--json"]);

    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["status"], "error");
    assert!(
        document["checks"][0]["message"]
            .as_str()
            .expect("message")
            .contains("Unknown profile 'missing'"),
        "{document}"
    );
}

#[test]
fn doctor_reports_missing_grants_without_creating_them() {
    let env = doctor_env();
    let missing_writable = env.workspace_dir.join("missing/cache");
    let missing_read_only = env.workspace_dir.join("missing/tools");
    env.write_global_settings(&format!(
        "{BASE_SETTINGS}\n[sandbox]\nwritable = [\"{}\"]\nread_only = [\"{}\"]\n",
        missing_writable.display(),
        missing_read_only.display()
    ));

    let output = run(&env, &["doctor", "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["status"], "warning");
    assert!(
        document["checks"]
            .as_array()
            .expect("checks")
            .iter()
            .any(|check| check["id"] == "grants.path_problem"
                && check["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("missing/cache"))),
        "{document}"
    );
    assert_eq!(
        document["data"]["grants"]["writable"][0]["status"],
        "would-create"
    );
    assert_eq!(
        document["data"]["grants"]["read_only"][0]["status"],
        "missing"
    );
    assert!(!missing_writable.exists(), "doctor must not create a grant");
    assert!(
        !missing_read_only.exists(),
        "doctor must not create a grant"
    );
}

#[test]
fn doctor_help_documents_the_command_and_json_output() {
    let env = doctor_env();

    let output = run(&env, &["doctor", "--help"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("--json"));
}
