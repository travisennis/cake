//! Integration tests for `cake debug tools`.

#![expect(clippy::expect_used, reason = "test code uses expect for assertions")]

mod support;

use std::fs;
use std::path::Path;
use std::process::Stdio;

use support::TestEnv;

fn tools_env() -> TestEnv {
    TestEnv::new("cake-debug-tools-test")
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

fn write_settings(env: &TestEnv, supports_images: bool) {
    env.write_global_settings(&format!(
        r#"default_model = "fixture"

[[models]]
name = "fixture"
model = "fixture-model"
base_url = "https://example.invalid/v1"
api_key_env = "DEBUG_TOOLS_KEY"
supports_images = {supports_images}
"#
    ));
}

fn write_mcp_config(env: &TestEnv, exposure: &str, command: &str, args: &[&str], env_value: &str) {
    let config_dir = env.home_dir.join(".config/cake");
    fs::create_dir_all(&config_dir).expect("create global config directory");
    let config = serde_json::json!({
        "mcpServers": {
            "fixture": {
                "command": command,
                "args": args,
                "env": { "FIXTURE_TOKEN": env_value },
                "exposure": exposure,
            }
        }
    });
    fs::write(
        config_dir.join("mcp.json"),
        serde_json::to_vec_pretty(&config).expect("serialize MCP config"),
    )
    .expect("write MCP config");
}

#[cfg(unix)]
fn write_toolbox_executable(env: &TestEnv, marker: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = env.home_dir.join(".config/cake/tools/probe");
    fs::create_dir_all(path.parent().expect("toolbox parent")).expect("create toolbox directory");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\ntouch {}\necho '{{\"name\":\"probe\",\"description\":\"Fixture probe\"}}'\n",
            marker.display()
        ),
    )
    .expect("write toolbox executable");
    let mut permissions = fs::metadata(&path).expect("toolbox metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("make toolbox executable");
    path
}

fn builtin<'a>(document: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    document["data"]["builtins"]
        .as_array()
        .expect("builtins array")
        .iter()
        .find(|entry| entry["name"] == name)
        .expect("built-in row must exist")
}

#[test]
fn debug_tools_text_reports_builtins_and_filters() {
    let env = tools_env();
    write_settings(&env, false);

    let output = run(&env, &["debug", "tools", "--sandbox", "workspace-write"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let report = stdout(&output);
    assert!(report.contains("Effective tools"));
    assert!(report.contains("Model: fixture (from default_model)"));
    assert!(report.contains("Model image support: no"));
    assert!(report.contains("Policy: workspace-write (from --sandbox)"));
    assert!(report.contains("Built-in tools"));
    assert!(report.contains("ReadImage"));
    assert!(report.contains("model has no image support"));
    assert!(report.contains("Gate readiness is not probed"));
    assert_eq!(stderr(&output), "");
    assert_eq!(
        fs::read_dir(env.data_dir.join("sessions")).unwrap().count(),
        0,
        "a diagnostic command must not create a session"
    );
}

#[test]
#[expect(
    clippy::cognitive_complexity,
    reason = "the test asserts the full machine-readable report shape"
)]
fn debug_tools_json_is_one_deterministic_document() {
    let env = tools_env();
    write_settings(&env, false);

    let first = run(
        &env,
        &["debug", "tools", "--sandbox", "workspace-write", "--json"],
    );
    let second = run(
        &env,
        &["debug", "tools", "--sandbox", "workspace-write", "--json"],
    );

    assert_eq!(first.status.code(), Some(0), "stderr: {}", stderr(&first));
    assert_eq!(stderr(&first), "");
    assert_eq!(
        stdout(&first),
        stdout(&second),
        "output must be byte-stable"
    );

    let document = parse_document(&first);
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["command"], "debug tools");
    assert_eq!(document["status"], "ok");
    assert_eq!(document["summary"]["builtin_tool_count"], 6);
    assert_eq!(document["summary"]["enabled_builtin_count"], 5);
    assert_eq!(document["summary"]["filtered_builtin_count"], 1);
    assert_eq!(document["summary"]["policy"], "workspace-write");
    assert_eq!(document["summary"]["policy_source"], "--sandbox");
    assert_eq!(document["data"]["model"]["name"], "fixture");
    assert_eq!(document["data"]["model"]["supports_images"], false);
    assert_eq!(
        document["data"]["builtins"]
            .as_array()
            .expect("builtins array")
            .len(),
        6
    );
    assert_eq!(builtin(&document, "ReadImage")["status"], "filtered");
    assert_eq!(
        builtin(&document, "ReadImage")["reason"],
        "model has no image support"
    );
    assert_eq!(builtin(&document, "Read")["status"], "enabled");
    assert_eq!(document["data"]["gate_readiness"]["probed"], false);
    assert!(
        document["data"]["gate_readiness"]["note"]
            .as_str()
            .expect("gate note")
            .contains("can additionally remove Bash and BashSession")
    );
    assert_eq!(document["data"]["mcp"]["connected"], false);
    assert_eq!(document["data"]["toolbox"]["describe_probed"], false);
}

