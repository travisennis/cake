//! CLI-level output-shape tests for the versioned diagnostic JSON document.
//!
//! Each test drives the real binary against fixture settings, sessions, and a
//! disabled judge, then compares stdout to the exact document a machine
//! consumer receives. No provider call is made; the judge is either disabled
//! (`CAKE_JUDGE=off`) or never reached.
//!
//! The contract itself is documented in `docs/integrations.md`.

#![expect(clippy::expect_used, reason = "test code uses expect for assertions")]
#![expect(clippy::panic, reason = "test code uses panic for assertions")]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use support::TestEnv;

const SESSION_ID: &str = "9f2b1c3d-4e5f-4a6b-8c7d-0e1f2a3b4c5d";

fn cake_env() -> TestEnv {
    TestEnv::new("cake-diagnostic-json-test")
}

/// Write a minimal valid skill under a discovery root.
fn write_skill(root: &Path, name: &str, description: &str) -> PathBuf {
    let directory = root.join(name);
    fs::create_dir_all(&directory).expect("failed to create skill directory");
    let path = directory.join("SKILL.md");
    fs::write(
        &path,
        format!("---\nname: {name}\ndescription: {description}\n---\nBody\n"),
    )
    .expect("failed to write SKILL.md");
    path
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

/// The fixture's working directory as the binary sees it: `current_dir`
/// resolves symlinks (e.g. `/var` -> `/private/var` on macOS) and session
/// discovery compares the working directory by exact path.
fn canonical_workspace(env: &TestEnv) -> PathBuf {
    fs::canonicalize(&env.workspace_dir).unwrap_or_else(|_| {
        panic!(
            "workspace dir should canonicalize: {}",
            env.workspace_dir.display()
        )
    })
}

/// Write one session fixture with a single user message.
fn write_session(env: &TestEnv, session_id: &str, timestamp: &str, prompt: &str) {
    let working_directory = canonical_workspace(env);
    let records = [
        serde_json::json!({
            "type": "session_meta",
            "format_version": 4,
            "session_id": session_id,
            "timestamp": timestamp,
            "working_directory": working_directory,
            "model": "glm-5.1",
            "tools": ["Bash", "Read"],
        }),
        serde_json::json!({
            "type": "task_start",
            "session_id": session_id,
            "task_id": "550e8400-e29b-41d4-a716-446655440001",
            "timestamp": timestamp,
        }),
        serde_json::json!({
            "type": "message",
            "role": "user",
            "content": prompt,
            "timestamp": timestamp,
        }),
    ];
    let sessions_dir = env.data_dir.join("sessions");
    fs::create_dir_all(&sessions_dir).expect("failed to create sessions directory");
    let contents = records
        .iter()
        .map(|record| serde_json::to_string(record).expect("record should serialize"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(sessions_dir.join(format!("{session_id}.jsonl")), contents)
        .expect("failed to write session fixture");
}

// =============================================================================
// `cake debug models --json`
// =============================================================================

#[test]
fn debug_models_json_is_one_versioned_document() {
    let env = cake_env();
    env.write_global_settings(
        r#"
[[models]]
name = "zen"
model = "glm-5.1"
base_url = "https://example.com/v1"
api_key_env = "SECRET_TOKEN"
api_type = "responses"
top_k = 40
"#,
    );
    env.write_project_settings("default_model = \"zen\"\n");

    let output = env
        .command()
        .args(["debug", "models", "--json"])
        .env("SECRET_TOKEN", "actual-secret-value")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), DEBUG_MODELS_GOLDEN);
    assert_eq!(
        stderr(&output),
        "",
        "machine mode must not write diagnostics to stderr"
    );
}

/// The exact document for the single `zen` model above.
const DEBUG_MODELS_GOLDEN: &str = r#"{
  "schema_version": 1,
  "command": "debug models",
  "status": "ok",
  "summary": {
    "count": 1
  },
  "checks": [],
  "data": {
    "default_model": "zen",
    "models": [
      {
        "api_key_env": "SECRET_TOKEN",
        "api_type": "responses",
        "base_url": "https://example.com/v1",
        "context_window": null,
        "max_output_tokens": null,
        "model": "glm-5.1",
        "name": "zen",
        "provider": null,
        "provider_headers": null,
        "providers": [],
        "reasoning_effort": null,
        "reasoning_max_tokens": null,
        "reasoning_summary": null,
        "supports_images": false,
        "temperature": null,
        "top_k": 40,
        "top_p": null
      }
    ],
    "project_trust": {
      "authority": [],
      "changes": [],
      "state": "not_required"
    }
  }
}
"#;

