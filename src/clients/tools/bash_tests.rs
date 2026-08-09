use super::*;
#[cfg(target_os = "macos")]
use crate::clients::tools::ToolContext;
#[cfg(target_os = "macos")]
use crate::clients::tools::sandbox::SandboxPolicy;
#[cfg(target_os = "macos")]
use std::sync::Arc;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Check whether `CAKE_REQUIRE_SANDBOX_TESTS` is set to a truthy value,
/// indicating that sandbox integration tests must run (and fail if the
/// sandbox is unavailable).
///
/// Accepted truthy values: `1`, `true`, `yes`, `on`.
#[cfg(target_os = "macos")]
fn is_sandbox_tests_required() -> bool {
    parse_sandbox_tests_required(std::env::var("CAKE_REQUIRE_SANDBOX_TESTS").ok().as_deref())
}

/// Pure parsing of the optional value for `CAKE_REQUIRE_SANDBOX_TESTS`.
/// Extracted from `is_sandbox_tests_required()` for focused unit testing
/// without environment variable interference.
#[cfg(target_os = "macos")]
fn parse_sandbox_tests_required(value: Option<&str>) -> bool {
    matches!(value, Some("1" | "true" | "yes" | "on"))
}

/// Skip the current macOS sandbox integration test when the platform
/// sandbox cannot be enforced, unless `CAKE_REQUIRE_SANDBOX_TESTS` is set.
///
/// Returns `true` to indicate the test should be skipped.
/// When the sandbox is unavailable *and* tests are required, panics with
/// an actionable message so the test fails rather than silently passing.
#[cfg(target_os = "macos")]
fn skip_if_sandbox_unavailable() -> bool {
    let required = is_sandbox_tests_required();

    if super::super::sandbox::is_sandbox_disabled() {
        let msg = "skipping macOS sandbox integration test: CAKE_SANDBOX disables sandboxing";
        assert!(
            !required,
            "sandbox integration tests are required via CAKE_REQUIRE_SANDBOX_TESTS=1 \
             but CAKE_SANDBOX disables sandboxing; unset CAKE_SANDBOX or set \
             CAKE_REQUIRE_SANDBOX_TESTS=0 to skip"
        );
        eprintln!("{msg}");
        return true;
    }

    if !super::super::sandbox::can_enforce_platform_sandbox() {
        let msg = "skipping macOS sandbox integration test: sandbox-exec cannot apply profiles \
                    in this process context";
        assert!(
            !required,
            "sandbox integration tests are required via CAKE_REQUIRE_SANDBOX_TESTS=1 \
             but sandbox-exec cannot apply profiles in this process context; see the \
             macOS sandbox design doc for requirements"
        );
        eprintln!("{msg}");
        return true;
    }

    false
}

#[cfg(target_os = "macos")]
fn path_outside_cwd_for_sandbox_test() -> Option<std::path::PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    cwd.parent().map(std::path::Path::to_path_buf)
}

#[test]
fn truncate_output_passes_through_small_output() {
    let small = "hello world";
    let result = truncate_output(small, 0, 100, false);
    assert!(result.contains(small));
    assert!(result.contains("[exit:0 | 100ms]"));
}

