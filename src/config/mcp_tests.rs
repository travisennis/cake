use std::collections::BTreeMap;
use std::path::Path;

use super::*;
use crate::config::trust::{ProjectConfigMode, ProjectTrustReport, TrustState};

fn trust(state: TrustState) -> ProjectTrustReport {
    ProjectTrustReport {
        state,
        mode: ProjectConfigMode::Enforce,
        project_root: std::path::PathBuf::from("/project"),
        project_key: "key".to_string(),
        record_path: std::path::PathBuf::from("/record.json"),
        authority: Vec::new(),
        changes: Vec::new(),
        invalid_reason: None,
    }
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, text).unwrap();
}

fn discover(
    config_root: &Path,
    project: &Path,
    explicit: &[std::path::PathBuf],
    trust: &ProjectTrustReport,
    mode: ProjectConfigMode,
) -> McpConfigLoad {
    let project_bytes = std::fs::read(project.join(PROJECT_MCP_CONFIG)).ok();
    discover_mcp_servers(
        explicit,
        project,
        config_root,
        project,
        trust,
        mode,
        project_bytes.as_deref(),
    )
    .unwrap()
}

#[test]
fn parses_standard_mcp_servers_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let config_root = tmp.path().join("config");
    let project = tmp.path().join("project");
    let global = config_root.join("cake").join(MCP_CONFIG_FILE);
    write(
        &global,
        r#"{
          "mcpServers": {
            "safari-mcp": {
              "command": "/usr/bin/safaridriver",
              "args": ["--mcp"],
              "env": {"SAFARI_TOKEN": "secret"}
            }
          }
        }"#,
    );

    let load = discover(
        &config_root,
        &project,
        &[],
        &trust(TrustState::NotRequired),
        ProjectConfigMode::Enforce,
    );

    assert!(load.warnings.is_empty(), "{:?}", load.warnings);
    assert_eq!(load.servers.len(), 1);
    let server = &load.servers[0];
    assert_eq!(server.name, "safari-mcp");
    assert_eq!(server.command, "/usr/bin/safaridriver");
    assert_eq!(server.args, ["--mcp"]);
    assert_eq!(server.env.get("SAFARI_TOKEN").unwrap(), "secret");
    assert_eq!(server.timeout_secs, DEFAULT_MCP_TIMEOUT_SECS);
    assert_eq!(server.description, None);
    assert_eq!(server.exposure, McpExposure::Deferred);
    assert_eq!(server.source, global);
}

#[test]
fn parses_description_and_exposure() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("mcp.json");
    write(
        &path,
        r#"{
          "mcpServers": {
            "safari": {
              "command": "safari",
              "description": "  Browser automation.  ",
              "exposure": "hidden"
            },
            "defaulted": {"command": "cmd"}
          }
        }"#,
    );

    let load = discover(
        tmp.path(),
        tmp.path(),
        &[path],
        &trust(TrustState::NotRequired),
        ProjectConfigMode::Enforce,
    );

    assert!(load.warnings.is_empty(), "{:?}", load.warnings);
    assert_eq!(load.servers.len(), 2);
    let safari = load
        .servers
        .iter()
        .find(|server| server.name == "safari")
        .unwrap();
    assert_eq!(safari.description.as_deref(), Some("Browser automation."));
    assert_eq!(safari.exposure, McpExposure::Hidden);
    let defaulted = load
        .servers
        .iter()
        .find(|server| server.name == "defaulted")
        .unwrap();
    assert_eq!(defaulted.description, None);
    assert_eq!(defaulted.exposure, McpExposure::Deferred);
}

#[test]
fn later_sources_override_earlier_servers_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let config_root = tmp.path().join("config");
    let project = tmp.path().join("project");
    write(
        &config_root.join("cake").join(MCP_CONFIG_FILE),
        r#"{"mcpServers":{"alpha":{"command":"global"},"beta":{"command":"global"}}}"#,
    );
    write(
        &project.join(PROJECT_MCP_CONFIG),
        r#"{"mcpServers":{"alpha":{"command":"project"}}}"#,
    );
    let explicit = tmp.path().join("extra.json");
    write(
        &explicit,
        r#"{"mcpServers":{"beta":{"command":"explicit"}}}"#,
    );

    let load = discover(
        &config_root,
        &project,
        &[explicit],
        &trust(TrustState::Trusted),
        ProjectConfigMode::Enforce,
    );

    assert_eq!(load.servers.len(), 2);
    assert_eq!(load.servers[0].name, "alpha");
    assert_eq!(load.servers[0].command, "project");
    assert_eq!(load.servers[1].name, "beta");
    assert_eq!(load.servers[1].command, "explicit");
}