#[test]
fn debug_models_json_orders_models_by_name() {
    let env = cake_env();
    env.write_global_settings(
        r#"
[[models]]
name = "zen"
model = "provider/zen"
base_url = "https://zen.example.com/v1"
api_key_env = "ZEN_TOKEN"

[[models]]
name = "alpha"
model = "provider/alpha"
base_url = "https://alpha.example.com/v1"
api_key_env = "ALPHA_TOKEN"
"#,
    );

    let output = run(&env, &["debug", "models", "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("stdout must be one JSON document");
    let models = document["data"]["models"]
        .as_array()
        .expect("models must be an array");
    assert_eq!(document["summary"]["count"], 2);
    assert_eq!(models[0]["name"], "alpha");
    assert_eq!(models[1]["name"], "zen");
}

#[test]
fn debug_models_json_reports_the_profile_default_model() {
    let env = cake_env();
    env.write_global_settings(
        r#"
[[models]]
name = "zen"
model = "provider/zen"
base_url = "https://zen.example.com/v1"
api_key_env = "ZEN_TOKEN"

[[models]]
name = "alpha"
model = "provider/alpha"
base_url = "https://alpha.example.com/v1"
api_key_env = "ALPHA_TOKEN"
"#,
    );
    env.write_project_settings(
        r#"
default_model = "zen"

[profiles.review]
default_model = "alpha"
"#,
    );

    let output = run(&env, &["--profile", "review", "debug", "models", "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("stdout must be one JSON document");
    assert_eq!(document["data"]["default_model"], "alpha");
    assert_eq!(
        document["data"]["models"]
            .as_array()
            .expect("models must be an array")
            .len(),
        2
    );
}

#[test]
fn debug_models_json_reports_settings_findings_in_the_document() {
    let env = cake_env();
    env.write_project_settings(
        r"
[unauthenticated]
typo_key = true
",
    );

    let output = run(&env, &["debug", "models", "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(
        stderr(&output),
        "",
        "the finding belongs in the document, not on stderr"
    );
    let document: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("stdout must be one JSON document");
    assert_eq!(document["status"], "warning");
    let checks = document["checks"]
        .as_array()
        .expect("checks must be an array");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["id"], "settings.unknown_key");
    assert_eq!(checks[0]["status"], "warning");
    assert!(
        checks[0]["message"]
            .as_str()
            .expect("message must be a string")
            .contains("unknown key 'unauthenticated'"),
        "check must name the unknown key: {}",
        checks[0]
    );
    // The defect report stays machine-readable even with nothing configured.
    assert_eq!(document["data"]["models"], serde_json::json!([]));
    assert_eq!(document["data"]["default_model"], serde_json::json!(null));
}

#[test]
fn debug_models_json_reports_a_settings_failure_as_a_document() {
    let env = cake_env();
    env.write_project_settings("not valid toml = [");

    let output = run(&env, &["debug", "models", "--json"]);

    // A settings failure is a configuration defect, so it exits 3 like every
    // other input error and still carries the failure in the document.
    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    let document: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("stdout must be one JSON document");
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["command"], "debug models");
    assert_eq!(document["status"], "error");
    assert_eq!(document["checks"][0]["id"], "settings.load_failed");
    assert!(
        document["checks"][0]["message"]
            .as_str()
            .expect("message must be a string")
            .contains("Failed to parse settings file"),
        "message must match the stderr diagnostic: {}",
        document["checks"][0]
    );
}

// =============================================================================
// `cake debug skills --json`
// =============================================================================

#[test]
fn debug_skills_json_is_one_versioned_document() {
    let env = cake_env();
    let root = canonical_workspace(&env).join(".agents/skills");
    let path = write_skill(&root, "alpha", "A project skill");

    let output = run(&env, &["debug", "skills", "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    let document: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("stdout must be one JSON document");
    assert_alpha_skill_summary(&document);
    assert_alpha_skill_rows(&document, &path);
    assert_eq!(document["data"]["project_trust"]["state"], "not_required");
}

fn assert_alpha_skill_summary(document: &serde_json::Value) {
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["command"], "debug skills");
    assert_eq!(document["status"], "ok");
    assert_eq!(document["summary"]["count"], 1);
    assert_eq!(document["summary"]["catalog_budget"], 8000);
    assert_eq!(document["summary"]["description_budget"], 400);
    assert_eq!(document["summary"]["catalog_over_budget"], false);
    assert_eq!(document["summary"]["catalog_over_budget_characters"], 0);
    assert_eq!(document["summary"]["descriptions_over_budget"], 0);
    assert_eq!(
        document["summary"]["estimated_tokens"],
        document["summary"]["rendered_characters"]
            .as_u64()
            .expect("rendered characters")
            .div_ceil(4)
    );
}

fn assert_alpha_skill_rows(document: &serde_json::Value, path: &Path) {
    let skills = document["data"]["skills"]
        .as_array()
        .expect("skills must be an array");
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0]["name"], "alpha");
    assert_eq!(skills[0]["description"], "A project skill");
    assert_eq!(skills[0]["scope"], "project");
    assert_eq!(skills[0]["description_characters"], 15);
    assert_eq!(skills[0]["description_over_budget"], false);
    assert_eq!(
        skills[0]["location"].as_str().expect("skill location"),
        path.display().to_string()
    );
    assert_eq!(
        skills[0]["base_directory"]
            .as_str()
            .expect("skill base directory"),
        path.parent().expect("skill parent").display().to_string()
    );
}

#[test]
fn debug_skills_json_reports_a_settings_failure_as_a_document() {
    let env = cake_env();
    env.write_project_settings("not valid toml = [");

    let output = run(&env, &["debug", "skills", "--json"]);

    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    let document: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("stdout must be one JSON document");
    assert_eq!(document["command"], "debug skills");
    assert_eq!(document["status"], "error");
    assert_eq!(document["checks"][0]["id"], "settings.load_failed");
}

#[test]
fn debug_skills_json_orders_rows_and_respects_skills_filter() {
    let env = cake_env();
    let root = env.workspace_dir.join(".agents/skills");
    write_skill(&root, "zeta", "A zeta skill");
    write_skill(&root, "alpha", "An alpha skill");

    let output = run(&env, &["debug", "skills", "--json"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    let skills = document["data"]["skills"].as_array().unwrap();
    assert_eq!(skills[0]["name"], "alpha");
    assert_eq!(skills[1]["name"], "zeta");

    let output = run(&env, &["debug", "skills", "--json", "--skills=alpha"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(document["summary"]["count"], 1);
    let skills = document["data"]["skills"].as_array().unwrap();
    assert_eq!(skills[0]["name"], "alpha");
}

#[test]
fn debug_skills_json_reports_diagnostics_in_checks_without_stderr() {
    let env = cake_env();
    let broken_dir = env.workspace_dir.join(".agents/skills/broken");
    fs::create_dir_all(&broken_dir).expect("failed to create broken skill dir");
    let broken = broken_dir.join("SKILL.md");
    fs::write(&broken, "missing frontmatter\n").expect("failed to write malformed skill");

    let output = run(&env, &["debug", "skills", "--json", "--skills=absent"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    let document: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(document["status"], "warning");
    assert_eq!(document["summary"]["count"], 0);
    assert_eq!(document["data"]["skills"], serde_json::json!([]));
    let checks = document["checks"].as_array().expect("checks array");
    assert_eq!(checks.len(), 2);
    assert_eq!(checks[0]["id"], "skills.discovery_failed");
    assert_eq!(checks[1]["id"], "skills.discovery_failed");
    assert!(
        checks[0]["message"]
            .as_str()
            .expect("requested-skill message")
            .contains("Requested skill 'absent'")
    );
    assert!(
        checks[1]["message"]
            .as_str()
            .expect("malformed-skill message")
            .contains("missing YAML frontmatter")
    );
    assert!(
        checks[1]["message"]
            .as_str()
            .expect("malformed-skill message")
            .contains(&broken.display().to_string())
    );
}

#[test]
fn debug_skills_json_reports_an_empty_catalog_without_findings() {
    let env = cake_env();

    let output = run(&env, &["debug", "skills", "--json", "--no-skills"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    let document: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(document["status"], "ok");
    assert_eq!(document["summary"]["count"], 0);
    assert_eq!(document["summary"]["rendered_characters"], 0);
    assert_eq!(document["summary"]["estimated_tokens"], 0);
    assert_eq!(document["checks"], serde_json::json!([]));
    assert_eq!(document["data"]["skills"], serde_json::json!([]));
}

#[test]
fn debug_skills_json_reports_settings_and_project_trust_checks() {
    let env = cake_env();
    let configured = env.workspace_dir.join("configured-skills");
    fs::create_dir_all(&configured).unwrap();
    env.write_project_settings(&format!(
        "[skills]\npath = \"{}\"\n\n[unauthenticated]\ntypo_key = true\n",
        configured.display()
    ));

    let output = run(&env, &["debug", "skills", "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    let document: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(document["status"], "warning");
    assert_eq!(document["data"]["project_trust"]["state"], "untrusted");
    let checks = document["checks"].as_array().expect("checks array");
    assert!(
        checks
            .iter()
            .any(|check| check["id"] == "settings.unknown_key")
    );
    assert!(
        checks
            .iter()
            .any(|check| check["id"] == "skills.project_trust")
    );
}

#[test]
fn debug_skills_json_marks_over_budget_catalog_and_descriptions() {
    let env = cake_env();
    let root = env.workspace_dir.join(".agents/skills");
    write_skill(&root, "verbose", "a deliberately long routing description");

    let output = run(
        &env,
        &[
            "debug",
            "skills",
            "--json",
            "--catalog-budget",
            "1",
            "--description-budget",
            "1",
        ],
    );

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(document["status"], "ok");
    assert_eq!(document["summary"]["catalog_over_budget"], true);
    assert!(
        document["summary"]["catalog_over_budget_characters"]
            .as_u64()
            .expect("over-budget characters")
            > 0
    );
    assert_eq!(document["summary"]["descriptions_over_budget"], 1);
    assert_eq!(
        document["data"]["skills"][0]["description_over_budget"],
        true
    );
}

// =============================================================================
// `cake sessions list --json`
// =============================================================================

#[test]
fn sessions_list_json_is_one_versioned_document() {
    let env = cake_env();
    write_session(&env, SESSION_ID, "2026-07-27T12:00:00Z", "list the files");

    let output = run(&env, &["sessions", "list", "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), SESSIONS_GOLDEN);
    assert_eq!(stderr(&output), "");
}

/// The exact document for the single session fixture above.
const SESSIONS_GOLDEN: &str = r#"{
  "schema_version": 1,
  "command": "sessions list",
  "status": "ok",
  "summary": {
    "count": 1
  },
  "checks": [],
  "data": {
    "sessions": [
      {
        "first_prompt": "list the files",
        "session_id": "9f2b1c3d-4e5f-4a6b-8c7d-0e1f2a3b4c5d",
        "timestamp": "2026-07-27T12:00:00Z"
      }
    ]
  }
}
"#;

#[test]
fn sessions_list_json_loads_a_session_with_a_checkpoint() {
    let env = cake_env();
    write_session(&env, SESSION_ID, "2026-07-27T12:00:00Z", "list the files");
    let path = env
        .data_dir
        .join("sessions")
        .join(format!("{SESSION_ID}.jsonl"));
    let checkpoint = serde_json::json!({
        "type": "compaction",
        "checkpoint_version": 1,
        "checkpoint_id": "checkpoint-1",
        "session_id": SESSION_ID,
        "task_id": "550e8400-e29b-41d4-a716-446655440001",
        "timestamp": "2026-07-27T12:00:01Z",
        "trigger": "manual",
        "summary": "retired work",
        "retained": [],
        "model": { "model": "glm-5.1" },
        "summary_instruction_version": 1,
        "estimated_input": {
            "before_tokens": 10,
            "after_tokens": 4,
            "basis": "content_class"
        }
    });
    let mut contents = fs::read_to_string(&path).expect("session fixture should exist");
    contents.push_str(&serde_json::to_string(&checkpoint).expect("checkpoint serializes"));
    contents.push('\n');
    fs::write(&path, contents).expect("session fixture should be writable");

    let output = run(&env, &["sessions", "list", "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), SESSIONS_GOLDEN);
    assert_eq!(stderr(&output), "");
}

#[test]
fn sessions_list_json_orders_equal_timestamps_by_session_id() {
    let env = cake_env();
    let timestamp = "2026-07-27T12:00:00Z";
    write_session(
        &env,
        "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
        timestamp,
        "second",
    );
    write_session(
        &env,
        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        timestamp,
        "first",
    );

    let output = run(&env, &["sessions", "list", "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("stdout must be one JSON document");
    let sessions = document["data"]["sessions"]
        .as_array()
        .expect("sessions must be an array");
    assert_eq!(
        sessions[0]["session_id"],
        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    );
    assert_eq!(
        sessions[1]["session_id"],
        "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
    );
}

#[test]
fn sessions_list_json_reports_an_empty_list_without_findings() {
    let env = cake_env();

    let output = run(&env, &["sessions", "list", "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("stdout must be one JSON document");
    assert_eq!(document["status"], "ok");
    assert_eq!(document["summary"]["count"], 0);
    assert_eq!(document["checks"], serde_json::json!([]));
    assert_eq!(document["data"]["sessions"], serde_json::json!([]));
}

#[test]
fn sessions_list_rejects_a_non_directory_sessions_path_at_startup() {
    let env = cake_env();
    // Store initialization rejects a non-directory sessions path before the
    // command can produce a document or reach `read_dir`.
    fs::create_dir_all(&env.data_dir).expect("failed to create data dir");
    fs::write(env.data_dir.join("sessions"), "").expect("failed to stage the sessions path");

    let output = run(&env, &["sessions", "list", "--json"]);

    assert!(!output.status.success());
    assert!(stdout(&output).is_empty(), "stdout must stay empty");
    let error = stderr(&output);
    assert!(
        error.contains("sessions directory") && error.contains("not a directory"),
        "stderr must name the invalid store path: {error}"
    );
}

#[cfg(unix)]
#[test]
fn sessions_list_json_reports_an_unreadable_directory_as_a_document() {
    use std::os::unix::fs::PermissionsExt as _;

    // Root ignores mode bits, so the denial cannot be simulated.
    // SAFETY: geteuid is always safe to call.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }

    let env = cake_env();
    // A real but unreadable sessions directory is the one listing failure the
    // command reports in its document. Store initialization leaves a narrower
    // mode alone instead of broadening it.
    fs::create_dir_all(&env.data_dir).expect("failed to create data dir");
    let sessions = env.data_dir.join("sessions");
    fs::create_dir_all(&sessions).expect("failed to create sessions dir");
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o000))
        .expect("failed to make sessions unreadable");

    let output = run(&env, &["sessions", "list", "--json"]);

    assert!(
        !output.status.success(),
        "an unreported failure must still exit nonzero, stderr: {}",
        stderr(&output)
    );
    let document: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("stdout must be one JSON document");
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["command"], "sessions list");
    assert_eq!(document["status"], "error");
    assert_eq!(document["data"], serde_json::json!({}));
    let checks = document["checks"]
        .as_array()
        .expect("checks must be an array");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["id"], "sessions.directory_unreadable");
    assert_eq!(checks[0]["status"], "error");
    let message = checks[0]["message"]
        .as_str()
        .expect("message must be a string");
    // Both channels name the same problem, so a consumer that reads only one of
    // them still learns what happened.
    assert!(
        message.contains("Failed to read sessions directory"),
        "message must name the failure: {message}"
    );
    assert!(
        stderr(&output).contains(message),
        "stderr must carry the same diagnostic: {}",
        stderr(&output)
    );

    // Restore access so the temporary root can be removed during cleanup.
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o700))
        .expect("failed to restore sessions access");
}

#[test]
fn sessions_list_json_reports_discovery_failures_as_warnings() {
    let env = cake_env();
    write_session(&env, SESSION_ID, "2026-07-27T12:00:00Z", "list the files");
    let malformed = env.data_dir.join("sessions").join("malformed.jsonl");
    fs::write(&malformed, "{not json}\n").expect("failed to write malformed session");

    let output = run(&env, &["sessions", "list", "--json"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(
        stderr(&output),
        "",
        "machine findings belong in the document, not on stderr"
    );
    let document: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("stdout must be one JSON document");
    assert_eq!(document["status"], "warning");
    assert_eq!(document["summary"]["count"], 1);
    assert_eq!(document["data"]["sessions"][0]["session_id"], SESSION_ID);
    let checks = document["checks"]
        .as_array()
        .expect("checks must be an array");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["id"], "sessions.discovery_failed");
    assert_eq!(checks[0]["status"], "warning");
    let message = checks[0]["message"]
        .as_str()
        .expect("message must be a string");
    assert!(
        message.contains(&malformed.display().to_string()),
        "the warning must name the file: {message}"
    );
    assert!(
        message.contains("failed to parse session header"),
        "the warning must name the reason: {message}"
    );
}

#[test]
fn sessions_list_text_mode_reports_discovery_failures_on_stderr() {
    let env = cake_env();
    write_session(&env, SESSION_ID, "2026-07-27T12:00:00Z", "list the files");
    let malformed = env.data_dir.join("sessions").join("malformed.jsonl");
    fs::write(&malformed, "{not json}\n").expect("failed to write malformed session");

    let output = run(&env, &["sessions", "list"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("list the files"));
    assert!(
        !stdout(&output).contains("could not inspect session file"),
        "warnings must not pollute the table: {}",
        stdout(&output)
    );
    let diagnostic = stderr(&output);
    assert!(
        diagnostic.contains("warning: could not inspect session file"),
        "stderr must carry the warning: {diagnostic}"
    );
    assert!(
        diagnostic.contains(&malformed.display().to_string()),
        "the warning must name the file: {diagnostic}"
    );
    assert!(
        diagnostic.contains("failed to parse session header"),
        "the warning must name the reason: {diagnostic}"
    );
}

// =============================================================================
// `cake bash check --json`
// =============================================================================

#[test]
fn bash_check_json_is_one_versioned_document_for_an_explicit_gate_bypass() {
    let env = cake_env();

    // A bypass short-circuits before any gate process, model, or provider work,
    // so this exercises the machine output path without a network call.
    let output = env
        .command()
        .args(["bash", "check", "--json", "--", "git status"])
        .env("CAKE_JUDGE", "off")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), BASH_CHECK_BYPASS_GOLDEN);
    assert_eq!(stderr(&output), "");
}

/// The exact document for an explicit pipeline bypass: no gate, no latency.
const BASH_CHECK_BYPASS_GOLDEN: &str = r#"{
  "schema_version": 1,
  "command": "bash check",
  "status": "ok",
  "summary": {
    "verdict": "bypassed"
  },
  "checks": [],
  "data": {
    "bypassed": true,
    "code": null,
    "confidence": null,
    "gate": null,
    "latency_ms": 0,
    "message": "gate checks were explicitly bypassed.",
    "observation": {
      "elapsed_ms": null,
      "failure_class": null,
      "outcome": "absent",
      "probability": null
    },
    "overridden": false,
    "probability": null,
    "stage": null,
    "stages": [],
    "verdict": null
  }
}
"#;

#[test]
fn bash_check_json_reports_settings_findings_in_the_document() {
    let env = cake_env();
    env.write_global_settings(
        r"
[tools.bash.judge]
not_a_real_key = 0.1
",
    );

    // A disabled judge keeps the run offline; the settings finding is what is
    // under test, and machine mode must carry it rather than write it to stderr.
    let output = env
        .command()
        .args(["bash", "check", "--json", "--", "rm -rf build"])
        .env("CAKE_JUDGE", "off")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(
        stderr(&output),
        "",
        "the finding belongs in the document, not on stderr"
    );
    let document: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("stdout must be one JSON document");
    assert_eq!(document["status"], "warning");
    assert_eq!(document["summary"]["verdict"], "bypassed");
    let checks = document["checks"]
        .as_array()
        .expect("checks must be an array");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["id"], "settings.unknown_key");
    assert_eq!(checks[0]["status"], "warning");
    assert!(
        checks[0]["message"]
            .as_str()
            .expect("message must be a string")
            .contains("unknown key 'not_a_real_key'"),
        "the finding must name the key: {}",
        checks[0]
    );
}

#[test]
fn bash_check_text_mode_keeps_settings_findings_on_stderr() {
    let env = cake_env();
    env.write_global_settings(
        r"
[tools.bash.judge]
not_a_real_key = 0.1
",
    );

    let output = env
        .command()
        .args(["bash", "check", "--", "git status"])
        .env("CAKE_JUDGE", "off")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    // The two modes report the same finding on different channels.
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("Verdict: bypassed"));
    assert!(
        stderr(&output).contains("unknown key 'not_a_real_key'"),
        "stderr must carry the finding: {}",
        stderr(&output)
    );
}

#[test]
fn bash_check_json_keeps_settings_findings_on_stderr_when_no_document_is_written() {
    let env = cake_env();
    env.write_project_settings("[unauthenticated]\ntypo_key = true\n");

    // An unconfigured judge model fails before any provider call and writes no
    // document, so the finding has to fall back to stderr instead of vanishing.
    // `CAKE_JUDGE=on` keeps an ambient bypass out of the judge-enabled path.
    let output = env
        .command()
        .args([
            "--model",
            "not-a-configured-model",
            "bash",
            "check",
            "--json",
            "--",
            "git status",
        ])
        .env("CAKE_JUDGE", "on")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "",
        "a failure that writes no document leaves machine stdout empty"
    );
    assert!(
        stderr(&output).contains("unknown key 'unauthenticated'"),
        "the finding must still reach stderr: {}",
        stderr(&output)
    );
}

#[test]
fn bash_check_json_reports_an_unloadable_settings_file_without_a_document() {
    let env = cake_env();
    env.write_project_settings("not valid toml = [");

    // The settings load precedes the judge, so this path cannot render a
    // document; it keeps the settings failure class on stderr with empty stdout.
    let output = run(&env, &["bash", "check", "--json", "--", "git status"]);

    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "",
        "a failure that writes no document leaves machine stdout empty"
    );
    assert!(
        stderr(&output).contains("Failed to parse settings file"),
        "stderr must name the settings failure: {}",
        stderr(&output)
    );
}

#[test]
fn bash_check_json_and_diagnostic_are_mutually_exclusive() {
    let env = cake_env();

    let output = run(
        &env,
        &[
            "bash",
            "check",
            "--json",
            "--diagnostic",
            "--",
            "git status",
        ],
    );

    assert_eq!(
        output.status.code(),
        Some(3),
        "asking for both machine modes is an input error"
    );
    assert!(
        stderr(&output).contains("cannot be used with"),
        "stderr: {}",
        stderr(&output)
    );
    assert_eq!(stdout(&output), "");
}

// =============================================================================
// Help Examples
// =============================================================================

#[test]
fn help_documents_examples_for_the_touched_commands() {
    let env = cake_env();
    for args in [
        &["--help"][..],
        &["debug", "--help"][..],
        &["debug", "models", "--help"][..],
        &["sessions", "--help"][..],
        &["sessions", "list", "--help"][..],
        &["bash", "--help"][..],
        &["bash", "check", "--help"][..],
    ] {
        let output = run(&env, args);
        assert!(
            output.status.success(),
            "{args:?} must succeed: {}",
            stderr(&output)
        );
        let help = stdout(&output);
        assert!(
            help.contains("Examples:"),
            "{args:?} must show an Examples section:\n{help}"
        );
    }
}

#[test]
fn help_examples_are_copy_pasteable_machine_mode_invocations() {
    let env = cake_env();
    for (args, expected) in [
        (&["--help"][..], "cake sessions list --json"),
        (
            &["debug", "models", "--help"][..],
            "cake debug models --json",
        ),
        (
            &["sessions", "list", "--help"][..],
            "cake sessions list --json",
        ),
        (&["bash", "check", "--help"][..], "cake bash check --json"),
    ] {
        let output = run(&env, args);
        let help = stdout(&output);
        assert!(
            help.contains(expected),
            "{args:?} must show `{expected}`:\n{help}"
        );
    }
}