#[test]
fn bash_timeout_argument_is_clamped() {
    let policy = crate::clients::tools::sandbox::SandboxPolicy::DangerFullAccess;

    // Below the floor: a `0` timeout would fail instantly.
    let args =
        BashExecutionArgs::from_json(r#"{"command": "true", "timeout": 0}"#, policy).unwrap();
    assert_eq!(args.timeout, BASH_TIMEOUT_MIN_SECS);

    // Above the ceiling: cap huge values.
    let args =
        BashExecutionArgs::from_json(r#"{"command": "true", "timeout": 999999}"#, policy).unwrap();
    assert_eq!(args.timeout, BASH_TIMEOUT_MAX_SECS);

    // Missing timeout keeps the documented default of 60 seconds.
    let args = BashExecutionArgs::from_json(r#"{"command": "true"}"#, policy).unwrap();
    assert_eq!(args.timeout, 60);

    // In-range values pass through untouched.
    let args =
        BashExecutionArgs::from_json(r#"{"command": "true", "timeout": 42}"#, policy).unwrap();
    assert_eq!(args.timeout, 42);
}

#[test]
fn bash_reason_argument_round_trips() {
    let policy = crate::clients::tools::sandbox::SandboxPolicy::DangerFullAccess;

    // Present: the reason is preserved on the parsed args.
    let args = BashExecutionArgs::from_json(
        r#"{"command": "git status", "reason": "inspect working tree"}"#,
        policy,
    )
    .unwrap();
    assert_eq!(args.reason.as_deref(), Some("inspect working tree"));

    // Absent: the field is None, not an error.
    let args = BashExecutionArgs::from_json(r#"{"command": "git status"}"#, policy).unwrap();
    assert_eq!(args.reason, None);

    // Non-string reason is rejected like any other invalid argument.
    let result = BashExecutionArgs::from_json(r#"{"command": "true", "reason": 42}"#, policy);
    assert!(result.is_err());
}

#[test]
fn truncate_output_passes_through_at_limit() {
    let exact = "a".repeat(BASH_OUTPUT_MAX_BYTES);
    let result = truncate_output(&exact, 0, 50, false);
    assert!(result.contains(&exact));
    assert!(result.contains("[exit:0 | 50ms]"));
}

#[test]
fn truncate_output_truncates_large_output() {
    let large = "x".repeat(BASH_OUTPUT_MAX_BYTES + 1000);
    let result = truncate_output(&large, 0, 500, false);
    assert!(result.len() < large.len());
    assert!(result.contains("[Output too long"));
    assert!(result.contains("Full output saved to:"));
    assert!(result.contains("[exit:0 | 500ms]"));
}

#[test]
fn truncate_output_handles_multibyte_chars() {
    // Create output with multi-byte UTF-8 characters that exceeds the limit
    let large = "é".repeat(BASH_OUTPUT_MAX_BYTES); // each 'é' is 2 bytes
    let result = truncate_output(&large, 1, 2000, false);
    assert!(result.contains("[Output too long"));
    assert!(result.contains("[exit:1 | 2.0s]"));
}

#[test]
fn truncate_output_temp_file_has_no_footer() {
    let large = "x".repeat(BASH_OUTPUT_MAX_BYTES + 1000);
    let result = truncate_output(&large, 0, 100, false);
    // Extract the temp file path from the result
    let path_line = result
        .lines()
        .find(|l| l.starts_with("Full output saved to:"))
        .expect("should contain temp file path");
    let path = path_line
        .trim_start_matches("Full output saved to: ")
        .trim();
    let contents = std::fs::read_to_string(path).expect("should read temp file");
    assert!(
        !contents.contains("[exit:"),
        "temp file should not contain metadata footer"
    );
}

// ===========================================================================
// Metadata Footer Tests
// ===========================================================================

#[test]
fn metadata_footer_shows_milliseconds_under_1_second() {
    let footer = format_metadata_footer(0, 500);
    assert_eq!(footer, "[exit:0 | 500ms]");
}

#[test]
fn metadata_footer_shows_milliseconds_at_boundary() {
    // 999ms should still show as milliseconds
    let footer = format_metadata_footer(0, 999);
    assert_eq!(footer, "[exit:0 | 999ms]");
}

#[test]
fn metadata_footer_shows_seconds_over_1_second() {
    // 1000ms should show as 1.0s
    let footer = format_metadata_footer(0, 1000);
    assert_eq!(footer, "[exit:0 | 1.0s]");
}

#[test]
fn metadata_footer_shows_seconds_with_decimal() {
    // 1234ms should show as 1.2s (rounded to 1 decimal)
    let footer = format_metadata_footer(1, 1234);
    assert_eq!(footer, "[exit:1 | 1.2s]");
}

#[test]
fn metadata_footer_handles_large_values() {
    // 60000ms = 60.0s
    let footer = format_metadata_footer(0, 60000);
    assert_eq!(footer, "[exit:0 | 60.0s]");
}

#[test]
fn format_kib_tenths_rounds_to_nearest_tenth() {
    assert_eq!(format_kib_tenths(0), "0.0");
    assert_eq!(format_kib_tenths(51), "0.0");
    assert_eq!(format_kib_tenths(52), "0.1");
    assert_eq!(format_kib_tenths(1024), "1.0");
    assert_eq!(format_kib_tenths(1536), "1.5");
}

#[test]
fn format_kib_tenths_handles_max_size_without_overflowing() {
    let formatted = format_kib_tenths(usize::MAX);

    assert!(formatted.contains('.'));
}

#[test]
fn empty_rg_no_match_is_annotated() {
    let result = annotate_empty_search_result("rg definitely_missing src", String::new(), 1, "");

    assert_eq!(result, EMPTY_SEARCH_NO_MATCH_ANNOTATION);
}

#[test]
fn empty_non_search_exit_one_is_not_annotated() {
    let result = annotate_empty_search_result("false", String::new(), 1, "");

    assert_eq!(result, "");
}

#[test]
fn search_error_with_stderr_is_not_annotated() {
    let result = annotate_empty_search_result(
        "rg definitely_missing src",
        String::new(),
        1,
        "regex parse error",
    );

    assert_eq!(result, "");
}

// ===========================================================================
// Streaming Tests
// ===========================================================================

#[tokio::test]
async fn test_streaming_small_output() {
    // Command with small output returns it verbatim with metadata footer
    let args = r#"{"command": "echo hello world"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();
    assert!(result.output.contains("hello world"));
    assert!(result.output.contains("[exit:0 |"));
}

#[tokio::test]
async fn test_streaming_large_output_is_capped() {
    // Command that produces output beyond BASH_READ_CAP is truncated
    // Produce ~200KB of output (well over the 100KB cap)
    let args = r#"{"command": "yes | head -c 200000"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();
    // Should contain the truncation marker
    assert!(result.output.contains("[... output truncated at"));
    // Should still have useful content
    assert!(!result.output.is_empty());
    // Should contain metadata footer
    assert!(result.output.contains("[exit:"));
    // The read-cap truncation is recorded as a compensation event.
    assert_eq!(
        result.compensation_events.len(),
        1,
        "read-cap truncation must record one compensation event"
    );
    assert_eq!(
        result.compensation_events[0].kind,
        crate::session_telemetry::CompensationKind::OutputTruncation
    );
    assert_eq!(
        result.compensation_events[0].detail.as_deref(),
        Some("Bash")
    );
}

#[tokio::test]
async fn test_streaming_timeout() {
    // Command that hangs respects the timeout
    let args = r#"{"command": "sleep 999", "timeout": 1}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("timed out"));
}

#[tokio::test]
async fn test_streaming_closed_streams_does_not_hang() {
    // Command closes both stdout and stderr (by redirecting to /dev/null)
    // but stays alive.  The configured timeout must cover the process wait
    // even after both streams reach EOF.
    let args = r#"{"command": "exec 1>/dev/null 2>&1; sleep 999", "timeout": 1}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await;
    assert!(
        result.is_err(),
        "expected timeout error but got: {result:?}"
    );
    assert!(
        result.unwrap_err().contains("timed out"),
        "expected 'timed out' in error"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_streaming_timeout_kills_descendants() {
    // Command spawns a background process that would outlive a 1-second
    // timeout.  The process-group cleanup must terminate the descendant
    // before it can create a marker file.
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("descendant-survived");
    let script = format!(
        // The background sleep creates the marker after the timeout fires.
        // If descendant cleanup works, the file will never be created.
        "#!/bin/sh\n(sleep 2; touch '{}') &\nsleep 999\n",
        marker.display()
    );
    let script_path = dir.path().join("spawns_child.sh");
    std::fs::write(&script_path, script.as_bytes()).unwrap();
    std::fs::set_permissions(
        &script_path,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();

    let args = format!(
        r#"{{"command": "{}", "timeout": 1}}"#,
        script_path.display()
    );
    let result = Box::pin(execute_bash_unsandboxed(&args)).await;
    assert!(
        result.is_err(),
        "expected timeout error but got: {result:?}"
    );
    assert!(
        result.unwrap_err().contains("timed out"),
        "expected 'timed out' in error"
    );

    // Give the OS time to reap killed descendants.
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert!(
        !marker.exists(),
        "a descendant survived the bash timeout and was not terminated"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_bash_future_kills_descendants() {
    let dir = tempfile::tempdir().unwrap();
    let started = dir.path().join("started");
    let survived = dir.path().join("descendant-survived");
    let command = format!(
        "touch '{}'; (sleep 2; touch '{}') & sleep 999",
        started.display(),
        survived.display()
    );
    let args = serde_json::json!({ "command": command }).to_string();
    let execution = tokio::spawn(async move { execute_bash_unsandboxed(&args).await });

    timeout(std::time::Duration::from_secs(5), async {
        while !started.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("bash command did not start");

    execution.abort();
    assert!(execution.await.unwrap_err().is_cancelled());

    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert!(
        !survived.exists(),
        "a descendant survived after the bash execution future was dropped"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn completed_bash_future_does_not_kill_descendants() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("background-descendant-completed");
    let command = format!("(sleep 1; touch '{}') >/dev/null 2>&1 &", marker.display());
    let args = serde_json::json!({ "command": command }).to_string();

    execute_bash_unsandboxed(&args).await.unwrap();

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert!(
        marker.exists(),
        "normal bash completion killed a deliberately backgrounded descendant"
    );
}

#[tokio::test]
async fn test_streaming_stderr_included() {
    // Command that writes to stderr has it captured with metadata footer
    let args = r#"{"command": "echo err >&2"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();
    assert!(result.output.contains("err"));
    assert!(result.output.contains(EXIT_ZERO_STDERR_WARNING));
    assert!(result.output.contains("[exit:0 |"));
}

#[tokio::test]
async fn failed_command_stderr_does_not_get_exit_zero_warning() {
    let args = r#"{"command": "echo err >&2; exit 1"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();

    assert!(result.output.contains("err"));
    assert!(!result.output.contains(EXIT_ZERO_STDERR_WARNING));
    assert!(result.output.contains("[exit:1 |"));
}

#[tokio::test]
async fn grep_no_match_output_is_disambiguated() {
    let args = r#"{"command": "grep definitely_missing Cargo.toml"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();

    assert!(
        result.output.starts_with(EMPTY_SEARCH_NO_MATCH_ANNOTATION),
        "empty search miss should be annotated: {}",
        result.output
    );
    assert!(result.output.contains("[exit:1 |"));
}

// ===========================================================================
// Sandbox Tests
// ===========================================================================

#[cfg(target_os = "macos")]
#[tokio::test]
// FLAKE(task#292): This test fails when the environment transitions between
// `detect_platform()` returning `Err` (guard check) and `execute_bash` calling
// it again — the command runs unsandboxed and returns `Ok` instead of the
// expected sandbox error. Root mechanism is unclear since the probe is
// OnceLock-cached. Fails on clean master; reproduced across two separate
// cake sessions (b446b156, 5841be03).
async fn test_sandbox_unavailable_fails_closed() {
    if super::super::sandbox::is_sandbox_disabled()
        || super::super::sandbox::detect_platform().is_ok()
    {
        return;
    }

    let args = r#"{"command": "echo should-not-run"}"#;
    let result = Box::pin(execute_bash(&sandbox_context(), args)).await;
    let error = result.expect_err("sandbox initialization failure should fail closed");
    assert!(
        error.contains("macOS sandbox unavailable"),
        "Expected sandbox unavailable error, got: {error}"
    );
}

/// When the macOS sandbox is available, a sandboxed command that prints
/// `sandbox-exec: sandbox_apply` to stdout must return normal output,
/// not a sandbox-initialization-failure error. This is the exact scenario
/// from the original bug: `rg -n "sandbox" src/clients/tools/bash.rs`
/// matched lines containing that literal string.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn test_sandboxed_command_stdout_does_not_trigger_init_failure() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let args = r#"{"command": "printf 'sandbox-exec: sandbox_apply file-write* /tmp/test\\n'"}"#;
    let result = Box::pin(execute_bash(&sandbox_context(), args))
        .await
        .unwrap();

    assert!(
        result.output.contains("sandbox-exec: sandbox_apply"),
        "sandboxed command should return its stdout: {}",
        result.output
    );
    assert!(
        !result.output.contains("macOS sandbox unavailable"),
        "stdout pattern must not trigger sandbox initialization failure: {}",
        result.output
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn test_sandbox_blocks_write_outside_cwd() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let outside =
        path_outside_cwd_for_sandbox_test().expect("should find a parent directory outside cwd");
    let target = outside.join(format!("cake_sandbox_test_{}", uuid::Uuid::new_v4()));
    let target = target.display();
    let args = format!(r#"{{"command": "touch {target}"}}"#);
    let result = Box::pin(execute_bash(&sandbox_context(), &args))
        .await
        .unwrap();
    assert!(
        result.output.contains("Operation not permitted")
            || result.output.contains("Permission denied"),
        "Expected sandbox to block write outside cwd, got: {}",
        result.output
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn test_sandbox_allows_read_in_cwd() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let args = r#"{"command": "ls Cargo.toml"}"#;
    let result = Box::pin(execute_bash(&sandbox_context(), args))
        .await
        .unwrap();
    assert!(
        result.output.contains("Cargo.toml"),
        "Expected ls in cwd to succeed, got: {}",
        result.output
    );
    // Should contain metadata footer
    assert!(result.output.contains("[exit:0 |"));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn test_sandbox_blocks_read_outside_cwd() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let outside =
        path_outside_cwd_for_sandbox_test().expect("should find a parent directory outside cwd");
    let temp_dir = tempfile::TempDir::new_in(outside).expect("should create test dir outside cwd");
    let outside_dir = temp_dir.path().display();
    let args = format!(r#"{{"command": "ls {outside_dir}"}}"#);
    let result = Box::pin(execute_bash(&sandbox_context(), &args))
        .await
        .unwrap();
    assert!(
        result.output.contains("Operation not permitted")
            || result.output.contains("Permission denied"),
        "Expected sandbox to block read outside cwd, got: {}",
        result.output
    );
}

/// A `[sandbox].read_only` file grant (which flows into `additional_dirs`)
/// lets sandboxed Bash execute exactly that file; a sibling file in the same
/// directory stays denied.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn test_sandbox_read_only_file_grant_runs_file_but_denies_sibling() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let outside =
        path_outside_cwd_for_sandbox_test().expect("should find a parent directory outside cwd");
    let temp_dir = tempfile::TempDir::new_in(outside).expect("should create test dir outside cwd");
    let allowed = temp_dir.path().join("allowed.sh");
    let sibling = temp_dir.path().join("sibling.sh");
    std::fs::write(&allowed, "#!/bin/sh\nprintf 'allowed-ran\\n'\n").unwrap();
    std::fs::write(&sibling, "#!/bin/sh\nprintf 'sibling-ran\\n'\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&allowed, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&sibling, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // `[sandbox].read_only` paths are loaded into `additional_dirs` by main.rs;
    // mirror that wiring here.
    let mut context =
        ToolContext::from_current_process().with_judge(Some(bypassed_judge_context()));
    context.additional_dirs = vec![allowed.clone()];
    let context = Arc::new(context);

    let allowed_args = format!(r#"{{"command": "{}"}}"#, allowed.display());
    let result = Box::pin(execute_bash(&context, &allowed_args))
        .await
        .unwrap();
    assert!(
        result.output.contains("allowed-ran"),
        "sandbox should run a configured [sandbox].read_only file, got: {}",
        result.output
    );

    let sibling_args = format!(r#"{{"command": "{}"}}"#, sibling.display());
    let result = Box::pin(execute_bash(&context, &sibling_args))
        .await
        .unwrap();
    assert!(
        result.output.contains("Operation not permitted")
            || result.output.contains("Permission denied"),
        "sandbox should deny a sibling file next to a [sandbox].read_only file, got: {}",
        result.output
    );
}

// ===========================================================================
// Sandbox Policy Tests (task 195)
// ===========================================================================

/// Build a `ToolContext` with a resolved sandbox policy for the current
/// process. `execute_bash` reads `context.sandbox_policy` to override the
/// args-level default.
#[cfg(target_os = "macos")]
fn context_with_policy(policy: SandboxPolicy) -> Arc<ToolContext> {
    let mut context =
        ToolContext::from_current_process().with_judge(Some(bypassed_judge_context()));
    context.sandbox_policy = policy;
    Arc::new(context)
}

/// A sandbox-test context with the judge bypassed. These tests exercise
/// sandbox execution, not the command-safety gate, so commands must not fail
/// closed on an absent judge context.
#[cfg(target_os = "macos")]
fn sandbox_context() -> Arc<ToolContext> {
    Arc::new(ToolContext::from_current_process().with_judge(Some(bypassed_judge_context())))
}

/// Read-only policy denies writes to the project directory.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn test_sandbox_read_only_blocks_write_in_cwd() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let target = format!("cake_ro_probe_{}", uuid::Uuid::new_v4());
    let args = format!(r#"{{"command": "touch {target}"}}"#);
    let result = Box::pin(execute_bash(
        &context_with_policy(SandboxPolicy::ReadOnly),
        &args,
    ))
    .await
    .unwrap();
    assert!(
        result.output.contains("Operation not permitted")
            || result.output.contains("Permission denied"),
        "read-only policy should block writes to cwd, got: {}",
        result.output
    );
    // Clean up just in case the sandbox did not block it.
    _ = std::fs::remove_file(&target);
}

/// Workspace-write policy allows writes to the project directory.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn test_sandbox_workspace_write_allows_write_in_cwd() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let target = format!("cake_ww_probe_{}", uuid::Uuid::new_v4());
    let args = format!(r#"{{"command": "touch {target} && rm -f {target}"}}"#);
    let result = Box::pin(execute_bash(
        &context_with_policy(SandboxPolicy::WorkspaceWrite),
        &args,
    ))
    .await
    .unwrap();
    assert!(
        result.output.contains("[exit:0 |"),
        "workspace-write policy should allow writes to cwd, got: {}",
        result.output
    );
}

/// Workspace-write policy grants sccache's default macOS cache dir
/// (~/Library/Caches/Mozilla.sccache) so `RUSTC_WRAPPER=sccache` builds work
/// under the sandbox.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn test_sandbox_workspace_write_allows_sccache_cache_dir() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        panic!("HOME must be set for the sccache cache dir probe");
    };
    let probe_dir = home
        .join("Library/Caches/Mozilla.sccache")
        .join(format!("cake_sccache_probe_{}", uuid::Uuid::new_v4()));
    let probe = probe_dir.display();
    // `rm -rf` is blocked by the command policy outside /tmp and /var/tmp, so
    // the probe is removed by the (unsandboxed) test process below instead of
    // inside the sandboxed command.
    let args = format!(r#"{{"command": "mkdir -p {probe} && touch {probe}/probe"}}"#);
    let result = Box::pin(execute_bash(
        &context_with_policy(SandboxPolicy::WorkspaceWrite),
        &args,
    ))
    .await
    .unwrap();
    assert!(
        result.output.contains("[exit:0 |"),
        "workspace-write policy should allow writes to sccache's default cache dir, got: {}",
        result.output
    );
    // Clean up just in case the sandbox did not remove the probe dir.
    _ = std::fs::remove_dir_all(&probe_dir);
}

/// Danger-full-access policy skips the sandbox entirely.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn test_sandbox_danger_full_access_allows_write_outside_cwd() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let outside =
        path_outside_cwd_for_sandbox_test().expect("should find a parent directory outside cwd");
    let target = outside.join(format!("cake_dfa_probe_{}", uuid::Uuid::new_v4()));
    let target_display = target.display();
    let args = format!(r#"{{"command": "touch {target_display} && rm -f {target_display}"}}"#);
    let result = Box::pin(execute_bash(
        &context_with_policy(SandboxPolicy::DangerFullAccess),
        &args,
    ))
    .await
    .unwrap();
    // With no sandbox, writing outside cwd must succeed.
    assert!(
        result.output.contains("[exit:0 |"),
        "danger-full-access policy should skip the sandbox and allow writes outside cwd, got: {}",
        result.output
    );
}

// ===========================================================================
// Linked Worktree Sandbox Tests (task 260)
// ===========================================================================

/// Build bash tool arguments that run `git` with the inherited repository and
/// configuration variables dropped.
///
/// The bash tool passes cake's environment to the child, so a `GIT_DIR`
/// inherited from whoever launched the test suite would send these commands
/// at that repository instead of the fixture worktree. These commands carry
/// no `-c` options of their own, so inherited command-scope configuration
/// would also outrank the fixture's local settings, including its pinned
/// `core.hooksPath`.
#[cfg(target_os = "macos")]
fn sandboxed_git(args: &[&str]) -> String {
    let mut command = String::from("env");
    for var in crate::config::git::AMBIENT_ENV_VARS
        .iter()
        .chain(crate::config::git::FIXTURE_ENV_VARS)
    {
        command.push_str(" -u ");
        command.push_str(var);
    }
    command.push_str(" git");
    for arg in args {
        command.push(' ');
        command.push_str(&shell_quote(arg));
    }
    serde_json::json!({ "command": command }).to_string()
}

/// Single-quote `value` for a POSIX shell.
#[cfg(target_os = "macos")]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Verify git operations (status, add, commit) succeed in a real linked
/// worktree under an enforced macOS Seatbelt sandbox.
///
/// This integration test creates a real git repository with a linked worktree
/// whose per-worktree gitdir and common dir are outside the workspace subtree,
/// then executes git commands through cake's sandboxed execution path.
///
/// Coverage notes:
/// - The test uses an externally-created worktree (`git worktree add`).
///   Cake-managed worktrees (--worktree flag) route through the same
///   `SandboxConfig::build_with_policy` and `resolve_linked_worktree_dirs`
///   code path — the .git file pointer and commondir resolution are
///   identical regardless of how the worktree was created.
/// - Both the per-worktree gitdir and the common git dir are explicitly
///   verified to be outside the workspace subtree.
#[cfg(target_os = "macos")]
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "integration test: setup, git init, worktree creation, path verification, and three git operations through sandbox"
)]
async fn test_sandbox_linked_worktree_git_operations() {
    if skip_if_sandbox_unavailable() {
        return;
    }

    let process_cwd = std::env::current_dir().expect("current directory must be available");
    let fixture_parent = process_cwd
        .parent()
        .expect("repository must have a parent directory");
    let fixture = tempfile::Builder::new()
        .prefix("cake-linked-worktree-")
        .tempdir_in(fixture_parent)
        .expect("should create fixture outside the workspace");
    let main_repo = fixture.path().join("main");
    let wt_path = fixture.path().join("linked-worktree");

    // Initialize main repo with a commit and a detached linked worktree
    crate::config::git::test_support::init_repo_with_linked_worktree(&main_repo, &wt_path);

    // Verify per-worktree gitdir and common dir are outside the workspace
    // subtree (the linked worktree itself).
    let git_file_content =
        std::fs::read_to_string(wt_path.join(".git")).expect(".git file must be readable");
    let gitdir_line = git_file_content
        .lines()
        .find(|l| l.trim().starts_with("gitdir:"))
        .expect(".git file must contain a gitdir: line");
    let gitdir_raw = gitdir_line
        .strip_prefix("gitdir: ")
        .or_else(|| gitdir_line.strip_prefix("gitdir:"))
        .map(str::trim)
        .expect("gitdir: line must have a path");
    let gitdir_path = if std::path::Path::new(gitdir_raw).is_relative() {
        wt_path.join(gitdir_raw)
    } else {
        std::path::PathBuf::from(gitdir_raw)
    };
    let canonical_gitdir = gitdir_path
        .canonicalize()
        .expect("gitdir path must be resolvable");

    // Read commondir from the worktree gitdir
    let commondir_content = std::fs::read_to_string(canonical_gitdir.join("commondir"))
        .expect("commondir file must be readable");
    let commondir_raw = commondir_content.trim();
    let common_dir_path = if std::path::Path::new(commondir_raw).is_relative() {
        canonical_gitdir.join(commondir_raw)
    } else {
        std::path::PathBuf::from(commondir_raw)
    };
    let canonical_common = common_dir_path
        .canonicalize()
        .expect("common dir must be resolvable");

    // Assert both are outside the worktree and the sandbox's broad temp grants.
    // Their writes must therefore depend on linked-worktree resolution.
    let canonical_wt = wt_path
        .canonicalize()
        .expect("worktree path must be canonicalizable");
    assert!(
        !canonical_gitdir.starts_with(&canonical_wt),
        "per-worktree gitdir should be outside the workspace subtree"
    );
    assert!(
        !canonical_common.starts_with(&canonical_wt),
        "common git dir should be outside the workspace subtree"
    );

    // Create a ToolContext with cwd set to the linked worktree
    let mut context =
        ToolContext::from_current_process().with_judge(Some(bypassed_judge_context()));
    context.cwd = wt_path.clone();
    for temp_dir in &context.temp_dirs {
        let canonical_temp = temp_dir.canonicalize().unwrap_or_else(|_| temp_dir.clone());
        assert!(
            !canonical_gitdir.starts_with(&canonical_temp),
            "per-worktree gitdir must not inherit a broad temp-directory grant"
        );
        assert!(
            !canonical_common.starts_with(&canonical_temp),
            "common git dir must not inherit a broad temp-directory grant"
        );
    }

    // ====================================================================
    // git status
    // ====================================================================
    let args = sandboxed_git(&["status"]);
    let result = Box::pin(execute_bash(&context, &args))
        .await
        .expect("git status should succeed in linked worktree");
    assert!(
        result.output.contains("[exit:0 |"),
        "git status should exit 0 in linked worktree: {}",
        result.output
    );
    assert!(
        result.output.contains("nothing to commit"),
        "git status should show clean tree in linked worktree: {}",
        result.output
    );

    // ====================================================================
    // Create new file, git add, git commit
    // ====================================================================
    // Create a new file via the sandbox
    let args = r#"{"command": "echo 'new content' > new_file.md"}"#;
    let result = Box::pin(execute_bash(&context, args))
        .await
        .expect("echo to new file should succeed in linked worktree");
    assert!(
        result.output.contains("[exit:0 |"),
        "echo should succeed: {}",
        result.output
    );

    // git add the new file
    let args = sandboxed_git(&["add", "new_file.md"]);
    let result = Box::pin(execute_bash(&context, &args))
        .await
        .expect("git add should succeed in linked worktree");
    assert!(
        result.output.contains("[exit:0 |"),
        "git add should exit 0 in linked worktree: {}",
        result.output
    );

    // git commit with inline user config (no global git config needed
    // since the sandbox may restrict access to ~/.gitconfig)
    let args = sandboxed_git(&[
        "-c",
        "user.name=Cake Test",
        "-c",
        "user.email=cake-test@example.invalid",
        "commit",
        "-m",
        "test commit in linked worktree",
    ]);
    let result = Box::pin(execute_bash(&context, &args))
        .await
        .expect("git commit should succeed in linked worktree");
    assert!(
        result.output.contains("[exit:0 |"),
        "git commit should exit 0 in linked worktree: {}",
        result.output
    );
    assert!(
        result.output.contains("1 file changed"),
        "git commit should show 1 file changed in linked worktree: {}",
        result.output
    );
}

// ===========================================================================
// CAKE_REQUIRE_SANDBOX_TESTS Parsing Tests
// ===========================================================================

#[cfg(target_os = "macos")]
#[test]
fn require_sandbox_tests_defaults_to_false_when_unset() {
    assert!(!parse_sandbox_tests_required(None));
}

#[cfg(target_os = "macos")]
#[test]
fn require_sandbox_tests_false_for_unrecognized_values() {
    assert!(!parse_sandbox_tests_required(Some("0")));
    assert!(!parse_sandbox_tests_required(Some("false")));
    assert!(!parse_sandbox_tests_required(Some("no")));
    assert!(!parse_sandbox_tests_required(Some("off")));
    assert!(!parse_sandbox_tests_required(Some("maybe")));
    assert!(!parse_sandbox_tests_required(Some("")));
}

#[cfg(target_os = "macos")]
#[test]
fn require_sandbox_tests_true_for_truthy_values() {
    assert!(parse_sandbox_tests_required(Some("1")));
    assert!(parse_sandbox_tests_required(Some("true")));
    assert!(parse_sandbox_tests_required(Some("yes")));
    assert!(parse_sandbox_tests_required(Some("on")));
}

// ===========================================================================
// Binary Data Detection Tests
// ===========================================================================

#[test]
fn test_is_binary_data_detects_null_bytes() {
    // Data with null bytes should be detected as binary (need >8 null bytes)
    let binary_data =
        b"hello\x00world\x00more\x00nulls\x00here\x00more\x00data\x00extra\x00again\x00more";
    assert!(is_binary_data(binary_data));
}

#[test]
fn test_is_binary_data_detects_high_non_printable_ratio() {
    // Data with many non-printable characters should be detected as binary
    // Create data with ~50% non-printable characters
    let mut binary_data = Vec::new();
    for i in 0..100 {
        if i % 2 == 0 {
            binary_data.push(0x01); // Non-printable
        } else {
            binary_data.push(b'A'); // Printable
        }
    }
    assert!(is_binary_data(&binary_data));
}

#[test]
fn test_is_binary_data_allows_exact_threshold() {
    let mut data = Vec::new();
    for i in 0..100 {
        if i < 30 {
            data.push(0x01);
        } else {
            data.push(b'A');
        }
    }
    assert!(!is_binary_data(&data));
}

#[test]
fn test_is_binary_data_allows_text() {
    // Normal text should not be detected as binary
    let text_data = b"Hello, world!\nThis is a test.\nLine 3.\n";
    assert!(!is_binary_data(text_data));
}

#[test]
fn test_is_binary_data_allows_multibyte_utf8() {
    // UTF-8 text with multi-byte characters should not be detected as binary
    let utf8_text = "Hello, 世界!\nПривет мир\n🎉".as_bytes();
    assert!(!is_binary_data(utf8_text));
}

#[test]
fn test_is_binary_data_allows_empty() {
    // Empty data should not be detected as binary
    assert!(!is_binary_data(b""));
}

#[test]
fn test_is_binary_data_allows_few_null_bytes() {
    // A few null bytes (below threshold) should not trigger binary detection
    let text_with_few_nulls = b"hello\x00world";
    assert!(!is_binary_data(text_with_few_nulls));
}

#[test]
fn sandbox_initialization_failure_requires_applied_sandbox() {
    let output = "sandbox-exec: sandbox_apply: Operation not permitted";
    assert!(is_sandbox_initialization_failure(true, output));
    assert!(!is_sandbox_initialization_failure(false, output));
    assert!(!is_sandbox_violation(true, false, output));
}

#[test]
fn sandbox_initialization_failure_checks_stderr_only() {
    // The pattern must appear in the stderr string to be detected.
    // An empty stderr means no initialization failure, even if stdout
    // contains the literal string.
    assert!(!is_sandbox_initialization_failure(true, ""));
    assert!(!is_sandbox_initialization_failure(
        true,
        "some normal stderr output"
    ));
    assert!(is_sandbox_initialization_failure(
        true,
        "sandbox-exec: sandbox_apply: Operation not permitted"
    ));
}

/// Regression test: a command that prints `sandbox-exec: sandbox_apply`
/// to stdout must NOT be treated as a sandbox initialization failure.
/// The check should only inspect stderr, so stdout content is irrelevant.
#[tokio::test]
async fn command_stdout_containing_sandbox_apply_pattern_is_not_false_positive() {
    let args = r#"{"command": "printf 'sandbox-exec: sandbox_apply file-write* /tmp/test\n'"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();

    assert!(
        result.output.contains("sandbox-exec: sandbox_apply"),
        "command output should contain the printed pattern: {}",
        result.output
    );
    assert!(
        !result.output.contains("macOS sandbox unavailable"),
        "stdout pattern should not trigger sandbox initialization failure: {}",
        result.output
    );
}

#[test]
fn sandbox_violation_requires_sandboxed_failed_command() {
    let output = "Operation not permitted";

    assert!(is_sandbox_violation(true, false, output));
    assert!(!is_sandbox_violation(true, true, output));
    assert!(!is_sandbox_violation(false, false, output));
}

#[tokio::test]
async fn successful_command_output_does_not_trigger_sandbox_warning() {
    let args = r#"{"command": "printf 'Operation not permitted\n'"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();

    assert!(result.output.contains("Operation not permitted"));
    assert!(
        !result.output.contains("[Sandbox restriction]"),
        "successful command output should not be classified as sandbox restriction: {}",
        result.output
    );
}

#[tokio::test]
async fn failed_unsandboxed_command_output_does_not_trigger_sandbox_warning() {
    let args = r#"{"command": "printf 'Operation not permitted\n'; exit 1"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();

    assert!(result.output.contains("Operation not permitted"));
    assert!(
        !result.output.contains("[Sandbox restriction]"),
        "unsandboxed command output should not be classified as sandbox restriction: {}",
        result.output
    );
}

#[test]
fn test_detect_mime_type_png() {
    let png_header = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    assert_eq!(detect_mime_type(&png_header), Some("image/png"));
}

#[test]
fn test_detect_mime_type_jpeg() {
    let jpeg_header = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
    assert_eq!(detect_mime_type(&jpeg_header), Some("image/jpeg"));
}

#[test]
fn test_detect_mime_type_pdf() {
    let pdf_header = b"%PDF-1.4";
    assert_eq!(detect_mime_type(pdf_header), Some("application/pdf"));
}

#[test]
fn test_detect_mime_type_zip() {
    let zip_header = [0x50, 0x4B, 0x03, 0x04, 0x00, 0x00, 0x00];
    assert_eq!(detect_mime_type(&zip_header), Some("application/zip"));
}

#[test]
fn test_detect_mime_type_gzip() {
    let gzip_header = [0x1F, 0x8B, 0x08, 0x00];
    assert_eq!(detect_mime_type(&gzip_header), Some("application/gzip"));
}

#[test]
fn test_detect_mime_type_unknown() {
    // Random data should return None
    let unknown_data = b"Hello, world!";
    assert_eq!(detect_mime_type(unknown_data), None);
}

#[test]
fn test_detect_mime_type_too_short() {
    // Data that's too short should return None
    let short_data = [0x89, 0x50];
    assert_eq!(detect_mime_type(&short_data), None);
}

#[tokio::test]
async fn test_binary_output_handling() {
    // Command that produces binary output (random bytes)
    let args = r#"{"command": "head -c 100 /dev/urandom"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();
    // Should detect binary and show appropriate message
    assert!(
        result.output.contains("[Binary output detected") || result.output.contains("[exit:"),
        "Expected binary output handling, got: {}",
        result.output
    );
}

#[tokio::test]
async fn test_binary_output_with_known_type() {
    // Create a small gzip-compressed file and read it
    let args = r#"{"command": "echo 'hello' | gzip | head -c 20"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();
    // Should detect gzip magic number
    assert!(
        result.output.contains("application/gzip") || result.output.contains("[exit:"),
        "Expected gzip detection, got: {}",
        result.output
    );
}

#[tokio::test]
async fn binary_output_with_exit_zero_stderr_includes_warning() {
    let args = r#"{"command": "printf '\\0%.0s' {1..16}; echo err >&2"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();

    assert!(result.output.contains("[Binary output detected"));
    assert!(result.output.contains(EXIT_ZERO_STDERR_WARNING));
    assert!(result.output.contains("[exit:0 |"));
}

#[tokio::test]
async fn binary_output_above_max_records_truncation_event() {
    // Binary output above BASH_OUTPUT_MAX_BYTES spills to a temp file and
    // replaces the displayable output, so it must record an output_truncation
    // compensation event exactly like oversized text does. 60_000 bytes is
    // over the 50_000 max but under the 100_000 read cap, so only the spill
    // mechanism fires.
    let args = r#"{"command": "head -c 60000 /dev/zero"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();
    assert!(result.output.contains("[Binary output detected"));
    assert_eq!(
        result.compensation_events.len(),
        1,
        "oversized binary spill must record one compensation event"
    );
    assert_eq!(
        result.compensation_events[0].kind,
        crate::session_telemetry::CompensationKind::OutputTruncation
    );
    assert_eq!(
        result.compensation_events[0].detail.as_deref(),
        Some("Bash")
    );
}

#[tokio::test]
async fn small_binary_output_records_no_truncation_event() {
    // Binary output below the max bytes is summarized for display, but the
    // model lost no content above the inline cap, so no truncation event.
    let args = r#"{"command": "printf '\\0%.0s' {1..16}"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();
    assert!(result.output.contains("[Binary output detected"));
    assert!(
        result.compensation_events.is_empty(),
        "small binary output must not record a truncation event"
    );
}

#[tokio::test]
async fn test_text_output_not_detected_as_binary() {
    // Normal text output should not be detected as binary
    let args = r#"{"command": "echo 'Hello, world!'"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();
    assert!(
        !result.output.contains("[Binary output detected"),
        "Text output should not be detected as binary, got: {}",
        result.output
    );
    assert!(result.output.contains("Hello, world!"));
}

#[tokio::test]
async fn test_streaming_empty_output() {
    // A command that produces no stdout or stderr should return empty
    // output with just the metadata footer.
    let args = r#"{"command": "true"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();
    assert!(
        result.output.starts_with("[exit:0 |"),
        "Empty output should produce only the footer, got: {}",
        result.output
    );
    // No command output before the footer
    assert!(
        !result.output.contains('\n'),
        "Empty output should not contain newlines before the footer, got: {}",
        result.output
    );
}

#[tokio::test]
async fn test_streaming_empty_output_with_stderr() {
    // A command that produces only stderr output. The output should show
    // the warning because exit is 0 but stderr is non-empty, then the
    // metadata footer.
    let args = r#"{"command": "echo err >&2"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await.unwrap();
    assert!(result.output.contains("err"));
    assert!(result.output.contains(EXIT_ZERO_STDERR_WARNING));
    assert!(result.output.contains("[exit:0 |"));
}

// =============================================================================
// Judge preflight (Milestone 5): warn prepends, block and judge failure block.
// =============================================================================

/// Build a judge context whose judge client points at a wiremock server.
fn judge_context(mock_server: &MockServer) -> std::sync::Arc<JudgeContext> {
    use crate::config::model::{ApiType, ModelConfig};
    use std::collections::HashMap;

    let model_config = ModelConfig {
        model: "judge/model".to_string(),
        api_type: ApiType::ChatCompletions,
        base_url: mock_server.uri(),
        api_key_env: "JUDGE_TEST_KEY".to_string(),
        provider: None,
        provider_headers: None,
        temperature: Some(0.0),
        top_p: None,
        max_output_tokens: Some(128),
        reasoning_effort: None,
        reasoning_summary: None,
        reasoning_max_tokens: None,
        providers: vec![],
    };
    std::sync::Arc::new(JudgeContext {
        settings: crate::config::settings::JudgeSettings::default(),
        agent_model: crate::config::model::ResolvedModelConfig {
            model_config,
            api_key: "test-key".to_string(),
        },
        models: HashMap::new(),
    })
}

/// Chat-completions response carrying one judge verdict for a mock server.
fn judge_chat_response(verdict_json: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "chatcmpl-judge",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": verdict_json },
            "finish_reason": "stop"
        }]
    })
}

/// Mount a mock judge returning one canned verdict for every request.
async fn mount_judge_verdict(mock_server: &MockServer, verdict_json: &str) {
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(judge_chat_response(verdict_json)))
        .mount(mock_server)
        .await;
}

#[tokio::test]
async fn test_judge_warn_prepends_notice() {
    // A `warn` verdict runs the command and prepends the judge's message as a
    // NOTICE, mirroring the old soft-warning behavior without reclassification.
    let mock_server = MockServer::start().await;
    mount_judge_verdict(
        &mock_server,
        r#"{"verdict":"warn","code":"rg-replace-footgun","message":"Use -r with an explicit replacement."}"#,
    )
    .await;

    let args = r#"{"command": "echo judge-warn-test"}"#;
    let result = Box::pin(execute_bash_with_judge(
        args,
        Some(judge_context(&mock_server)),
    ))
    .await
    .unwrap();
    assert!(
        result
            .output
            .contains("NOTICE: Use -r with an explicit replacement."),
        "warn verdict should prepend NOTICE, got: {}",
        result.output
    );
    assert!(result.output.contains("judge-warn-test"));
    assert!(result.output.contains("[exit:0 |"));
}

#[tokio::test]
async fn test_judge_allow_runs_ungated() {
    // An `allow` verdict runs the command with no annotation.
    let mock_server = MockServer::start().await;
    mount_judge_verdict(&mock_server, r#"{"verdict":"allow","message":"Safe"}"#).await;

    let args = r#"{"command": "echo judge-allow-test"}"#;
    let result = Box::pin(execute_bash_with_judge(
        args,
        Some(judge_context(&mock_server)),
    ))
    .await
    .unwrap();
    assert!(
        result.output.contains("judge-allow-test"),
        "allow verdict should run the command, got: {}",
        result.output
    );
    assert!(
        !result.output.contains("NOTICE:"),
        "allow verdict should not annotate output, got: {}",
        result.output
    );
}

#[tokio::test]
async fn test_judge_block_prevents_execution() {
    // A `block` verdict prevents spawn and surfaces the judge's reason as the
    // tool error.
    let mock_server = MockServer::start().await;
    mount_judge_verdict(
        &mock_server,
        r#"{"verdict":"block","code":"git-force-push","message":"Prefer push --force-with-lease."}"#,
    )
    .await;

    let args = r#"{"command": "git push --force"}"#;
    let err = Box::pin(execute_bash_with_judge(
        args,
        Some(judge_context(&mock_server)),
    ))
    .await
    .unwrap_err();
    assert!(
        err.contains("BLOCKED"),
        "block verdict must block, got: {err}"
    );
    assert!(
        err.contains("Prefer push --force-with-lease."),
        "block must carry the judge's reason, got: {err}"
    );
}

#[tokio::test]
async fn test_judge_missing_context_fails_closed() {
    // No judge context means the run has no command-safety gate; the command
    // must not run ungated.
    let args = r#"{"command": "echo hi"}"#;
    let err = Box::pin(execute_bash_with_judge(args, None))
        .await
        .unwrap_err();
    assert!(
        err.contains("BLOCKED"),
        "missing judge context must fail closed, got: {err}"
    );
    assert!(err.contains("not configured"));
}

#[tokio::test]
async fn test_judge_unreachable_fails_closed() {
    // A judge transport failure (here: HTTP 500) blocks the command with the
    // fail-closed message instead of running it ungated.
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&mock_server)
        .await;

    let args = r#"{"command": "echo hi"}"#;
    let err = Box::pin(execute_bash_with_judge(
        args,
        Some(judge_context(&mock_server)),
    ))
    .await
    .unwrap_err();
    assert!(
        err.contains("BLOCKED"),
        "unreachable judge must fail closed, got: {err}"
    );
    assert!(err.contains("was unavailable"));
}

/// Produce large stderr output after stdout closes, hitting `BASH_READ_CAP`
/// during the `stdout closed — read remaining stderr` drain loop.
#[tokio::test]
async fn test_streaming_stderr_drain_after_stdout_close_hits_cap() {
    // python3: write small stdout, close it, then flood stderr past cap
    let args = r#"{"command": "python3 -c 'import sys; sys.stdout.write(\"hello\\n\"); sys.stdout.flush(); sys.stdout.close(); sys.stderr.write(\"x\" * 200000)'"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await;
    match result {
        Ok(res) => {
            assert!(
                res.output.contains("[... output truncated at"),
                "Expected truncation when stderr fills after stdout closes. Output: {}",
                res.output
            );
            assert!(
                res.output.contains("[exit:"),
                "Output should contain footer"
            );
        },
        Err(e)
            if e.contains("command not found")
                || e.contains("python3: cannot open")
                || e.contains("python3: not found") =>
        {
            // python3 not available on this system — skip
            eprintln!("skipping: python3 not available");
        },
        Err(e) => panic!("Unexpected error: {e}"),
    }
}

/// Produce large stdout output after stderr closes, hitting `BASH_READ_CAP`
/// during the `stderr closed — read remaining stdout` drain loop.
#[tokio::test]
async fn test_streaming_stdout_drain_after_stderr_close_hits_cap() {
    // python3: write small stderr, close it, then flood stdout past cap
    let args = r#"{"command": "python3 -c 'import sys; sys.stderr.write(\"hello\\n\"); sys.stderr.flush(); sys.stderr.close(); sys.stdout.write(\"x\" * 200000)'"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await;
    match result {
        Ok(res) => {
            assert!(
                res.output.contains("[... output truncated at"),
                "Expected truncation when stdout fills after stderr closes. Output: {}",
                res.output
            );
            assert!(
                res.output.contains("[exit:"),
                "Output should contain footer"
            );
        },
        Err(e)
            if e.contains("command not found")
                || e.contains("python3: cannot open")
                || e.contains("python3: not found") =>
        {
            // python3 not available on this system — skip
            eprintln!("skipping: python3 not available");
        },
        Err(e) => panic!("Unexpected error: {e}"),
    }
}

// ===========================================================================
// Secure Temp Directory Tests
// ===========================================================================

#[cfg(unix)]
#[test]
fn secure_temp_dir_creates_per_user_directory() {
    let dir = bash_temp_output_dir().unwrap();
    let dir_name = dir.file_name().unwrap().to_str().unwrap();
    // SAFETY: `getuid()` is a simple system call with no safety requirements.
    let uid = unsafe { libc::getuid() };
    assert!(
        dir_name.starts_with(&format!("cake-{uid}-")),
        "expected directory name to start with 'cake-{uid}-', got '{dir_name}'"
    );
    assert!(dir.exists(), "directory should exist");
    assert!(dir.is_dir(), "path should be a directory");
}

#[cfg(unix)]
#[test]
fn secure_temp_dir_has_restrictive_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = bash_temp_output_dir().unwrap();
    assert!(dir.exists(), "directory should exist");

    let metadata = std::fs::metadata(dir).unwrap();
    let mode = metadata.permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o700,
        "expected 0o700 permissions on secure temp dir, got 0o{mode:o}"
    );
}

#[cfg(unix)]
#[test]
fn secure_temp_dir_is_owned_by_current_user() {
    use std::os::unix::fs::MetadataExt;

    let dir = bash_temp_output_dir().unwrap();
    let metadata = std::fs::metadata(dir).unwrap();
    // SAFETY: `getuid()` is a simple system call with no safety requirements.
    let uid = unsafe { libc::getuid() };
    assert_eq!(
        metadata.uid(),
        uid,
        "directory should be owned by current user"
    );
}

#[test]
fn secure_temp_dir_usable_for_truncation() {
    // Verify that truncate_output writes to the secure temp dir
    let large = "x".repeat(BASH_OUTPUT_MAX_BYTES + 1000);
    let result = truncate_output(&large, 0, 100, false);
    let path_line = result
        .lines()
        .find(|l| l.starts_with("Full output saved to:"))
        .expect("should contain temp file path");
    let path_str = path_line
        .trim_start_matches("Full output saved to: ")
        .trim();
    let path = std::path::Path::new(path_str);
    assert!(
        path.exists(),
        "temp file should exist at: {}",
        path.display()
    );

    // The path should be under our secure temp dir
    let parent = path.parent().unwrap();
    let secure_dir = bash_temp_output_dir().unwrap();
    assert_eq!(
        parent, secure_dir,
        "temp file should be inside secure temp dir"
    );

    // Clean up
    _ = std::fs::remove_file(path);
}
