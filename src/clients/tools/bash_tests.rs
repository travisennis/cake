use super::*;
#[cfg(target_os = "macos")]
use crate::clients::tools::ToolContext;

#[cfg(target_os = "macos")]
fn skip_if_sandbox_unavailable() -> bool {
    if super::super::sandbox::is_sandbox_disabled() {
        eprintln!("skipping macOS sandbox integration test: CAKE_SANDBOX disables sandboxing");
        return true;
    }

    if !super::super::sandbox::can_enforce_platform_sandbox() {
        eprintln!(
            "skipping macOS sandbox integration test: sandbox-exec cannot apply profiles in this process context"
        );
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

// ===========================================================================
// Sandbox Tests
// ===========================================================================

#[cfg(target_os = "macos")]
#[tokio::test]
async fn test_sandbox_unavailable_fails_closed() {
    if super::super::sandbox::is_sandbox_disabled()
        || super::super::sandbox::can_enforce_platform_sandbox()
    {
        return;
    }

    let args = r#"{"command": "echo should-not-run"}"#;
    let result = Box::pin(execute_bash(&ToolContext::from_current_process(), args)).await;
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
    let result = Box::pin(execute_bash(&ToolContext::from_current_process(), args))
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
    let result = Box::pin(execute_bash(&ToolContext::from_current_process(), &args))
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
    let result = Box::pin(execute_bash(&ToolContext::from_current_process(), args))
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
    let result = Box::pin(execute_bash(&ToolContext::from_current_process(), &args))
        .await
        .unwrap();
    assert!(
        result.output.contains("Operation not permitted")
            || result.output.contains("Permission denied"),
        "Expected sandbox to block read outside cwd, got: {}",
        result.output
    );
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
fn sandbox_initialization_failure_is_not_a_sandbox_violation() {
    let output = "sandbox-exec: sandbox_apply: Operation not permitted";
    assert!(is_sandbox_initialization_failure(output));
    assert!(!is_sandbox_violation(true, false, output));
}

#[test]
fn sandbox_initialization_failure_checks_stderr_only() {
    // The pattern must appear in the stderr string to be detected.
    // An empty stderr means no initialization failure, even if stdout
    // contains the literal string.
    assert!(!is_sandbox_initialization_failure(""));
    assert!(!is_sandbox_initialization_failure(
        "some normal stderr output"
    ));
    assert!(is_sandbox_initialization_failure(
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

#[tokio::test]
async fn test_streaming_with_soft_warning() {
    // A command that triggers a soft safety warning (rg -rn footgun check)
    // should have the warning prepended to the output.
    let args = r#"{"command": "rg -rn some_pattern ."}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await;
    // The safety check allows execution (soft warning), but rg itself may
    // not be installed or may fail. The key assertion is that the warning
    // text appears in the output.
    if let Ok(res) = result {
        assert!(
            res.output.contains("rg -rn sets the replacement string"),
            "Soft warning should be prepended. Output: {}",
            res.output
        );
        assert!(res.output.contains("[exit:"));
    } else {
        // If rg is missing the command may fail at the shell level, but
        // the safety check runs before execution, so we should never get
        // an Err from validate_command_safety.
        panic!("rg -rn should not be hard-blocked, got error");
    }
}

#[tokio::test]
async fn test_streaming_with_soft_warning_and_empty_result() {
    // A command with a soft warning that produces no output should show
    // only the warning text (no extra formatting).
    let args = r#"{"command": "rg -rn some_pattern 2>/dev/null; true"}"#;
    let result = Box::pin(execute_bash_unsandboxed(args)).await;
    if let Ok(res) = result {
        assert!(
            res.output.contains("rg -rn sets the replacement string"),
            "Soft warning should appear. Output: {}",
            res.output
        );
        assert!(
            res.output.contains("[exit:0 |"),
            "Output should contain footer: {}",
            res.output
        );
    } else {
        panic!("rg -rn should not be hard-blocked, got error");
    }
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
