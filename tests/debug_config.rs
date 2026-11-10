//! Integration tests for `cake debug config`.

#![expect(clippy::expect_used, reason = "test code uses expect for assertions")]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use support::TestEnv;

fn config_env() -> TestEnv {
    TestEnv::new("cake-debug-config-test")
}

fn run(env: &TestEnv, args: &[&str]) -> std::process::Output {
    env.command()
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake")
}

fn run_with_key(env: &TestEnv, args: &[&str], secret: &str) -> std::process::Output {
    env.command()
        .args(args)
        .env("CONFIG_TEST_KEY", secret)
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

fn write_model_settings(env: &TestEnv) {
    env.write_global_settings(
        r#"default_model = "zen"

[[models]]
name = "zen"
model = "glm-5.1"
base_url = "https://example.invalid/v1"
api_key_env = "CONFIG_TEST_KEY"
"#,
    );
}

fn write_skill(env: &TestEnv, name: &str) {
    let directory = env.workspace_dir.join(".agents/skills").join(name);
    fs::create_dir_all(&directory).expect("create skill directory");
    fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: A routing description\n---\n"),
    )
    .expect("write skill");
}

fn write_hooks(path: &Path, command: &str) {
    fs::create_dir_all(path.parent().expect("hooks parent")).expect("create hooks directory");
    let hooks = serde_json::json!({
        "version": 1,
        "hooks": {
            "PreToolUse": [{
                "matcher": "Bash",
                "hooks": [{
                    "type": "command",
                    "command": command,
                    "fail_closed": true
                }]
            }]
        }
    });
    fs::write(
        path,
        serde_json::to_vec_pretty(&hooks).expect("serialize hooks"),
    )
    .expect("write hooks");
}

#[cfg(unix)]
fn write_executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::create_dir_all(path.parent().expect("tool parent")).expect("create tool directory");
    fs::write(path, body).expect("write tool");
    let mut permissions = fs::metadata(path).expect("tool metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make tool executable");
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path)
        .unwrap_or_else(|error| panic!("path should canonicalize: {}: {error}", path.display()))
}

#[test]
#[expect(
    clippy::cognitive_complexity,
    reason = "the test asserts the full human-readable report shape"
)]
fn debug_config_text_reports_effective_sections_without_the_credential() {
    let env = config_env();
    fs::create_dir_all(env.home_dir.join(".config/cake")).expect("create global config directory");
    write_model_settings(&env);
    write_skill(&env, "alpha");
    fs::create_dir_all(env.workspace_dir.join(".cake")).expect("create .cake");
    fs::write(
        env.workspace_dir.join(".cake/system.md"),
        "PROJECT PROMPT BODY MUST NOT BE PRINTED",
    )
    .expect("write project prompt");

    let output = run_with_key(&env, &["debug", "config"], "actual-secret-value");

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let report = stdout(&output);
    assert!(report.contains("Effective configuration"));
    assert!(report.contains("Selected: zen (from default_model)"));
    assert!(report.contains("Credential: environment CONFIG_TEST_KEY (present)"));
    assert!(report.contains("Settings sources"));
    assert!(report.contains("Project trust: not_required (mode inspect)"));
    assert!(report.contains("System prompt\n  Source: project"));
    assert!(report.contains("Policy: workspace-write (from default)"));
    assert!(report.contains("Effective sets:"));
    assert!(report.contains("Tools\n"));
    assert!(report.contains("Skills: all (default); selected 1, filtered 0"));
    assert!(report.contains("Hooks\n"));
    assert!(report.contains("Toolbox (describe probing skipped)"));
    assert!(!report.contains("actual-secret-value"));
    assert!(!report.contains("PROJECT PROMPT BODY MUST NOT BE PRINTED"));
    assert_eq!(stderr(&output), "");
    assert_eq!(
        fs::read_dir(env.data_dir.join("sessions")).unwrap().count(),
        0,
        "a diagnostic command must not create a session"
    );
}

#[test]
fn debug_config_json_is_one_deterministic_document() {
    let env = config_env();
    fs::create_dir_all(env.home_dir.join(".config/cake")).expect("create global config directory");
    write_model_settings(&env);
    write_skill(&env, "alpha");

    let first = run_with_key(&env, &["debug", "config", "--json"], "actual-secret-value");
    let second = run_with_key(&env, &["debug", "config", "--json"], "actual-secret-value");

    assert_eq!(first.status.code(), Some(0), "stderr: {}", stderr(&first));
    assert_eq!(
        stdout(&first),
        stdout(&second),
        "output must be byte-stable"
    );
    assert_eq!(stderr(&first), "");
    let document = parse_document(&first);
    assert_config_json_document(&env, &document);
    assert!(!stdout(&first).contains("actual-secret-value"));
}

