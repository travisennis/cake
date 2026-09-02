use super::*;

// ── toolbox_directories ──

const BASE: &str = "/invocation/dir";

fn base() -> &'static Path {
    Path::new(BASE)
}

#[test]
fn directories_default_when_env_unset() {
    let default_dir = Path::new("/home/user/.config/cake/tools");
    let dirs = toolbox_directories(None, &[], default_dir, base(), Path::new("/project"));
    assert_eq!(
        dirs,
        vec![
            default_dir.to_path_buf(),
            PathBuf::from("/project/.cake/tools")
        ]
    );
}

#[test]
fn directories_env_replaces_default() {
    let dirs = toolbox_directories(
        Some("/a:/b"),
        &[],
        Path::new("/default"),
        base(),
        Path::new("/project"),
    );
    assert_eq!(
        dirs,
        vec![
            PathBuf::from("/a"),
            PathBuf::from("/b"),
            PathBuf::from("/project/.cake/tools"),
        ]
    );
}

#[test]
fn directories_empty_env_preserves_project_local_tools() {
    let dirs = toolbox_directories(
        Some(""),
        &[],
        Path::new("/default"),
        base(),
        Path::new("/project"),
    );
    assert_eq!(dirs, vec![PathBuf::from("/project/.cake/tools")]);
}

#[test]
fn directories_skips_empty_env_segments() {
    let dirs = toolbox_directories(
        Some("/a::/b:"),
        &[],
        Path::new("/default"),
        base(),
        Path::new("/project"),
    );
    assert_eq!(
        dirs,
        vec![
            PathBuf::from("/a"),
            PathBuf::from("/b"),
            PathBuf::from("/project/.cake/tools"),
        ]
    );
}

#[test]
fn directories_extra_dirs_appended_after_env() {
    let extra = vec![PathBuf::from("/extra")];
    let dirs = toolbox_directories(
        Some("/a"),
        &extra,
        Path::new("/default"),
        base(),
        Path::new("/project"),
    );
    assert_eq!(
        dirs,
        vec![
            PathBuf::from("/a"),
            PathBuf::from("/extra"),
            PathBuf::from("/project/.cake/tools"),
        ]
    );

    let dirs = toolbox_directories(
        None,
        &extra,
        Path::new("/default"),
        base(),
        Path::new("/project"),
    );
    assert_eq!(
        dirs,
        vec![
            PathBuf::from("/default"),
            PathBuf::from("/extra"),
            PathBuf::from("/project/.cake/tools"),
        ]
    );

    let dirs = toolbox_directories(
        Some(""),
        &extra,
        Path::new("/default"),
        base(),
        Path::new("/project"),
    );
    assert_eq!(
        dirs,
        vec![
            PathBuf::from("/extra"),
            PathBuf::from("/project/.cake/tools"),
        ]
    );
}

#[test]
fn directories_relative_entries_anchored_to_base_dir() {
    // Relative env and flag entries resolve against the invocation
    // directory, not whatever the process cwd becomes (e.g. a worktree).
    let extra = vec![PathBuf::from("flag/tools")];
    let dirs = toolbox_directories(
        Some("env/tools:/abs"),
        &extra,
        Path::new("/default"),
        base(),
        Path::new("/project"),
    );
    assert_eq!(
        dirs,
        vec![
            PathBuf::from(format!("{BASE}/env/tools")),
            PathBuf::from("/abs"),
            PathBuf::from(format!("{BASE}/flag/tools")),
            PathBuf::from("/project/.cake/tools"),
        ]
    );
}

#[test]
fn directories_project_local_uses_active_worktree_root() {
    let dirs = toolbox_directories(
        Some("/configured"),
        &[],
        Path::new("/default"),
        base(),
        Path::new("/active/worktree"),
    );
    assert_eq!(
        dirs,
        vec![
            PathBuf::from("/configured"),
            PathBuf::from("/active/worktree/.cake/tools"),
        ]
    );
}

#[test]
fn directories_deduplicate_configured_project_local_path() {
    let dirs = toolbox_directories(
        Some(".cake/tools"),
        &[PathBuf::from(".cake/tools")],
        Path::new("/default"),
        base(),
        base(),
    );
    assert_eq!(dirs, vec![PathBuf::from(format!("{BASE}/.cake/tools"))]);
}