#[test]
#[expect(
    clippy::cognitive_complexity,
    reason = "the test asserts both selection modes and the external sources they retain"
)]
fn debug_tools_applies_tools_and_no_tools_selection() {
    let env = tools_env();
    write_settings(&env, true);
    write_mcp_config(
        &env,
        "deferred",
        "/usr/bin/fixture-mcp",
        &[],
        "secret-value",
    );
    #[cfg(unix)]
    write_toolbox_executable(&env, &env.workspace_dir.join("toolbox-describe-ran"));

    let output = run(
        &env,
        &[
            "debug",
            "tools",
            "--sandbox",
            "workspace-write",
            "--tools",
            "Read,Missing",
            "--json",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["status"], "warning");
    assert_eq!(document["summary"]["enabled_builtin_count"], 1);
    assert_eq!(builtin(&document, "Read")["status"], "enabled");
    for name in ["Bash", "BashSession", "ReadImage", "Edit", "Write"] {
        assert_eq!(builtin(&document, name)["status"], "filtered");
        assert_eq!(builtin(&document, name)["reason"], "not in --tools");
    }
    assert_eq!(
        document["data"]["selection"]["selected"],
        serde_json::json!(["Read", "Missing"])
    );
    assert_eq!(
        document["data"]["selection"]["unknown"],
        serde_json::json!(["Missing"])
    );
    assert_eq!(document["checks"][0]["id"], "tools.unknown_selection");
    assert!(
        document["summary"]["mcp_server_count"]
            .as_u64()
            .unwrap_or_default()
            > 0,
        "external MCP sources stay visible under an exact-name selection"
    );

    let output = run(
        &env,
        &[
            "debug",
            "tools",
            "--sandbox",
            "workspace-write",
            "--no-tools",
            "--json",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["summary"]["enabled_builtin_count"], 0);
    assert_eq!(document["data"]["selection"]["source"], "--no-tools");
    for entry in document["data"]["builtins"]
        .as_array()
        .expect("builtins array")
    {
        assert_eq!(entry["status"], "filtered");
        assert_eq!(entry["reason"], "--no-tools");
    }
    assert_eq!(document["summary"]["mcp_server_count"], 1);
    assert_eq!(document["data"]["tool_search"]["state"], "absent");
    assert_eq!(
        document["data"]["tool_search"]["fallback"],
        serde_json::Value::Null
    );
    assert!(
        document["summary"]["toolbox_candidate_count"]
            .as_u64()
            .unwrap_or_default()
            > 0,
        "toolbox sources stay visible without tools"
    );
}

#[test]
#[cfg(unix)]
#[expect(
    clippy::cognitive_complexity,
    reason = "the test asserts the read-only projection across built-ins and both external sources"
)]
fn debug_tools_read_only_marks_external_sources_unavailable_without_running_them() {
    let env = tools_env();
    write_settings(&env, true);
    let describe_marker = env.workspace_dir.join("toolbox-describe-ran");
    write_toolbox_executable(&env, &describe_marker);
    let connect_marker = env.workspace_dir.join("mcp-connect-ran");
    let touch_command = format!("touch {}", connect_marker.display());
    write_mcp_config(
        &env,
        "deferred",
        "/bin/sh",
        &["-c", touch_command.as_str()],
        "secret-value",
    );

    let output = run(
        &env,
        &["debug", "tools", "--sandbox", "read-only", "--json"],
    );

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    for name in ["Bash", "BashSession", "Edit", "Write"] {
        assert_eq!(builtin(&document, name)["status"], "filtered");
        assert_eq!(builtin(&document, name)["reason"], "read-only policy");
    }
    assert_eq!(builtin(&document, "Read")["status"], "enabled");
    assert_eq!(builtin(&document, "ReadImage")["status"], "enabled");
    assert_eq!(
        document["data"]["sandbox"]["external_tools_available"],
        false
    );
    let directory = &document["data"]["toolbox"]["directories"][0];
    assert_eq!(directory["status"], "read-only");
    assert_eq!(directory["candidates"][0]["status"], "unprobed");
    assert_eq!(directory["candidates"][0]["available"], false);
    let server = &document["data"]["mcp"]["servers"][0];
    assert_eq!(server["status"], "unprobed");
    assert_eq!(server["available"], false);
    assert_eq!(server["reason"], "read-only policy");
    assert_eq!(document["data"]["tool_search"]["state"], "absent");
    assert!(
        document["data"]["tool_search"]["reason"]
            .as_str()
            .expect("tool search reason")
            .contains("read-only policy")
    );
    assert!(!describe_marker.exists(), "read-only must not run describe");
    assert!(!connect_marker.exists(), "read-only must not connect MCP");
}

