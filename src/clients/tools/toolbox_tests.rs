use super::*;
use std::path::{Path, PathBuf};

fn fixture_tool(path: PathBuf, format: ToolboxFormat) -> ToolboxTool {
    ToolboxTool {
        registered_name: "tb__fixture".to_string(),
        original_name: "fixture".to_string(),
        path,
        description: "Test fixture tool.".to_string(),
        parameters: serde_json::json!({ "type": "object", "properties": {} }),
        format,
        timeout_secs: 5,
        replay: crate::types::ReplaySafety::Never,
    }
}

#[cfg(unix)]
fn write_executable(dir: &Path, name: &str, content: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

// ── argument serialization ──

#[test]
fn key_value_lines_render_strings_verbatim_and_others_as_json() {
    let args = serde_json::json!({
        "name": "some value",
        "count": 3,
        "enabled": true
    });
    let lines = arguments_to_key_value_lines(&args).unwrap();
    assert!(lines.contains("name=some value\n"));
    assert!(lines.contains("count=3\n"));
    assert!(lines.contains("enabled=true\n"));
}

#[test]
fn key_value_lines_reject_multiline_strings() {
    let args = serde_json::json!({ "message": "hello\nadmin=true" });
    let err = arguments_to_key_value_lines(&args).unwrap_err();
    assert!(err.contains("multiline"), "unexpected error: {err}");
}

#[test]
fn key_value_lines_reject_structural_characters_in_names() {
    let args = serde_json::json!({ "message\nadmin": "true" });
    let err = arguments_to_key_value_lines(&args).unwrap_err();
    assert!(err.contains("argument name"), "unexpected error: {err}");
}

#[test]
fn parse_arguments_accepts_empty_string_as_empty_object() {
    let value = parse_arguments("tb__t", "").unwrap();
    assert_eq!(value, serde_json::json!({}));
}

#[test]
fn parse_arguments_rejects_non_object() {
    let err = parse_arguments("tb__t", "[1, 2]").unwrap_err();
    assert!(err.contains("JSON object"), "unexpected error: {err}");
}

#[test]
fn parse_arguments_repairs_control_characters() {
    let value = parse_arguments("tb__t", "{\"a\": \"line1\nline2\"}").unwrap();
    assert_eq!(value["a"], "line1\nline2");
}

// ── output truncation ──

#[test]
fn truncate_output_passes_small_output_through() {
    assert_eq!(truncate_output("small".to_string()), "small");
}

#[test]
fn truncate_output_caps_large_output_with_marker() {
    let large = "a".repeat(MAX_OUTPUT_BYTES + 100);
    let truncated = truncate_output(large);
    assert!(truncated.len() < MAX_OUTPUT_BYTES + 100);
    assert!(truncated.ends_with("bytes ...]"));
}

#[test]
fn truncate_output_retreats_to_utf8_boundary() {
    let large = format!("{}😀", "a".repeat(MAX_OUTPUT_BYTES - 1));
    let truncated = truncate_output(large);
    assert!(truncated.starts_with(&"a".repeat(MAX_OUTPUT_BYTES - 1)));
    assert!(truncated.ends_with("bytes ...]"));
}

// ── subprocess execution ──

#[cfg(unix)]
mod subprocess {
    use super::*;

    async fn run(tool: &ToolboxTool, cwd: &Path, arguments: &str) -> Result<ToolResult, ToolError> {
        let session_id = "11111111-2222-3333-4444-555555555555";
        execute_toolbox_tool(tool, session_id, cwd, arguments).await
    }

    #[tokio::test]
    async fn json_format_receives_arguments_on_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_executable(dir.path(), "echo_stdin", "#!/bin/sh\ncat\n");
        let tool = fixture_tool(path, ToolboxFormat::Json);

        let result = run(&tool, dir.path(), r#"{"key": "value"}"#).await.unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&result.output).unwrap(),
            serde_json::json!({ "key": "value" })
        );
    }

    #[tokio::test]
    async fn text_format_receives_key_value_lines_on_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_executable(dir.path(), "echo_stdin", "#!/bin/sh\ncat\n");
        let tool = fixture_tool(path, ToolboxFormat::Text);

        let result = run(&tool, dir.path(), r#"{"key": "value"}"#).await.unwrap();
        assert_eq!(result.output, "key=value\n");
    }

    #[tokio::test]
    async fn text_format_rejects_multiline_values_before_spawning_tool() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("tool-ran");
        let script = format!("#!/bin/sh\ntouch '{}'\ncat\n", marker.display());
        let path = write_executable(dir.path(), "echo_stdin", &script);
        let tool = fixture_tool(path, ToolboxFormat::Text);

        let err = run(&tool, dir.path(), r#"{"message": "hello\nadmin=true"}"#)
            .await
            .unwrap_err();
        assert!(err.message.contains("multiline"), "unexpected error: {err}");
        assert!(!marker.exists(), "invalid arguments must fail before spawn");
    }

    #[tokio::test]
    async fn execute_environment_exposes_action_agent_and_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_executable(
            dir.path(),
            "env_probe",
            "#!/bin/sh\nprintf '%s|%s|%s|%s' \
             \"$TOOLBOX_ACTION\" \"$AGENT\" \"$CAKE_THREAD_ID\" \"$AGENT_THREAD_ID\"\n",
        );
        let tool = fixture_tool(path, ToolboxFormat::Json);

        let result = run(&tool, dir.path(), "{}").await.unwrap();
        assert_eq!(
            result.output,
            "execute|cake|11111111-2222-3333-4444-555555555555|\
             11111111-2222-3333-4444-555555555555"
        );
    }

    #[tokio::test]
    async fn tool_runs_in_the_context_working_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_executable(dir.path(), "pwd_probe", "#!/bin/sh\npwd\n");
        let tool = fixture_tool(path, ToolboxFormat::Json);
        let cwd = dir.path().canonicalize().unwrap();

        let result = run(&tool, &cwd, "{}").await.unwrap();
        assert_eq!(Path::new(result.output.trim()), cwd);
    }

    #[tokio::test]
    async fn nonzero_exit_returns_error_with_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_executable(
            dir.path(),
            "fails",
            "#!/bin/sh\necho 'diagnostic detail' >&2\nexit 3\n",
        );
        let tool = fixture_tool(path, ToolboxFormat::Json);

        let err = run(&tool, dir.path(), "{}").await.unwrap_err();
        assert!(
            err.message.contains("tb__fixture"),
            "unexpected error: {err}"
        );
        assert!(
            err.message.contains("diagnostic detail"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn nonzero_exit_uses_stdout_when_stderr_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_executable(
            dir.path(),
            "fails",
            "#!/bin/sh\nprintf 'stdout detail'\nexit 4\n",
        );
        let tool = fixture_tool(path, ToolboxFormat::Json);

        let err = run(&tool, dir.path(), "{}").await.unwrap_err();
        assert!(
            err.message.contains("stdout detail"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn missing_executable_reports_path_and_tool_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing-tool");
        let tool = fixture_tool(path.clone(), ToolboxFormat::Json);

        let err = run(&tool, dir.path(), "{}").await.unwrap_err();
        assert!(
            err.message.contains("tb__fixture"),
            "unexpected error: {err}"
        );
        assert!(
            err.message.contains(&path.display().to_string()),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn timeout_kills_the_tool_and_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_executable(dir.path(), "sleeps", "#!/bin/sh\nsleep 30\n");
        let mut tool = fixture_tool(path, ToolboxFormat::Json);
        tool.timeout_secs = 1;

        let err = run(&tool, dir.path(), "{}").await.unwrap_err();
        assert!(
            err.message.contains("timed out after 1"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn timeout_kills_descendants_before_they_can_mutate_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("timeout-descendant-survived");
        let script = format!(
            "#!/bin/sh\n(sleep 2; touch '{}') &\nsleep 30\n",
            marker.display()
        );
        let path = write_executable(dir.path(), "spawns_child", &script);
        let mut tool = fixture_tool(path, ToolboxFormat::Json);
        tool.timeout_secs = 1;

        let err = run(&tool, dir.path(), "{}").await.unwrap_err();
        assert!(
            err.message.contains("timed out after 1"),
            "unexpected error: {err}"
        );
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        assert!(
            !marker.exists(),
            "a descendant survived the toolbox timeout and mutated the workspace"
        );
    }

    #[tokio::test]
    async fn timeout_applies_when_tool_never_reads_oversized_stdin() {
        let dir = tempfile::tempdir().unwrap();
        // The tool sleeps without reading stdin; arguments far beyond the
        // OS pipe capacity would block the stdin write forever if it were
        // not detached from the timed section.
        let path = write_executable(dir.path(), "ignores_stdin", "#!/bin/sh\nsleep 30\n");
        let mut tool = fixture_tool(path, ToolboxFormat::Json);
        tool.timeout_secs = 1;

        let big_value = "x".repeat(2_000_000);
        let arguments = format!("{{\"payload\": \"{big_value}\"}}");
        let started = std::time::Instant::now();
        let err = run(&tool, dir.path(), &arguments).await.unwrap_err();
        assert!(
            err.message.contains("timed out after 1"),
            "unexpected error: {err}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "call must not hang past the timeout"
        );
    }

    #[tokio::test]
    async fn oversized_stdout_is_capped_and_tool_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        // Emits ~200KB, four times the cap; unbounded buffering would keep
        // it all in memory and a naive reader would wait for completion.
        let path = write_executable(
            dir.path(),
            "floods",
            "#!/bin/sh\nhead -c 200000 /dev/zero | tr '\\0' 'a'\n",
        );
        let tool = fixture_tool(path, ToolboxFormat::Json);

        let result = run(&tool, dir.path(), "{}").await.unwrap();
        assert!(
            result.output.len() <= MAX_OUTPUT_BYTES + 100,
            "output must be capped near {MAX_OUTPUT_BYTES} bytes, got {}",
            result.output.len()
        );
        assert!(
            result.output.ends_with("bytes ...]"),
            "capped output must end with the truncation marker"
        );
    }

    #[tokio::test]
    async fn output_cap_kills_descendants_before_they_can_mutate_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("output-cap-descendant-survived");
        let script = format!(
            "#!/bin/sh\n(sleep 2; touch '{}') &\nexec yes\n",
            marker.display()
        );
        let path = write_executable(dir.path(), "floods_with_child", &script);
        let tool = fixture_tool(path, ToolboxFormat::Json);

        let result = run(&tool, dir.path(), "{}").await.unwrap();
        assert!(result.output.ends_with("bytes ...]"));
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        assert!(
            !marker.exists(),
            "a descendant survived the toolbox output cap and mutated the workspace"
        );
    }

    #[tokio::test]
    async fn tool_that_ignores_stdin_still_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_executable(dir.path(), "no_stdin", "#!/bin/sh\nexec printf 'done'\n");
        let tool = fixture_tool(path, ToolboxFormat::Json);

        let result = run(&tool, dir.path(), r#"{"ignored": "payload"}"#)
            .await
            .unwrap();
        assert_eq!(result.output, "done");
    }
}