// ── discovery filtering ──

#[cfg(unix)]
fn write_executable(dir: &Path, name: &str, content: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[cfg(unix)]
#[test]
fn discovery_filters_and_sorts() {
    let dir = tempfile::tempdir().unwrap();
    write_executable(dir.path(), "zeta", "#!/bin/sh\n");
    write_executable(dir.path(), "alpha", "#!/bin/sh\n");
    write_executable(dir.path(), ".hidden", "#!/bin/sh\n");
    write_executable(dir.path(), "notes.md", "#!/bin/sh\n");
    write_executable(dir.path(), "readme.TXT", "#!/bin/sh\n");
    std::fs::write(dir.path().join("plain"), "not executable").unwrap();
    std::fs::create_dir(dir.path().join("subdir")).unwrap();

    let entries = discover_toolbox_entries(&[dir.path().to_path_buf()]);
    let names: Vec<&str> = entries.iter().map(|e| e.filename.as_str()).collect();
    assert_eq!(names, vec!["alpha", "zeta"]);
}

#[cfg(unix)]
#[test]
fn discovery_preserves_directory_order() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    write_executable(first.path(), "tool_b", "#!/bin/sh\n");
    write_executable(second.path(), "tool_a", "#!/bin/sh\n");

    let entries =
        discover_toolbox_entries(&[first.path().to_path_buf(), second.path().to_path_buf()]);
    let names: Vec<&str> = entries.iter().map(|e| e.filename.as_str()).collect();
    assert_eq!(names, vec!["tool_b", "tool_a"]);
}

#[test]
fn discovery_skips_missing_directory() {
    let entries = discover_toolbox_entries(&[PathBuf::from("/nonexistent/toolbox/dir")]);
    assert!(entries.is_empty());
}

// ── describe parsing: JSON format ──

fn parse(stdout: &str) -> Result<ToolboxTool, String> {
    parse_describe_output(stdout, Path::new("/tools/fixture"))
}

#[test]
fn json_compact_args_format() {
    let tool = parse(
        r#"{"name": "run_tests", "description": "Run tests. Extra detail.",
            "args": {"pattern": ["string?", "Test filter"], "count": ["integer", "Run count"]}}"#,
    )
    .unwrap();
    assert_eq!(tool.registered_name, "tb__run_tests");
    assert_eq!(tool.original_name, "run_tests");
    assert_eq!(tool.description, "Run tests. Extra detail.");
    assert_eq!(tool.format, ToolboxFormat::Json);
    assert_eq!(tool.timeout_secs, DEFAULT_EXECUTE_TIMEOUT_SECS);
    assert_eq!(
        tool.parameters,
        serde_json::json!({
            "type": "object",
            "properties": {
                "count": { "type": "integer", "description": "Run count" },
                "pattern": { "type": "string", "description": "Test filter" }
            },
            "required": ["count"]
        })
    );
}

#[test]
fn json_full_object_input_schema_used_verbatim() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": { "items": { "type": "array", "items": { "type": "string" } } },
        "required": ["items"]
    });
    let tool = parse(&format!(
        r#"{{"name": "batch", "description": "Batch.", "inputSchema": {schema}}}"#
    ))
    .unwrap();
    assert_eq!(tool.parameters, schema);
}