#[test]
fn project_file_is_ignored_when_untrusted() {
    let tmp = tempfile::tempdir().unwrap();
    let config_root = tmp.path().join("config");
    let project = tmp.path().join("project");
    write(
        &project.join(PROJECT_MCP_CONFIG),
        r#"{"mcpServers":{"alpha":{"command":"project"}}}"#,
    );

    let load = discover(
        &config_root,
        &project,
        &[],
        &trust(TrustState::Untrusted),
        ProjectConfigMode::Inspect,
    );

    assert!(load.servers.is_empty());
    assert_eq!(load.warnings.len(), 1);
    assert!(load.warnings[0].contains("not trusted"));
}

#[test]
fn project_file_is_ignored_when_project_config_is_off() {
    let tmp = tempfile::tempdir().unwrap();
    let config_root = tmp.path().join("config");
    let project = tmp.path().join("project");
    write(
        &project.join(PROJECT_MCP_CONFIG),
        r#"{"mcpServers":{"alpha":{"command":"project"}}}"#,
    );

    let load = discover(
        &config_root,
        &project,
        &[],
        &trust(TrustState::Trusted),
        ProjectConfigMode::Ignore,
    );

    assert!(load.servers.is_empty());
    assert!(load.warnings.is_empty());
}

#[test]
fn a_non_object_top_level_document_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("array.json");
    write(&path, "[1, 2]");
    let error = discover_mcp_servers(
        std::slice::from_ref(&path),
        tmp.path(),
        tmp.path(),
        tmp.path(),
        &trust(TrustState::NotRequired),
        ProjectConfigMode::Enforce,
        None,
    )
    .unwrap_err();
    assert!(matches!(error, McpConfigError::Invalid { .. }));
    assert!(error.to_string().contains("mcpServers"));
}

#[test]
fn project_mcp_paths_inside_cake_are_filtered() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    let inside = project.join(".cake").join("mcp.json");
    let outside = tmp.path().join("outside.json");
    write(&inside, r#"{"mcpServers":{}}"#);
    write(&outside, r#"{"mcpServers":{}}"#);

    let filtered = filter_project_mcp_paths(vec![inside, outside.clone()], &[project.as_path()]);
    assert_eq!(filtered, vec![outside]);
}

#[test]
fn a_malformed_global_file_warns_and_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let config_root = tmp.path().join("config");
    let project = tmp.path().join("project");
    write(&config_root.join("cake").join(MCP_CONFIG_FILE), "{not json");

    let load = discover(
        &config_root,
        &project,
        &[],
        &trust(TrustState::NotRequired),
        ProjectConfigMode::Enforce,
    );

    assert!(load.servers.is_empty());
    assert_eq!(load.warnings.len(), 1);
    assert!(load.warnings[0].contains("invalid JSON"));
}

#[test]
fn missing_explicit_file_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("missing.json");
    let error = discover_mcp_servers(
        std::slice::from_ref(&missing),
        tmp.path(),
        tmp.path(),
        tmp.path(),
        &trust(TrustState::NotRequired),
        ProjectConfigMode::Enforce,
        None,
    )
    .unwrap_err();

    assert!(error.to_string().contains("does not exist"));
    assert!(matches!(error, McpConfigError::Missing { .. }));
}

#[test]
fn malformed_explicit_file_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("bad.json");
    write(&path, "[]");
    let error = discover_mcp_servers(
        &[path],
        tmp.path(),
        tmp.path(),
        tmp.path(),
        &trust(TrustState::NotRequired),
        ProjectConfigMode::Enforce,
        None,
    )
    .unwrap_err();

    assert!(matches!(error, McpConfigError::Invalid { .. }));
}

#[test]
fn invalid_servers_are_skipped_with_warnings() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("mcp.json");
    write(
        &path,
        r#"{
          "mcpServers": {
            "bad name": {"command": "one"},
            "remote": {"url": "https://example.test/mcp"},
            "sse": {"type": "sse", "command": "two"},
            "empty": {},
            "nul": {"command": "three\u0000four"},
            "bad-env": {"command": "four", "env": {"9BAD": "x"}},
            "bad-exposure": {"command": "four", "exposure": "eager"},
            "bad-description": {"command": "four", "description": "bad\u0000text"},
            "disabled": {"command": "five", "disabled": true},
            "ok": {"command": "six", "timeout_secs": 99999}
          }
        }"#,
    );

    let load = discover(
        tmp.path(),
        tmp.path(),
        &[path],
        &trust(TrustState::NotRequired),
        ProjectConfigMode::Enforce,
    );

    let names: Vec<&str> = load
        .servers
        .iter()
        .map(|server| server.name.as_str())
        .collect();
    assert_eq!(names, ["ok"]);
    assert_eq!(load.servers[0].timeout_secs, MAX_MCP_TIMEOUT_SECS);
    assert_eq!(load.warnings.len(), 8, "{:?}", load.warnings);
}