#[expect(
    clippy::cognitive_complexity,
    reason = "the helper asserts the full machine-readable report shape"
)]
fn assert_config_json_document(env: &TestEnv, document: &serde_json::Value) {
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["command"], "debug config");
    assert_eq!(document["status"], "ok");
    assert_eq!(document["summary"]["model"], "zen");
    assert_eq!(document["summary"]["model_configured"], true);
    assert_eq!(document["summary"]["credential_present"], true);
    assert_eq!(document["summary"]["policy"], "workspace-write");
    assert_eq!(document["summary"]["policy_source"], "default");
    assert_eq!(document["summary"]["settings_source_count"], 2);
    assert_eq!(document["summary"]["skill_count"], 1);
    assert_eq!(document["summary"]["warning_count"], 0);
    assert_eq!(document["checks"], serde_json::json!([]));
    assert_eq!(document["data"]["model"]["name"], "zen");
    assert_eq!(document["data"]["model"]["source"], "default_model");
    assert_eq!(
        document["data"]["model"]["credential"]["name"],
        "CONFIG_TEST_KEY"
    );
    assert_eq!(document["data"]["model"]["credential"]["present"], true);
    assert_eq!(document["data"]["settings_sources"][0]["origin"], "global");
    assert_eq!(document["data"]["settings_sources"][0]["present"], true);
    assert_eq!(document["data"]["settings_sources"][1]["origin"], "project");
    assert_eq!(document["data"]["settings_sources"][1]["present"], false);
    assert_eq!(document["data"]["project_trust"]["state"], "not_required");
    assert_eq!(document["data"]["project_trust"]["mode"], "inspect");
    assert_eq!(document["data"]["system_prompt"]["source"], "builtin");
    assert_eq!(document["data"]["tools"]["source"], "default");
    assert_eq!(
        document["data"]["tools"]["builtin"],
        serde_json::json!(["Bash", "BashSession", "Edit", "Read", "Write"])
    );
    assert_eq!(document["data"]["skills"]["selected"][0]["name"], "alpha");
    assert_eq!(
        document["data"]["skills"]["filtered"],
        serde_json::json!([])
    );
    assert_eq!(document["data"]["hooks"]["commands_redacted"], true);
    assert_eq!(document["data"]["toolbox"]["describe_probed"], false);
    assert!(
        document["data"]["grants"]["read_write"]
            .as_array()
            .expect("read_write array")
            .iter()
            .any(|path| path == &canonical(&env.workspace_dir).display().to_string())
    );
}

#[test]
#[expect(
    clippy::cognitive_complexity,
    reason = "the test asserts the full machine-readable report shape"
)]
fn debug_config_local_flags_and_profile_change_the_report() {
    let env = config_env();
    let shared = env.workspace_dir.join("shared");
    fs::create_dir_all(&shared).expect("create shared directory");
    write_skill(&env, "alpha");
    write_skill(&env, "zeta");
    env.write_global_settings(&format!(
        r#"default_model = "zen"

[[models]]
name = "zen"
model = "glm-5.1"
base_url = "https://example.invalid/v1"
api_key_env = "CONFIG_TEST_KEY"

[profiles.shared]
directories = ["{}"]
"#,
        shared.display()
    ));

    let output = run_with_key(
        &env,
        &[
            "--profile",
            "shared",
            "debug",
            "config",
            "--sandbox",
            "read-only",
            "--add-dir",
            "shared",
            "--skills=alpha",
            "--tools=Read",
            "--json",
        ],
        "actual-secret-value",
    );

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["summary"]["profile"], "shared");
    assert_eq!(document["summary"]["policy"], "read-only");
    assert_eq!(document["summary"]["policy_source"], "--sandbox");
    assert_eq!(document["summary"]["skill_count"], 1);
    assert_eq!(document["summary"]["filtered_skill_count"], 1);
    assert_eq!(document["data"]["skills"]["selection"], "only");
    assert_eq!(document["data"]["skills"]["reason"], "--skills");
    assert_eq!(document["data"]["skills"]["selected"][0]["name"], "alpha");
    assert_eq!(document["data"]["skills"]["filtered"][0]["name"], "zeta");
    assert_eq!(document["data"]["tools"]["source"], "--tools");
    assert_eq!(
        document["data"]["tools"]["selected"],
        serde_json::json!(["Read"])
    );
    assert_eq!(
        document["data"]["tools"]["builtin"],
        serde_json::json!(["Read"])
    );
    assert_eq!(
        document["data"]["settings_sources"][0]["path"],
        env.home_dir
            .join(".config/cake/settings.toml")
            .display()
            .to_string()
    );
    assert!(
        document["data"]["grants"]["read_only"]
            .as_array()
            .expect("read_only array")
            .iter()
            .any(|path| path == &canonical(&shared).display().to_string())
    );
}

