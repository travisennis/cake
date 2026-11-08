//! End-to-end coverage for `cake mcp` server-definition management.
//!
//! These tests drive the real binary without a provider connection: `add`,
//! `remove`, `list`, and `get` only read and write configuration.

#![expect(clippy::expect_used, reason = "test code uses expect for assertions")]
#![expect(
    dead_code,
    reason = "shared support helpers are used by some test binaries only"
)]
#![cfg(unix)]

mod support;
#[path = "support/trust.rs"]
mod trust_helpers;

use std::fs;
use std::path::PathBuf;
use std::process::Output;

use support::TestEnv;

fn run(env: &TestEnv, args: &[&str]) -> Output {
    env.command()
        .args(args)
        .output()
        .expect("failed to execute cake")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn global_config_path(env: &TestEnv) -> PathBuf {
    env.home_dir.join(".config").join("cake").join("mcp.json")
}

fn read_config(path: &PathBuf) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(path).expect("config file should be readable"))
        .expect("config file should be valid JSON")
}

fn document(output: &Output) -> serde_json::Value {
    serde_json::from_str(&stdout(output)).expect("stdout should carry one JSON document")
}

#[test]
fn add_writes_a_global_entry() {
    let env = TestEnv::new("cake-mcp-manage-add");

    let added = add_fixture(&env);

    assert!(
        stdout(&added).contains("Added global MCP server 'fixture'"),
        "{}",
        stdout(&added)
    );
    let config = read_config(&global_config_path(&env));
    assert_eq!(config["mcpServers"]["fixture"]["command"], "python3");
    assert_eq!(config["mcpServers"]["fixture"]["args"][0], "server.py");
    assert_eq!(
        config["mcpServers"]["fixture"]["env"]["FAKE_MCP_ERA"],
        "modern"
    );
    assert_eq!(config["mcpServers"]["fixture"]["timeout_secs"], 30);
}

#[test]
fn list_reports_the_entry_as_json_and_text() {
    let env = TestEnv::new("cake-mcp-manage-list");
    _ = add_fixture(&env);

    let listed = run(&env, &["mcp", "list", "--json"]);
    assert!(listed.status.success(), "{}", stderr(&listed));
    let listed_document = document(&listed);
    assert_eq!(listed_document["command"], "mcp list");
    assert_eq!(listed_document["status"], "ok");
    assert_eq!(listed_document["summary"]["count"], 1);
    assert_listed_entry(&listed_document["data"]["servers"][0]);
    assert!(
        !stdout(&listed).contains("modern"),
        "env values must never be printed: {}",
        stdout(&listed)
    );

    let text = run(&env, &["mcp", "list"]);
    assert!(text.status.success(), "{}", stderr(&text));
    let text = stdout(&text);
    assert!(text.contains("fixture"), "{text}");
    assert!(text.contains("command: python3 server.py --flag"), "{text}");
    assert!(text.contains("env: FAKE_MCP_ERA"), "{text}");
    assert!(text.contains("timeout: 30s"), "{text}");
    assert!(!text.contains("modern"), "{text}");
}

#[test]
fn get_reports_one_entry() {
    let env = TestEnv::new("cake-mcp-manage-get");
    _ = add_fixture(&env);

    let got = run(&env, &["mcp", "get", "fixture", "--json"]);

    assert!(got.status.success(), "{}", stderr(&got));
    let got_document = document(&got);
    assert_eq!(got_document["command"], "mcp get");
    assert_eq!(got_document["status"], "ok");
    assert_eq!(got_document["data"]["server"]["name"], "fixture");
    assert_eq!(got_document["data"]["server"]["command"], "python3");
    assert!(!stdout(&got).contains("modern"));
}

#[test]
fn remove_deletes_the_entry_and_reports_an_absent_name() {
    let env = TestEnv::new("cake-mcp-manage-remove");
    _ = add_fixture(&env);

    let removed = run(&env, &["mcp", "remove", "fixture"]);
    assert!(removed.status.success(), "{}", stderr(&removed));
    assert!(stdout(&removed).contains("Removed global MCP server 'fixture'"));
    let config = read_config(&global_config_path(&env));
    assert!(config["mcpServers"].get("fixture").is_none());

    let again = run(&env, &["mcp", "remove", "fixture"]);
    assert!(again.status.success(), "{}", stderr(&again));
    assert!(
        stdout(&again).contains("No global MCP server named 'fixture' found"),
        "{}",
        stdout(&again)
    );
}