#[test]
fn disabled_server_is_skipped_without_a_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("mcp.json");
    write(
        &path,
        r#"{"mcpServers":{"off":{"command":"cmd","disabled":true}}}"#,
    );
    let load = discover(
        tmp.path(),
        tmp.path(),
        &[path],
        &trust(TrustState::NotRequired),
        ProjectConfigMode::Enforce,
    );
    assert!(load.servers.is_empty());
    assert!(load.warnings.is_empty());
}

#[test]
fn registered_names_are_provider_safe() {
    assert_eq!(
        registered_tool_name("safari-mcp", "list_tabs").as_deref(),
        Some("mcp__safari-mcp__list_tabs")
    );
    assert_eq!(
        registered_tool_name("server", "get.page").as_deref(),
        Some("mcp__server__get_page")
    );
    assert_eq!(registered_tool_name("server", "!!!"), None);
}

#[test]
fn long_registered_names_are_truncated_with_a_stable_suffix() {
    let server = "s";
    let tool = "a".repeat(120);
    let first = registered_tool_name(server, &tool).unwrap();
    let second = registered_tool_name(server, &tool).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.len(), MAX_REGISTERED_NAME_LEN);
    assert!(first.starts_with("mcp__s__"));
    assert_ne!(
        first,
        registered_tool_name(server, &format!("{}b", "a".repeat(119))).unwrap()
    );
}

#[test]
fn server_then_tool_truncation_stays_under_the_limit() {
    let name = registered_tool_name("a".repeat(MAX_SERVER_NAME_LEN).as_str(), "list_tabs").unwrap();
    assert!(name.len() <= MAX_REGISTERED_NAME_LEN);
}

#[test]
fn environment_map_is_preserved_in_server_source_order() {
    let server = build_server(
        "s",
        "cmd",
        Vec::new(),
        BTreeMap::from([("A".to_string(), "1".to_string())]),
        None,
        McpExposure::Direct,
        None,
        Path::new("mcp.json"),
    );
    assert_eq!(server.env.get("A").unwrap(), "1");
}

fn draft(name: &str) -> McpServerDraft {
    McpServerDraft {
        name: name.to_string(),
        command: "python3".to_string(),
        args: vec!["server.py".to_string()],
        env: BTreeMap::from([("FAKE_MCP_ERA".to_string(), "modern".to_string())]),
        timeout_secs: 30,
        description: None,
        exposure: McpExposure::default(),
    }
}

#[test]
fn upsert_creates_a_discoverable_global_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let path = global_config_path(tmp.path());

    upsert_server(&path, &draft("fixture")).unwrap();

    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let entry = &parsed["mcpServers"]["fixture"];
    assert_eq!(entry["command"], "python3");
    assert_eq!(entry["args"][0], "server.py");
    assert_eq!(entry["env"]["FAKE_MCP_ERA"], "modern");
    assert_eq!(entry["timeout_secs"], 30);
    assert!(entry.get("description").is_none());
    assert!(entry.get("exposure").is_none());

    let load = discover(
        tmp.path(),
        tmp.path(),
        &[],
        &trust(TrustState::NotRequired),
        ProjectConfigMode::Enforce,
    );
    assert_eq!(load.servers.len(), 1);
    assert_eq!(load.servers[0].name, "fixture");
    assert_eq!(load.servers[0].timeout_secs, 30);
}

#[test]
fn upsert_replaces_one_entry_and_preserves_the_rest_of_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = global_config_path(tmp.path());
    write(
        &path,
        r#"{
          "meta": {"keep": true},
          "mcpServers": {
            "alpha": {"command": "old", "disabled": true},
            "beta": {"command": "keep", "custom": 7}
          }
        }"#,
    );

    upsert_server(&path, &draft("alpha")).unwrap();

    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(parsed["meta"]["keep"], true);
    assert_eq!(parsed["mcpServers"]["alpha"]["command"], "python3");
    assert!(parsed["mcpServers"]["alpha"].get("disabled").is_none());
    assert_eq!(parsed["mcpServers"]["beta"]["custom"], 7);
}