#[test]
#[expect(
    clippy::cognitive_complexity,
    reason = "the test asserts trust, hooks, grants, and toolbox boundaries together"
)]
fn debug_config_excludes_untrusted_project_authority_without_running_it() {
    let env = config_env();
    let project_grant = env.workspace_dir.join("project-grant");
    env.write_project_settings(&format!(
        "[sandbox]\nwritable = [\"{}\"]\n",
        project_grant.display()
    ));
    let hook_marker = env.workspace_dir.join("hook-ran");
    write_hooks(
        &env.workspace_dir.join(".cake/hooks.json"),
        &format!("touch {}", hook_marker.display()),
    );
    let tool_marker = env.workspace_dir.join("tool-ran");
    #[cfg(unix)]
    write_executable(
        &env.workspace_dir.join(".cake/tools/probe"),
        &format!(
            "#!/bin/sh\ntouch {}\necho '{{\"name\":\"probe\"}}'\n",
            tool_marker.display()
        ),
    );

    let output = run(&env, &["debug", "config", "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    let document = parse_document(&output);
    assert_eq!(document["status"], "warning");
    assert_eq!(document["data"]["project_trust"]["state"], "untrusted");
    assert_eq!(
        document["data"]["settings_sources"][1]["authority_applied"],
        false
    );
    assert_eq!(
        document["data"]["hooks"]["sources"][1]["status"],
        "untrusted"
    );
    assert_eq!(document["data"]["hooks"]["groups"], serde_json::json!([]));
    assert_eq!(
        document["data"]["toolbox"]["directories"][1]["status"],
        "untrusted"
    );
    assert_eq!(
        document["data"]["toolbox"]["directories"][1]["project_local"],
        true
    );
    assert!(
        !document["data"]["grants"]["read_write"]
            .as_array()
            .expect("read_write array")
            .iter()
            .any(|path| path == &project_grant.display().to_string())
    );
    let rendered = stdout(&output);
    assert!(!rendered.contains("touch"));
    assert!(!rendered.contains("probe-ran"));
    assert!(!hook_marker.exists(), "an untrusted hook must not run");
    assert!(!tool_marker.exists(), "a toolbox describe must not run");
    assert!(
        !project_grant.exists(),
        "the report must not create a configured writable directory"
    );

    let output = run(&env, &["debug", "config"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("project configuration is untrusted"),
        "text mode must print the project-trust warning: {}",
        stderr(&output)
    );
}

#[test]
fn debug_config_one_run_trust_applies_authority_but_still_runs_nothing() {
    let env = config_env();
    let hook_marker = env.workspace_dir.join("hook-ran");
    write_hooks(
        &env.workspace_dir.join(".cake/hooks.json"),
        &format!("touch {}", hook_marker.display()),
    );
    let tool_marker = env.workspace_dir.join("tool-ran");
    #[cfg(unix)]
    write_executable(
        &env.workspace_dir.join(".cake/tools/probe"),
        &format!(
            "#!/bin/sh\ntouch {}\necho '{{\"name\":\"probe\"}}'\n",
            tool_marker.display()
        ),
    );

    let output = run(&env, &["--trust-project", "debug", "config", "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["data"]["project_trust"]["state"], "trusted");
    assert_eq!(document["data"]["project_trust"]["mode"], "inspect-one-run");
    assert_eq!(
        document["data"]["settings_sources"][1]["authority_applied"],
        true
    );
    assert_eq!(document["data"]["hooks"]["sources"][1]["status"], "loaded");
    assert_eq!(
        document["data"]["hooks"]["groups"][0]["event"],
        "PreToolUse"
    );
    assert_eq!(document["data"]["hooks"]["groups"][0]["matcher"], "Bash");
    assert_eq!(document["data"]["hooks"]["groups"][0]["hook_count"], 1);
    assert_eq!(
        document["data"]["toolbox"]["directories"][1]["status"],
        "loadable"
    );
    assert!(!stdout(&output).contains("touch"));
    assert!(!hook_marker.exists(), "the report must never run a hook");
    assert!(
        !tool_marker.exists(),
        "the report must never describe a toolbox tool"
    );
}

#[test]
fn debug_config_reports_global_hooks_without_command_text() {
    let env = config_env();
    let marker = env.workspace_dir.join("global-hook-ran");
    write_hooks(
        &env.home_dir.join(".config/cake/hooks.json"),
        &format!("touch {}", marker.display()),
    );

    let output = run(&env, &["debug", "config", "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["data"]["hooks"]["sources"][0]["status"], "loaded");
    assert_eq!(document["data"]["hooks"]["sources"][0]["group_count"], 1);
    assert_eq!(document["data"]["hooks"]["groups"][0]["hook_count"], 1);
    assert_eq!(
        document["data"]["hooks"]["groups"][0]["fail_closed_count"],
        1
    );
    assert!(!stdout(&output).contains("touch"));
    assert!(!marker.exists(), "the report must never run a hook");
}

#[test]
#[cfg(unix)]
fn debug_config_lists_toolbox_candidates_without_describing_them() {
    let env = config_env();
    let marker = env.workspace_dir.join("toolbox-describe-ran");
    let tool = env.home_dir.join(".config/cake/tools/probe");
    write_executable(
        &tool,
        &format!(
            "#!/bin/sh\ntouch {}\necho '{{\"name\":\"probe\"}}'\n",
            marker.display()
        ),
    );

    let output = run(&env, &["debug", "config", "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    let directory = &document["data"]["toolbox"]["directories"][0];
    assert_eq!(
        directory["path"],
        tool.parent().unwrap().display().to_string()
    );
    assert_eq!(directory["status"], "loadable");
    assert_eq!(directory["candidates"][0]["filename"], "probe");
    assert_eq!(
        directory["candidates"][0]["path"],
        tool.display().to_string()
    );
    assert_eq!(document["data"]["toolbox"]["describe_probed"], false);
    assert!(!marker.exists(), "the report must never run describe");

    let output = run(
        &env,
        &["debug", "config", "--sandbox", "read-only", "--json"],
    );
    let document = parse_document(&output);
    assert_eq!(
        document["data"]["toolbox"]["directories"][0]["status"],
        "read-only"
    );
    assert!(!marker.exists(), "read-only must not run describe either");
}

#[test]
fn debug_config_reports_skills_and_selection_diagnostics() {
    let env = config_env();
    write_skill(&env, "alpha");
    write_skill(&env, "zeta");

    let output = run(
        &env,
        &["debug", "config", "--skills=alpha,missing", "--json"],
    );

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["status"], "warning");
    assert_eq!(document["data"]["skills"]["selection"], "only");
    assert_eq!(
        document["data"]["skills"]["selected"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(document["data"]["skills"]["filtered"][0]["name"], "zeta");
    assert!(
        document["checks"]
            .as_array()
            .expect("checks array")
            .iter()
            .any(|check| {
                check["id"] == "skills.discovery_failed"
                    && check["message"]
                        .as_str()
                        .is_some_and(|message| message.contains("Requested skill 'missing'"))
            })
    );
    assert_eq!(
        document["data"]["skills"]["diagnostics"][0]["level"],
        "warning"
    );
}

#[test]
fn debug_config_reports_malformed_hooks_without_running_them() {
    let env = config_env();
    let hooks = env.home_dir.join(".config/cake/hooks.json");
    fs::create_dir_all(hooks.parent().expect("hooks parent")).expect("create hooks directory");
    fs::write(&hooks, "not json").expect("write malformed hooks");

    let output = run(&env, &["debug", "config", "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["status"], "warning");
    assert_eq!(document["data"]["hooks"]["sources"][0]["status"], "invalid");
    assert!(
        document["checks"]
            .as_array()
            .expect("checks array")
            .iter()
            .any(|check| check["id"] == "config.hooks_invalid")
    );
}

#[test]
fn debug_config_reports_missing_writable_paths_without_creating_them() {
    let env = config_env();
    let missing = env.workspace_dir.join("missing/parent/cache");
    env.write_global_settings(&format!(
        "[sandbox]\nwritable = [\"{}\"]\n",
        missing.display()
    ));

    let output = run(&env, &["debug", "config", "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["status"], "warning");
    let source = document["data"]["grants"]["sources"]
        .as_array()
        .expect("sources array")
        .iter()
        .find(|source| source["id"] == "sandbox_writable")
        .expect("sandbox_writable source");
    assert_eq!(source["entries"][0]["status"], "would-create");
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
fn debug_config_settings_failure_is_an_error_document() {
    let env = config_env();
    env.write_project_settings("not valid toml = [");

    let output = run(&env, &["debug", "config", "--json"]);

    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["command"], "debug config");
    assert_eq!(document["status"], "error");
    assert_eq!(document["checks"][0]["id"], "settings.load_failed");
}

#[test]
fn debug_config_reports_an_unavailable_model_choice_without_failing() {
    let env = config_env();
    write_model_settings(&env);

    let output = run_with_key(
        &env,
        &["--model", "missing", "debug", "config", "--json"],
        "actual-secret-value",
    );

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["summary"]["model"], "missing");
    assert_eq!(document["summary"]["model_configured"], false);
    assert_eq!(document["status"], "warning");
    assert!(
        document["checks"]
            .as_array()
            .expect("checks array")
            .iter()
            .any(|check| check["id"] == "config.model_unavailable")
    );
}

#[test]
fn debug_config_reports_the_settings_system_prompt_source_without_its_body() {
    let env = config_env();
    let prompt = env.workspace_dir.join("settings-prompt.md");
    fs::write(&prompt, "SETTINGS PROMPT BODY MUST NOT BE PRINTED").expect("write prompt");
    env.write_global_settings(&format!(
        r#"default_model = "zen"
system_prompt = "{}"

[[models]]
name = "zen"
model = "glm-5.1"
base_url = "https://example.invalid/v1"
api_key_env = "CONFIG_TEST_KEY"
"#,
        prompt.display()
    ));

    let output = run_with_key(&env, &["debug", "config", "--json"], "actual-secret-value");

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["data"]["system_prompt"]["source"], "settings");
    assert_eq!(
        document["data"]["system_prompt"]["path"],
        prompt.display().to_string()
    );
    assert!(!stdout(&output).contains("SETTINGS PROMPT BODY MUST NOT BE PRINTED"));
}

#[test]
fn debug_config_no_project_config_ignores_project_authority_and_prompt() {
    let env = config_env();
    env.write_project_settings("[sandbox]\nwritable = [\"project-grant\"]\n");
    fs::write(
        env.workspace_dir.join(".cake/system.md"),
        "PROJECT PROMPT MUST BE SKIPPED",
    )
    .expect("write project prompt");
    write_hooks(
        &env.workspace_dir.join(".cake/hooks.json"),
        "touch project-hook-ran",
    );

    let output = run(&env, &["--no-project-config", "debug", "config", "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["data"]["project_trust"]["mode"], "ignore");
    assert_eq!(document["data"]["project_trust"]["state"], "not_required");
    assert_eq!(document["data"]["settings_sources"][1]["read"], false);
    assert_eq!(
        document["data"]["settings_sources"][1]["path"],
        serde_json::Value::Null
    );
    assert_eq!(document["data"]["system_prompt"]["source"], "builtin");
    assert_eq!(
        document["data"]["hooks"]["sources"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        document["data"]["toolbox"]["directories"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(!stdout(&output).contains("PROJECT PROMPT MUST BE SKIPPED"));
}

#[test]
fn debug_config_reports_settings_warnings_in_checks() {
    let env = config_env();
    env.write_global_settings("[unrecognized]\ntypo_key = true\n");

    let output = run(&env, &["debug", "config", "--json"]);

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

    let output = run(&env, &["debug", "config"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(stderr(&output).contains("warning:"));
}

#[test]
fn debug_config_help_lists_local_flags_and_examples() {
    let env = config_env();
    let output = run(&env, &["debug", "config", "--help"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let help = stdout(&output);
    for flag in [
        "--sandbox",
        "--add-dir",
        "--skills",
        "--no-skills",
        "--tools",
        "--no-tools",
        "--toolbox",
        "--json",
    ] {
        assert!(help.contains(flag), "help must list {flag}: {help}");
    }
    assert!(help.contains("cake debug config --sandbox read-only --add-dir"));

    let output = run(&env, &["--help"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("cake debug config --json"));
}