/// Add the shared fixture entry and assert the command succeeded.
fn add_fixture(env: &TestEnv) -> Output {
    let output = run(
        env,
        &[
            "mcp",
            "add",
            "fixture",
            "--env",
            "FAKE_MCP_ERA=modern",
            "--timeout-secs",
            "30",
            "--",
            "python3",
            "server.py",
            "--flag",
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    output
}

/// Assert the fields every resolved listing reports for the fixture entry.
fn assert_listed_entry(entry: &serde_json::Value) {
    assert_eq!(entry["name"], "fixture");
    assert_eq!(entry["command"], "python3");
    assert_eq!(entry["args"][0], "server.py");
    assert_eq!(entry["env"][0], "FAKE_MCP_ERA");
    assert_eq!(entry["timeout_secs"], 30);
    assert!(
        entry["source"]
            .as_str()
            .expect("source should be a string")
            .ends_with("mcp.json")
    );
}

#[test]
fn a_malformed_global_file_is_not_rewritten() {
    let env = TestEnv::new("cake-mcp-manage-malformed");
    let path = global_config_path(&env);
    fs::create_dir_all(path.parent().expect("config path has a parent"))
        .expect("failed to create config directory");
    let original = "{ not json";
    fs::write(&path, original).expect("failed to write malformed config");

    let output = run(&env, &["mcp", "add", "fixture", "--", "python3"]);

    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("invalid"),
        "the diagnostic should name the malformed file: {}",
        stderr(&output)
    );
    assert_eq!(
        fs::read_to_string(&path).expect("config should still exist"),
        original
    );
}

#[test]
fn a_remote_url_is_rejected_without_writing() {
    let env = TestEnv::new("cake-mcp-manage-url");

    let output = run(
        &env,
        &["mcp", "add", "remote", "--url", "https://example.com/mcp"],
    );

    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("stdio servers only"),
        "{}",
        stderr(&output)
    );
    assert!(!global_config_path(&env).exists());
}

#[test]
fn an_invalid_env_order_is_an_input_error() {
    let env = TestEnv::new("cake-mcp-manage-env");

    let output = run(
        &env,
        &[
            "mcp",
            "add",
            "fixture",
            "--env",
            "NO_EQUALS",
            "--",
            "python3",
        ],
    );

    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(stderr(&output).contains("KEY=VALUE"), "{}", stderr(&output));
}

#[test]
fn a_missing_get_name_reports_a_not_found_document() {
    let env = TestEnv::new("cake-mcp-manage-get-missing");

    let output = run(&env, &["mcp", "get", "missing", "--json"]);

    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let document = document(&output);
    assert_eq!(document["command"], "mcp get");
    assert_eq!(document["status"], "error");
    assert_eq!(document["checks"][0]["id"], "mcp.server_not_found");
    assert!(
        document["checks"][0]["message"]
            .as_str()
            .expect("message should be a string")
            .contains("missing")
    );
}

#[test]
fn project_servers_require_trust_to_appear() {
    let env = TestEnv::new("cake-mcp-manage-project");
    let project_config = env.workspace_dir.join(".cake").join("mcp.json");
    fs::create_dir_all(project_config.parent().expect("config path has a parent"))
        .expect("failed to create .cake");
    fs::write(
        &project_config,
        r#"{"mcpServers":{"project-server":{"command":"echo"}}}"#,
    )
    .expect("failed to write project mcp.json");

    let untrusted = run(&env, &["mcp", "list", "--json"]);
    assert!(untrusted.status.success(), "{}", stderr(&untrusted));
    let untrusted_document = document(&untrusted);
    assert_eq!(untrusted_document["status"], "warning");
    assert!(
        untrusted_document["checks"]
            .as_array()
            .expect("checks should be an array")
            .iter()
            .any(|check| {
                check["id"] == "mcp.config_invalid"
                    && check["message"]
                        .as_str()
                        .is_some_and(|message| message.contains("not trusted"))
            }),
        "untrusted project config should be reported: {untrusted_document}"
    );
    assert_eq!(untrusted_document["summary"]["count"], 0);

    env.trust_project();

    let trusted = run(&env, &["mcp", "list", "--json"]);
    assert!(trusted.status.success(), "{}", stderr(&trusted));
    let trusted_document = document(&trusted);
    assert_eq!(trusted_document["status"], "ok");
    assert_eq!(trusted_document["summary"]["count"], 1);
    assert_eq!(
        trusted_document["data"]["servers"][0]["name"],
        "project-server"
    );
    assert!(
        trusted_document["data"]["servers"][0]["source"]
            .as_str()
            .expect("source should be a string")
            .ends_with(".cake/mcp.json")
    );

    let got = run(&env, &["mcp", "get", "project-server", "--json"]);
    assert!(got.status.success(), "{}", stderr(&got));
    assert_eq!(document(&got)["data"]["server"]["command"], "echo");
}
