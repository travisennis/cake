//! Integration tests for `cake debug grants`.

#![expect(clippy::expect_used, reason = "test code uses expect for assertions")]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use support::TestEnv;

fn grants_env() -> TestEnv {
    TestEnv::new("cake-debug-grants-test")
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

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path)
        .unwrap_or_else(|error| panic!("path should canonicalize: {}: {error}", path.display()))
}

fn canonical_workspace(env: &TestEnv) -> PathBuf {
    canonical(&env.workspace_dir)
}

fn source<'a>(document: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    document["data"]["sources"]
        .as_array()
        .expect("sources array")
        .iter()
        .find(|source| source["id"] == id)
        .unwrap_or_else(|| panic!("source '{id}' should exist"))
}

fn assert_path_in(paths: &serde_json::Value, expected: &Path) {
    let expected = expected.display().to_string();
    assert!(
        paths
            .as_array()
            .expect("paths array")
            .iter()
            .any(|path| path == &expected),
        "expected {expected} in {paths}"
    );
}

#[test]
fn debug_grants_text_reports_policy_sources_and_effective_sets() {
    let env = grants_env();

    let output = run(&env, &["debug", "grants"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let report = stdout(&output);
    assert!(report.contains("Effective grants"));
    assert!(report.contains("Policy: workspace-write (from default)"));
    assert!(report.contains("OS sandbox: applied"));
    assert!(report.contains("Working directory"));
    assert!(report.contains("Configured writable directories"));
    assert!(report.contains("read_write ("));
    assert!(report.contains("read_only ("));
    assert!(report.contains("sandbox_writable ("));
    assert!(report.contains("user_grants ("));
    assert!(report.contains("Bash writable: read_write + sandbox_writable"));
    assert!(report.contains("Bash read-execute:"));
    assert!(report.contains("Diagnostics: none"));
    assert_eq!(
        fs::read_dir(env.data_dir.join("sessions")).unwrap().count(),
        0,
        "a diagnostic command must not create a session"
    );
}

#[test]
fn debug_grants_json_is_one_deterministic_document() {
    let env = grants_env();
    fs::create_dir_all(env.home_dir.join(".config/cake")).expect("create global config directory");
    let workspace = canonical_workspace(&env);

    let first = run(&env, &["debug", "grants", "--json"]);
    let second = run(&env, &["debug", "grants", "--json"]);

    assert_eq!(first.status.code(), Some(0), "stderr: {}", stderr(&first));
    assert_eq!(second.status.code(), Some(0), "stderr: {}", stderr(&second));
    assert_eq!(
        stdout(&first),
        stdout(&second),
        "output must be byte-stable"
    );
    assert_eq!(stderr(&first), "");
    let document = parse_document(&first);
    assert_grants_json_document(&env, &workspace, &document);
}

fn assert_grants_json_document(env: &TestEnv, workspace: &Path, document: &serde_json::Value) {
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["command"], "debug grants");
    assert_eq!(document["status"], "ok");
    assert_eq!(document["summary"]["policy"], "workspace-write");
    assert_eq!(document["summary"]["policy_source"], "default");
    assert_eq!(document["summary"]["sandbox_enforced"], true);
    assert_eq!(document["summary"]["source_count"], 15);
    assert_eq!(document["checks"], serde_json::json!([]));
    assert_path_in(&document["data"]["read_write"], workspace);
    assert_eq!(
        source(document, "user_skill_root")["entries"][0]["status"],
        "would-create"
    );
    assert!(
        !document["data"]["read_write"]
            .as_array()
            .expect("read_write array")
            .iter()
            .any(|path| path == &env.home_dir.join(".agents/skills").display().to_string()),
        "a missing built-in grant must not be presented as effective"
    );
    assert_path_in(
        &document["data"]["read_only"],
        &canonical(&env.home_dir.join(".config/cake")),
    );
    assert_eq!(document["data"]["project_trust"]["state"], "not_required");
    assert_eq!(
        source(document, "working_directory")["entries"][0]["path"],
        workspace.display().to_string()
    );
    assert_eq!(
        source(document, "credential_deny_list")["entries"][0]["status"],
        "denied"
    );
}

#[test]
fn debug_grants_local_flags_and_profile_change_the_report() {
    let env = grants_env();
    let shared = env.workspace_dir.join("shared");
    let shared_skills = env.workspace_dir.join("shared-skills");
    fs::create_dir_all(&shared).expect("create shared directory");
    fs::create_dir_all(&shared_skills).expect("create shared skills directory");
    env.write_global_settings(&format!(
        "[profiles.shared]\ndirectories = [\"{}\"]\n",
        shared.display()
    ));

    let output = run(
        &env,
        &[
            "--profile",
            "shared",
            "debug",
            "grants",
            "--sandbox",
            "read-only",
            "--add-dir",
            "shared",
            "--json",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["summary"]["policy"], "read-only");
    assert_eq!(document["summary"]["policy_source"], "--sandbox");
    assert_eq!(document["summary"]["sandbox_writable_count"], 0);
    assert_path_in(&document["data"]["read_only"], &canonical(&shared));
    assert!(
        !document["data"]["read_write"]
            .as_array()
            .expect("read_write array")
            .iter()
            .any(|path| path == &canonical_workspace(&env).display().to_string())
    );
    assert_eq!(
        source(&document, "add_dir")["entries"][0]["path"],
        canonical(&shared).display().to_string()
    );
    assert_eq!(
        source(&document, "add_dir")["entries"][0]["status"],
        "granted"
    );
}

#[test]
fn debug_grants_skills_flags_match_the_selected_catalog() {
    let env = grants_env();
    for name in ["alpha", "zeta"] {
        let directory = env.workspace_dir.join(".agents/skills").join(name);
        fs::create_dir_all(&directory).expect("create skill directory");
        fs::write(
            directory.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: A routing description\n---\n"),
        )
        .expect("write skill");
    }

    let output = run(&env, &["debug", "grants", "--json", "--skills=alpha"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    let entries = source(&document, "skill_directories")["entries"]
        .as_array()
        .expect("skill entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0]["path"],
        canonical(&env.workspace_dir.join(".agents/skills/alpha"))
            .display()
            .to_string()
    );

    let output = run(&env, &["debug", "grants", "--json", "--no-skills"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(
        source(&document, "skill_directories")["entries"],
        serde_json::json!([])
    );
}

#[test]
fn debug_grants_reports_missing_paths_without_creating_them() {
    let env = grants_env();
    let missing = env.workspace_dir.join("missing/parent/cache");
    env.write_global_settings(&format!(
        "[sandbox]\nwritable = [\"{}\"]\n",
        missing.display()
    ));

    let output = run(&env, &["debug", "grants", "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    let document = parse_document(&output);
    assert_eq!(document["status"], "warning");
    let entry = &source(&document, "sandbox_writable")["entries"][0];
    assert_eq!(entry["status"], "would-create");
    assert!(
        entry["detail"]
            .as_str()
            .expect("would-create detail")
            .contains("would create it and grant it")
    );
    assert!(
        document["checks"]
            .as_array()
            .expect("checks array")
            .iter()
            .any(|check| check["id"] == "grants.path_problem")
    );
    assert!(
        !missing.exists(),
        "the diagnostic must not create the grant"
    );
    assert!(
        !env.home_dir.join(".agents/skills").exists(),
        "the diagnostic must not create the user skill root"
    );
}

#[test]
fn debug_grants_excludes_untrusted_project_authority() {
    let env = grants_env();
    let project_grant = env.workspace_dir.join("project-grant");
    fs::create_dir_all(&project_grant).expect("create project grant");
    env.write_project_settings(&format!(
        "[sandbox]\nwritable = [\"{}\"]\n",
        project_grant.display()
    ));

    let output = run(&env, &["debug", "grants", "--json"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    let document = parse_document(&output);
    assert_eq!(document["data"]["project_trust"]["state"], "untrusted");
    assert_eq!(
        source(&document, "sandbox_writable")["entries"],
        serde_json::json!([])
    );
    assert!(
        !document["data"]["read_write"]
            .as_array()
            .expect("read_write array")
            .iter()
            .any(|path| path == &project_grant.display().to_string())
    );

    let models = run(&env, &["debug", "models", "--json"]);
    assert_eq!(models.status.code(), Some(0), "stderr: {}", stderr(&models));
    assert_eq!(
        document["data"]["project_trust"],
        parse_document(&models)["data"]["project_trust"],
        "the trust object must match debug models"
    );

    let output = run(&env, &["debug", "grants"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("project configuration is untrusted"),
        "text mode must print the project-trust warning: {}",
        stderr(&output)
    );
    assert!(!stdout(&output).contains(&project_grant.display().to_string()));
}

#[test]
fn debug_grants_reports_cake_sandbox_environment_source() {
    let env = grants_env();
    let output = env
        .command()
        .args(["debug", "grants", "--json"])
        .env("CAKE_SANDBOX", "off")
        .output()
        .expect("failed to execute cake");

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["summary"]["policy"], "danger-full-access");
    assert_eq!(document["summary"]["policy_source"], "CAKE_SANDBOX");
    assert_eq!(document["summary"]["sandbox_enforced"], false);
}

#[test]
fn debug_grants_json_reports_settings_warnings_in_checks() {
    let env = grants_env();
    env.write_global_settings("[unrecognized]\ntypo_key = true\n");

    let output = run(&env, &["debug", "grants", "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    let document = parse_document(&output);
    assert_eq!(document["status"], "warning");
    assert!(
        document["checks"]
            .as_array()
            .expect("checks array")
            .iter()
            .any(|check| check["id"] == "settings.unknown_key")
    );

    let output = run(&env, &["debug", "grants"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(stderr(&output).contains("warning:"));
}

#[test]
fn debug_grants_settings_failure_is_an_error_document() {
    let env = grants_env();
    env.write_project_settings("not valid toml = [");

    let output = run(&env, &["debug", "grants", "--json"]);

    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["command"], "debug grants");
    assert_eq!(document["status"], "error");
    assert_eq!(document["checks"][0]["id"], "settings.load_failed");
}

#[test]
fn debug_grants_help_lists_local_flags_and_examples() {
    let env = grants_env();
    let output = run(&env, &["debug", "grants", "--help"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let help = stdout(&output);
    for flag in [
        "--sandbox",
        "--add-dir",
        "--skills",
        "--no-skills",
        "--json",
    ] {
        assert!(help.contains(flag), "help must list {flag}: {help}");
    }
    assert!(help.contains("cake debug grants --sandbox read-only --add-dir"));

    let output = run(&env, &["--help"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("cake debug grants --json"));
}