#[test]
fn debug_tools_reports_deferred_mcp_and_tool_search_fallback() {
    let env = tools_env();
    write_settings(&env, true);

    let output = run(
        &env,
        &["debug", "tools", "--sandbox", "workspace-write", "--json"],
    );
    let document = parse_document(&output);
    assert_eq!(document["data"]["tool_search"]["state"], "absent");
    assert_eq!(document["data"]["tool_search"]["deferred_server_count"], 0);
    assert!(
        document["data"]["tool_search"]["reason"]
            .as_str()
            .expect("tool search reason")
            .contains("no deferred MCP servers")
    );

    write_mcp_config(
        &env,
        "deferred",
        "/usr/bin/fixture-mcp",
        &[],
        "secret-value",
    );
    let output = run(
        &env,
        &["debug", "tools", "--sandbox", "workspace-write", "--json"],
    );
    let document = parse_document(&output);
    assert_eq!(document["data"]["tool_search"]["state"], "conditional");
    assert_eq!(document["data"]["tool_search"]["deferred_server_count"], 1);
    assert_eq!(
        document["data"]["tool_search"]["fallback"],
        serde_json::Value::Null
    );

    let output = run(
        &env,
        &[
            "debug",
            "tools",
            "--sandbox",
            "workspace-write",
            "--tools",
            "Read",
            "--json",
        ],
    );
    let document = parse_document(&output);
    assert_eq!(document["data"]["tool_search"]["state"], "absent");
    assert_eq!(document["data"]["tool_search"]["fallback"], "direct");
    assert!(
        document["data"]["tool_search"]["reason"]
            .as_str()
            .expect("tool search reason")
            .contains("does not include ToolSearch")
    );

    write_mcp_config(&env, "direct", "/usr/bin/fixture-mcp", &[], "secret-value");
    let output = run(
        &env,
        &["debug", "tools", "--sandbox", "workspace-write", "--json"],
    );
    let document = parse_document(&output);
    assert_eq!(document["data"]["tool_search"]["state"], "absent");
    assert_eq!(document["data"]["tool_search"]["deferred_server_count"], 0);
}