#[test]
fn json_full_input_schema_without_type_is_normalized_to_object() {
    let tool =
        parse(r#"{"name": "batch", "inputSchema": {"properties": {"item": {"type": "string"}}}}"#)
            .unwrap();
    assert_eq!(tool.parameters["type"], "object");
    assert_eq!(tool.parameters["properties"]["item"]["type"], "string");
}

#[test]
fn json_full_non_object_input_schema_rejected() {
    let err = parse(
        r#"{"name": "batch", "inputSchema": {"type": "array", "items": {"type": "string"}}}"#,
    )
    .unwrap_err();
    assert!(err.contains("top-level object"), "unexpected error: {err}");
}

#[test]
fn json_invalid_full_input_schema_rejected() {
    let err = parse(
        r#"{"name": "batch", "inputSchema": {"type": "object", "required": ["item", "item"]}}"#,
    )
    .unwrap_err();
    assert!(
        err.contains("not valid JSON Schema draft 2020-12"),
        "unexpected error: {err}"
    );
}

#[test]
fn json_without_args_gets_empty_schema() {
    let tool = parse(r#"{"name": "ping", "description": "Ping."}"#).unwrap();
    assert_eq!(
        tool.parameters,
        serde_json::json!({ "type": "object", "properties": {} })
    );
}

#[test]
fn json_timeout_field_overrides_default() {
    let tool = parse(r#"{"name": "slow", "description": "Slow.", "timeout": 300}"#).unwrap();
    assert_eq!(tool.timeout_secs, 300);
}

#[test]
fn json_invalid_timeout_rejected() {
    let err = parse(r#"{"name": "bad", "timeout": 0}"#).unwrap_err();
    assert!(err.contains("timeout"), "unexpected error: {err}");
    let err = parse(r#"{"name": "bad", "timeout": "60"}"#).unwrap_err();
    assert!(err.contains("timeout"), "unexpected error: {err}");
}

#[test]
fn json_missing_name_rejected() {
    let err = parse(r#"{"description": "No name."}"#).unwrap_err();
    assert!(err.contains("name"), "unexpected error: {err}");
}

#[test]
fn json_malformed_args_pair_rejected() {
    let err = parse(r#"{"name": "bad", "args": {"p": "string"}}"#).unwrap_err();
    assert!(err.contains("'p'"), "unexpected error: {err}");
}

#[test]
fn json_unsupported_arg_type_rejected() {
    let err = parse(r#"{"name": "bad", "args": {"p": ["object", "d"]}}"#).unwrap_err();
    assert!(err.contains("unsupported type"), "unexpected error: {err}");
}

#[test]
fn json_non_object_input_schema_rejected() {
    let err = parse(r#"{"name": "bad", "inputSchema": "not a schema"}"#).unwrap_err();
    assert!(err.contains("inputSchema"), "unexpected error: {err}");
}

// ── describe parsing: text format ──

#[test]
fn text_format_with_params_and_multiline_description() {
    let tool = parse(
        "name: deploy\n\
         description: Deploy the app.\n\
         description: Second line.\n\
         \n\
         env: string Target environment\n\
         dry_run: boolean? Skip side effects\n",
    )
    .unwrap();
    assert_eq!(tool.registered_name, "tb__deploy");
    assert_eq!(tool.description, "Deploy the app.\nSecond line.");
    assert_eq!(tool.format, ToolboxFormat::Text);
    assert_eq!(
        tool.parameters,
        serde_json::json!({
            "type": "object",
            "properties": {
                "dry_run": { "type": "boolean", "description": "Skip side effects" },
                "env": { "type": "string", "description": "Target environment" }
            },
            "required": ["env"]
        })
    );
}

#[test]
fn text_format_missing_name_rejected() {
    let err = parse("description: No name here.").unwrap_err();
    assert!(err.contains("name"), "unexpected error: {err}");
}

#[test]
fn text_format_param_without_explicit_type_rejected() {
    let err = parse("name: t\npattern: the filter to use").unwrap_err();
    assert!(err.contains("unsupported type"), "unexpected error: {err}");
}

#[test]
fn text_format_param_with_unencodable_name_rejected() {
    let err = parse("name: t\ntarget=value: string Target").unwrap_err();
    assert!(err.contains("argument name"), "unexpected error: {err}");
    assert!(err.contains("target=value"), "unexpected error: {err}");
}

#[test]
fn text_format_duplicate_parameter_rejected() {
    let err = parse("name: t\ntarget: string First\ntarget: string Second").unwrap_err();
    assert!(err.contains("duplicate"), "unexpected error: {err}");
    assert!(err.contains("target"), "unexpected error: {err}");
}

#[test]
fn text_format_line_without_colon_rejected() {
    let err = parse("name: t\njust some prose").unwrap_err();
    assert!(err.contains("unparseable"), "unexpected error: {err}");
}

#[test]
fn text_format_param_with_underscores_and_no_description() {
    let tool = parse("name: t\nmax_count: integer").unwrap();
    assert_eq!(
        tool.parameters["properties"]["max_count"],
        serde_json::json!({ "type": "integer", "description": "" })
    );
}

// ── shared validation ──

#[test]
fn empty_describe_output_rejected() {
    let err = parse("   \n  ").unwrap_err();
    assert!(err.contains("no output"), "unexpected error: {err}");
}

#[test]
fn tool_name_with_invalid_characters_rejected() {
    let err = parse(r#"{"name": "bad name!"}"#).unwrap_err();
    assert!(err.contains("bad name!"), "unexpected error: {err}");
}

#[test]
fn tool_name_longer_than_provider_limit_rejected() {
    // 60 characters is the maximum: tb__ + 60 = the 64-character provider
    // function-name limit.
    let max_name = "a".repeat(60);
    let tool = parse(&format!(r#"{{"name": "{max_name}"}}"#)).unwrap();
    assert_eq!(tool.registered_name.len(), 64);

    let over = "a".repeat(61);
    let err = parse(&format!(r#"{{"name": "{over}"}}"#)).unwrap_err();
    assert!(err.contains("61 characters"), "unexpected error: {err}");
}

// ── describe subprocess integration ──

#[cfg(unix)]
#[tokio::test]
async fn load_toolbox_tools_full_cycle_and_precedence() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    write_executable(
        first.path(),
        "greet",
        "#!/bin/sh\nprintf '{\"name\": \"greet\", \"description\": \"Greets.\"}'\n",
    );
    // Same describe name from a later directory: skipped as a duplicate.
    write_executable(
        second.path(),
        "other_file",
        "#!/bin/sh\nprintf '{\"name\": \"greet\", \"description\": \"Duplicate.\"}'\n",
    );
    // Text-format tool.
    write_executable(
        second.path(),
        "lint",
        "#!/bin/sh\nprintf 'name: lint\\ndescription: Lints.\\n'\n",
    );
    // Broken tool: non-zero describe exit is skipped without failing startup.
    write_executable(first.path(), "broken", "#!/bin/sh\nexit 3\n");

    let tools =
        load_toolbox_tools(&[first.path().to_path_buf(), second.path().to_path_buf()]).await;

    let names: Vec<&str> = tools.iter().map(|t| t.registered_name.as_str()).collect();
    assert_eq!(names, vec!["tb__greet", "tb__lint"]);
    assert_eq!(tools[0].description, "Greets.");
    assert_eq!(tools[0].format, ToolboxFormat::Json);
    assert_eq!(tools[1].format, ToolboxFormat::Text);
}

#[cfg(unix)]
#[tokio::test]
async fn runaway_describe_output_is_capped_and_tool_skipped() {
    let dir = tempfile::tempdir().unwrap();
    // A broken tool that streams endless describe output must be skipped
    // quickly with bounded memory, not buffered until the timeout.
    write_executable(dir.path(), "runaway", "#!/bin/sh\nexec yes\n");
    // A healthy sibling proves one broken tool does not block the rest.
    write_executable(
        dir.path(),
        "healthy",
        "#!/bin/sh\nprintf '{\"name\": \"healthy\", \"description\": \"OK.\"}'\n",
    );

    let started = std::time::Instant::now();
    let tools = load_toolbox_tools(&[dir.path().to_path_buf()]).await;
    assert!(
        started.elapsed() < std::time::Duration::from_secs(8),
        "runaway describe output must be cut off by the cap, not the timeout"
    );
    let names: Vec<&str> = tools.iter().map(|t| t.registered_name.as_str()).collect();
    assert_eq!(names, vec!["tb__healthy"]);
}

#[cfg(unix)]
#[tokio::test]
async fn runaway_describe_output_kills_descendants() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("describe-descendant-survived");
    let script = format!(
        "#!/bin/sh\n(sleep 2; touch '{}') &\nexec yes\n",
        marker.display()
    );
    write_executable(dir.path(), "runaway_with_child", &script);

    let tools = load_toolbox_tools(&[dir.path().to_path_buf()]).await;
    assert!(tools.is_empty());
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert!(
        !marker.exists(),
        "a descendant survived the describe output cap and mutated the workspace"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn describe_uses_name_from_output_not_filename() {
    let dir = tempfile::tempdir().unwrap();
    write_executable(
        dir.path(),
        "some-file-name",
        "#!/bin/sh\nprintf '{\"name\": \"actual_name\", \"description\": \"D.\"}'\n",
    );
    let tools = load_toolbox_tools(&[dir.path().to_path_buf()]).await;
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].registered_name, "tb__actual_name");
}