#[test]
fn upsert_omits_default_timeout_and_empty_collections() {
    let tmp = tempfile::tempdir().unwrap();
    let path = global_config_path(tmp.path());
    let mut minimal = draft("minimal");
    minimal.args.clear();
    minimal.env.clear();
    minimal.timeout_secs = DEFAULT_MCP_TIMEOUT_SECS;

    upsert_server(&path, &minimal).unwrap();

    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let entry = parsed["mcpServers"]["minimal"].as_object().unwrap();
    assert_eq!(entry.len(), 1);
    assert_eq!(entry["command"], "python3");
}

#[test]
fn upsert_writes_description_and_non_default_exposure() {
    let tmp = tempfile::tempdir().unwrap();
    let path = global_config_path(tmp.path());
    let mut configured = draft("fixture");
    configured.description = Some("  Browser automation.  ".to_string());
    configured.exposure = McpExposure::Hidden;

    upsert_server(&path, &configured).unwrap();

    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let entry = &parsed["mcpServers"]["fixture"];
    assert_eq!(entry["description"], "Browser automation.");
    assert_eq!(entry["exposure"], "hidden");
}

#[test]
fn upsert_leaves_a_malformed_file_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let path = global_config_path(tmp.path());
    let original = "{ not json";
    write(&path, original);

    let error = upsert_server(&path, &draft("fixture")).unwrap_err();

    assert!(matches!(error, McpManageError::Invalid { .. }), "{error}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn upsert_rejects_an_invalid_draft() {
    let tmp = tempfile::tempdir().unwrap();
    let path = global_config_path(tmp.path());
    let mut invalid_name = draft("bad name");
    invalid_name.name = "bad name".to_string();
    assert!(matches!(
        upsert_server(&path, &invalid_name),
        Err(McpManageError::InvalidName { .. })
    ));

    let mut invalid_command = draft("fixture");
    invalid_command.command = "  ".to_string();
    assert!(matches!(
        upsert_server(&path, &invalid_command),
        Err(McpManageError::InvalidCommand { .. })
    ));

    let mut invalid_timeout = draft("fixture");
    invalid_timeout.timeout_secs = 0;
    assert!(matches!(
        upsert_server(&path, &invalid_timeout),
        Err(McpManageError::InvalidTimeout { .. })
    ));

    let mut invalid_env = draft("fixture");
    invalid_env
        .env
        .insert("BAD-NAME".to_string(), "value".to_string());
    assert!(matches!(
        upsert_server(&path, &invalid_env),
        Err(McpManageError::InvalidEnv { .. })
    ));

    let mut invalid_description = draft("fixture");
    invalid_description.description = Some("bad\0description".to_string());
    assert!(matches!(
        upsert_server(&path, &invalid_description),
        Err(McpManageError::InvalidDescription { .. })
    ));
    assert!(!path.exists(), "validation must not create the file");
}

#[test]
fn remove_deletes_one_entry_and_reports_presence() {
    let tmp = tempfile::tempdir().unwrap();
    let path = global_config_path(tmp.path());
    write(
        &path,
        r#"{"mcpServers":{"alpha":{"command":"a"},"beta":{"command":"b"}}}"#,
    );

    assert!(remove_server(&path, "alpha").unwrap());
    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(parsed["mcpServers"].get("alpha").is_none());
    assert_eq!(parsed["mcpServers"]["beta"]["command"], "b");

    let before = std::fs::read_to_string(&path).unwrap();
    assert!(!remove_server(&path, "alpha").unwrap());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

#[test]
fn remove_of_a_missing_file_is_a_no_op() {
    let tmp = tempfile::tempdir().unwrap();
    let path = global_config_path(tmp.path());

    assert!(!remove_server(&path, "fixture").unwrap());
    assert!(!path.exists());
}

#[cfg(unix)]
#[test]
fn upsert_writes_owner_only_permissions() {
    use std::os::unix::fs::PermissionsExt as _;

    let tmp = tempfile::tempdir().unwrap();
    let path = global_config_path(tmp.path());

    upsert_server(&path, &draft("fixture")).unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[cfg(unix)]
#[test]
fn upsert_writes_through_a_symlink_without_replacing_it() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("dotfiles-mcp.json");
    let link = global_config_path(tmp.path());
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    write(&target, r#"{"mcpServers":{}}"#);
    symlink(&target, &link).unwrap();

    upsert_server(&link, &draft("fixture")).unwrap();

    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(parsed["mcpServers"]["fixture"]["command"], "python3");
}