#[test]
#[cfg(unix)]
#[expect(
    clippy::cognitive_complexity,
    reason = "the test asserts both unprobed external source shapes and secret omission"
)]
fn debug_tools_lists_toolbox_and_mcp_sources_without_probing() {
    let env = tools_env();
    write_settings(&env, true);
    let describe_marker = env.workspace_dir.join("toolbox-describe-ran");
    let tool = write_toolbox_executable(&env, &describe_marker);
    write_mcp_config(
        &env,
        "deferred",
        "/usr/bin/fixture-mcp",
        &["--fixture"],
        "secret-value",
    );

    let output = run(
        &env,
        &["debug", "tools", "--sandbox", "workspace-write", "--json"],
    );

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stderr(&output), "");
    let rendered = stdout(&output);
    assert!(!rendered.contains("secret-value"));
    assert!(
        !describe_marker.exists(),
        "the command must not run describe"
    );

    let document = parse_document(&output);
    let directory = &document["data"]["toolbox"]["directories"][0];
    assert_eq!(
        directory["path"],
        tool.parent().unwrap().display().to_string()
    );
    assert_eq!(directory["source"], "global");
    assert_eq!(directory["status"], "loadable");
    assert_eq!(directory["candidates"][0]["filename"], "probe");
    assert_eq!(
        directory["candidates"][0]["path"],
        tool.display().to_string()
    );
    assert_eq!(directory["candidates"][0]["status"], "unprobed");
    assert_eq!(directory["candidates"][0]["available"], true);

    let server = &document["data"]["mcp"]["servers"][0];
    assert_eq!(server["name"], "fixture");
    assert_eq!(server["command"], "/usr/bin/fixture-mcp");
    assert_eq!(server["args"], serde_json::json!(["--fixture"]));
    assert_eq!(server["exposure"], "deferred");
    assert_eq!(server["status"], "unprobed");
    assert_eq!(
        server["source"],
        env.home_dir
            .join(".config/cake/mcp.json")
            .display()
            .to_string()
    );
}

#[test]
fn debug_tools_reports_unknown_selection_as_a_warning() {
    let env = tools_env();
    write_settings(&env, true);

    let output = run(
        &env,
        &[
            "debug",
            "tools",
            "--sandbox",
            "workspace-write",
            "--tools",
            "Missing",
            "--json",
        ],
    );

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["status"], "warning");
    assert_eq!(document["data"]["selection"]["unknown"][0], "Missing");
    assert_eq!(document["checks"][0]["id"], "tools.unknown_selection");
    assert_eq!(document["summary"]["enabled_builtin_count"], 0);
    assert!(
        document["data"]["builtins"]
            .as_array()
            .expect("builtins array")
            .iter()
            .all(|entry| entry["name"] != "Missing")
    );
}

#[test]
fn debug_tools_prints_the_project_trust_warning_in_text_mode() {
    let env = tools_env();
    write_settings(&env, true);
    env.write_project_settings("directories = [\"/tmp/cake-debug-tools-untrusted\"]\n");

    let output = run(&env, &["debug", "tools", "--sandbox", "workspace-write"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("project configuration is untrusted"),
        "text mode must print the project-trust warning: {}",
        stderr(&output)
    );

    let output = run(
        &env,
        &["debug", "tools", "--sandbox", "workspace-write", "--json"],
    );
    let document = parse_document(&output);
    assert_eq!(document["status"], "warning");
    assert!(
        document["checks"]
            .as_array()
            .expect("checks array")
            .iter()
            .any(|check| check["id"] == "config.project_trust")
    );
}

#[test]
fn debug_tools_settings_failure_is_an_error_document() {
    let env = tools_env();
    env.write_global_settings("default_model = [\n");

    let output = run(&env, &["debug", "tools", "--json"]);

    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    let document = parse_document(&output);
    assert_eq!(document["command"], "debug tools");
    assert_eq!(document["status"], "error");
    assert_eq!(document["checks"][0]["id"], "settings.load_failed");
}

#[test]
fn debug_tools_help_lists_flags_and_examples() {
    let env = tools_env();

    let output = run(&env, &["debug", "tools", "--help"]);

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let help = stdout(&output);
    assert!(help.contains("Examples:"));
    assert!(help.contains("cake debug tools --tools Read"));
    assert!(help.contains("cake debug tools --no-tools"));
    assert!(help.contains("cake debug tools --sandbox read-only"));
    assert!(help.contains("--json"));
}
