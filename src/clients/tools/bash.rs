use serde::Deserialize;
use std::process::Stdio;
use std::time::Instant;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::{Duration, timeout};
use tracing::debug;

use crate::time_format::format_seconds_tenths;

/// Maximum number of null bytes or control characters (excluding common whitespace)
/// allowed before considering output as binary.
const BINARY_NULL_BYTE_THRESHOLD: usize = 8;

/// Non-printable character ratio threshold (30%).
/// When more than this percentage of bytes are non-printable (excluding
/// common whitespace and high bytes), the data is considered binary.
const BINARY_RATIO_THRESHOLD_PERCENT: usize = 30;
const BYTES_PER_KIB: u128 = 1024;
const TENTHS_PER_KIB: u128 = 10;
const EXIT_ZERO_STDERR_WARNING: &str = "[stderr output present despite exit 0]";

/// Maximum number of bytes the Bash tool will return inline.
/// Output exceeding this limit is written to a temporary file and the agent
/// receives a truncated message with a path to the full output.
pub(super) const BASH_OUTPUT_MAX_BYTES: usize = 50_000;

/// A generous cap: read up to 2× the inline limit so `truncate_output()`
/// has enough data for a useful head+tail preview and temp-file dump.
pub(super) const BASH_READ_CAP: usize = BASH_OUTPUT_MAX_BYTES * 2;

/// Arguments for bash execution, including optional sandboxing
struct BashExecutionArgs {
    command: String,
    timeout: u64,
    use_sandbox: bool,
}

impl BashExecutionArgs {
    fn from_json(arguments: &str) -> Result<Self, String> {
        #[derive(Deserialize)]
        struct BashArgs {
            command: String,
            timeout: Option<u64>,
        }

        let args: BashArgs =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid bash arguments: {e}"))?;

        Ok(Self {
            command: args.command,
            timeout: args.timeout.unwrap_or(60),
            use_sandbox: !super::sandbox::is_sandbox_disabled(),
        })
    }
}

#[cfg(test)]
impl BashExecutionArgs {
    fn with_sandbox(mut self, use_sandbox: bool) -> Self {
        self.use_sandbox = use_sandbox;
        self
    }
}

// =============================================================================
// Bash Tool Definition
// =============================================================================

/// Returns the Bash tool definition
pub(super) fn bash_tool() -> super::Tool {
    super::Tool {
        type_: "function".to_string(),
        name: "Bash".to_string(),
        description: include_str!("bash-description.txt").to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command to execute"
                },
                "timeout": {
                    "type": "number",
                    "description": "Timeout in seconds (default: 60)"
                }
            },
            "required": ["command"]
        }),
    }
}

// =============================================================================
// Bash Execution
// =============================================================================

/// Detect if a failed sandboxed command looks like a sandbox-related permission failure.
fn is_sandbox_violation(sandboxed: bool, success: bool, output: &str) -> bool {
    if !sandboxed || success {
        return false;
    }

    if is_sandbox_initialization_failure(output) {
        return false;
    }

    output.contains("Operation not permitted")
        || output.contains("os error 1")
        || (output.contains("Permission denied") && output.contains("sandbox"))
}

/// Detect when the sandbox engine itself failed before the requested command ran.
///
/// Must be called with **stderr only**, not combined stdout+stderr. The
/// `sandbox-exec` wrapper writes its initialization errors to stderr, so
/// checking only stderr avoids false positives when a user command prints
/// or searches for the literal string `sandbox-exec: sandbox_apply` in its
/// normal output.
fn is_sandbox_initialization_failure(stderr: &str) -> bool {
    stderr.contains("sandbox-exec: sandbox_apply")
}

const fn should_warn_exit_zero_stderr(success: bool, stderr: &str) -> bool {
    success && !stderr.is_empty()
}

/// Check if raw bytes appear to be binary data rather than text.
/// Returns true if the data contains:
/// - Multiple null bytes (common in binary files)
/// - A high ratio of non-printable characters (excluding common whitespace)
fn is_binary_data(data: &[u8]) -> bool {
    if data.is_empty() {
        return false;
    }

    // Count null bytes - even a few null bytes strongly indicate binary
    let mut null_count: usize = 0;
    for &b in data {
        if b == 0 {
            null_count += 1;
        }
    }
    if null_count > BINARY_NULL_BYTE_THRESHOLD {
        return true;
    }

    // Count non-printable characters (excluding common whitespace: \t, \n, \r)
    let mut non_printable_count: usize = 0;
    for &b in data {
        // Allow tabs, newlines, and carriage returns
        if matches!(b, b'\t' | b'\n' | b'\r') {
            continue;
        }
        // Allow printable ASCII (32-126)
        if (32..=126).contains(&b) {
            continue;
        }
        // Allow high bytes that could be valid UTF-8 continuation/start bytes
        // (we'll let the UTF-8 check below catch actual invalid sequences)
        if b >= 128 {
            continue;
        }
        non_printable_count += 1;
    }

    // If more than 30% of the data is non-printable, it's likely binary
    non_printable_count * 100 > data.len() * BINARY_RATIO_THRESHOLD_PERCENT
}

/// Format bytes as KiB with one decimal place using integer rounding.
fn format_kib_tenths(size_bytes: usize) -> String {
    let rounded_tenths = (size_bytes as u128)
        .saturating_mul(TENTHS_PER_KIB)
        .saturating_add(BYTES_PER_KIB / 2)
        / BYTES_PER_KIB;
    format!(
        "{}.{:01}",
        rounded_tenths / TENTHS_PER_KIB,
        rounded_tenths % TENTHS_PER_KIB
    )
}

/// Create a result message for binary output, saving the data to a temp file.
fn handle_binary_output(
    data: &[u8],
    exit_code: i32,
    elapsed_ms: u128,
    warn_exit_zero_stderr: bool,
) -> String {
    let size_bytes = data.len();
    let size_kb = format_kib_tenths(size_bytes);

    // Try to detect MIME type using the `file` command if available
    let mime_type = detect_mime_type(data);

    // Save binary data to a temp file
    let tmp_dir = std::env::temp_dir().join("cake");
    _ = std::fs::create_dir_all(&tmp_dir);
    let file_name = format!("bash_binary_{}", uuid::Uuid::new_v4());
    let tmp_path = tmp_dir.join(&file_name);

    match std::fs::write(&tmp_path, data) {
        Ok(()) => {
            let footer = format_metadata_suffix(exit_code, elapsed_ms, warn_exit_zero_stderr);
            format!(
                "[Binary output detected - {size_bytes} bytes ({size_kb} KB)]\n\
                 Detected type: {}\n\
                 Binary data saved to: {}\n\
                 The command produced binary output which cannot be displayed as text.\n\
                 You can inspect the file with appropriate tools (e.g., `file`, `hexdump`, `xxd`).\n\
                 {}",
                mime_type.unwrap_or("unknown"),
                tmp_path.display(),
                footer
            )
        },
        Err(e) => {
            let footer = format_metadata_suffix(exit_code, elapsed_ms, warn_exit_zero_stderr);
            format!(
                "[Binary output detected - {size_bytes} bytes ({size_kb} KB)]\n\
                 Detected type: {}\n\
                 Failed to save binary data to temp file: {e}\n\
                 The command produced binary output which cannot be displayed as text.\n\
                 {}",
                mime_type.unwrap_or("unknown"),
                footer
            )
        },
    }
}

/// Attempt to detect the MIME type of binary data using content-based detection.
/// Returns None if the type cannot be determined.
fn detect_mime_type(data: &[u8]) -> Option<&'static str> {
    infer::get(data).map(|kind| kind.mime_type())
}

/// Format metadata footer with exit code and elapsed time
/// Shows milliseconds for values under 1 second, seconds otherwise
fn format_metadata_footer(exit_code: i32, elapsed_ms: u128) -> String {
    if elapsed_ms > 999 {
        let elapsed_sec = format_seconds_tenths(elapsed_ms);
        format!("[exit:{exit_code} | {elapsed_sec}s]")
    } else {
        format!("[exit:{exit_code} | {elapsed_ms}ms]")
    }
}

fn format_metadata_suffix(exit_code: i32, elapsed_ms: u128, warn_exit_zero_stderr: bool) -> String {
    let footer = format_metadata_footer(exit_code, elapsed_ms);
    if warn_exit_zero_stderr {
        format!("{EXIT_ZERO_STDERR_WARNING}\n\n{footer}")
    } else {
        footer
    }
}

/// Append metadata footer to output
fn append_metadata(
    output: &str,
    exit_code: i32,
    elapsed_ms: u128,
    warn_exit_zero_stderr: bool,
) -> String {
    let footer = format_metadata_suffix(exit_code, elapsed_ms, warn_exit_zero_stderr);
    if output.is_empty() {
        footer
    } else {
        format!("{}\n\n{footer}", output.trim_end())
    }
}

/// Execute a bash command
pub(super) async fn execute_bash(
    context: &super::ToolContext,
    arguments: &str,
) -> Result<super::ToolResult, String> {
    let args = BashExecutionArgs::from_json(arguments)?;
    Box::pin(execute_bash_with_args(context, args)).await
}

#[expect(
    clippy::too_many_lines,
    reason = "bash execution spans safety checks, sandbox setup, and output handling"
)]
async fn execute_bash_with_args(
    context: &super::ToolContext,
    args: BashExecutionArgs,
) -> Result<super::ToolResult, String> {
    // Pre-execution safety check: block known-destructive commands,
    // collect soft warnings to prepend to output.
    let safety_warnings = super::bash_safety::validate_command_safety(&args.command)?;

    let start_time = Instant::now();

    // Build sandbox configuration with additional directories
    let sandbox_config = super::sandbox::SandboxConfig::build(context);

    // Create command with proper stdio configuration
    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(&args.command)
        .current_dir(&context.cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    // Apply sandbox if enabled
    if args.use_sandbox {
        if let Some(strategy) = super::sandbox::detect_platform()? {
            strategy.apply(&mut command, &sandbox_config)?;
        }
    } else {
        tracing::debug!("Sandbox disabled; running without filesystem restrictions");
    }

    // Spawn the command with piped stdout/stderr for streaming
    let mut child = command
        .spawn()
        .map_err(|e| format!("Failed to spawn command: {e}"))?;

    let mut stdout = child.stdout.take().ok_or("Failed to capture stdout")?;
    let mut stderr = child.stderr.take().ok_or("Failed to capture stderr")?;

    let mut buf = Vec::with_capacity(BASH_OUTPUT_MAX_BYTES);
    let mut stderr_buf = Vec::new();
    let mut tmp_stdout = [0u8; 8192];
    let mut tmp_stderr = [0u8; 8192];
    let mut hit_cap = false;

    // Read both streams concurrently, interleaved, with a timeout.
    // stderr is also captured separately so sandbox-init errors can be
    // detected from stderr alone, avoiding false positives when a user
    // command prints the literal string `sandbox-exec: sandbox_apply`.
    let read_result = timeout(Duration::from_secs(args.timeout), async {
        loop {
            tokio::select! {
                n = stdout.read(&mut tmp_stdout) => {
                    let n = n.map_err(|e| format!("stdout read error: {e}"))?;
                    if n == 0 {
                        // stdout closed — read remaining stderr
                        loop {
                            let n = stderr.read(&mut tmp_stderr).await
                                .map_err(|e| format!("stderr read error: {e}"))?;
                            if n == 0 { return Ok::<_, String>(()); }
                            buf.extend_from_slice(&tmp_stderr[..n]);
                            stderr_buf.extend_from_slice(&tmp_stderr[..n]);
                            if buf.len() >= BASH_READ_CAP { hit_cap = true; return Ok(()); }
                        }
                    }
                    buf.extend_from_slice(&tmp_stdout[..n]);
                    if buf.len() >= BASH_READ_CAP { hit_cap = true; return Ok(()); }
                }
                n = stderr.read(&mut tmp_stderr) => {
                    let n = n.map_err(|e| format!("stderr read error: {e}"))?;
                    if n == 0 {
                        // stderr closed — read remaining stdout
                        loop {
                            let n = stdout.read(&mut tmp_stdout).await
                                .map_err(|e| format!("stdout read error: {e}"))?;
                            if n == 0 { return Ok(()); }
                            buf.extend_from_slice(&tmp_stdout[..n]);
                            if buf.len() >= BASH_READ_CAP { hit_cap = true; return Ok(()); }
                        }
                    }
                    buf.extend_from_slice(&tmp_stderr[..n]);
                    stderr_buf.extend_from_slice(&tmp_stderr[..n]);
                    if buf.len() >= BASH_READ_CAP { hit_cap = true; return Ok(()); }
                }
            }
        }
    })
    .await;

    match read_result {
        Ok(Ok(())) => {},
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err(format!("Command timed out after {} seconds", args.timeout)),
    }

    // If we hit the cap, kill the child explicitly
    if hit_cap {
        _ = child.kill().await;
    }
    let status = child.wait().await.ok();
    let elapsed_ms = start_time.elapsed().as_millis();
    let stderr_str = String::from_utf8_lossy(&stderr_buf);
    let success = status
        .as_ref()
        .is_some_and(std::process::ExitStatus::success);
    let exit_code = status.and_then(|s| s.code()).unwrap_or(-1);
    let warn_exit_zero_stderr = should_warn_exit_zero_stderr(success, &stderr_str);

    // Check for binary data before converting to string
    if is_binary_data(&buf) {
        return Ok(super::ToolResult {
            output: handle_binary_output(&buf, exit_code, elapsed_ms, warn_exit_zero_stderr),
        });
    }

    let output_str = String::from_utf8_lossy(&buf);

    if args.use_sandbox && is_sandbox_initialization_failure(&stderr_str) {
        return Err(format!(
            "{}\n\n\
            macOS sandbox unavailable: sandbox-exec could not apply a sandbox profile, \
            so the requested command did not run. This commonly happens when cake is \
            itself running inside another Seatbelt sandbox. Set CAKE_SANDBOX=off to \
            run Bash commands without filesystem sandboxing.",
            output_str.trim_end()
        ));
    }

    let result = if output_str.is_empty() {
        String::new()
    } else if hit_cap {
        format!("{output_str}\n[... output truncated at {BASH_READ_CAP} bytes ...]")
    } else if success {
        output_str.into_owned()
    } else if is_sandbox_violation(args.use_sandbox, success, &output_str) {
        format!(
            "{output_str}\n\n\
            [Sandbox restriction]: This command was blocked by the filesystem sandbox. \
            The sandbox restricts file access to the project directory and standard system paths. \
            Do NOT retry with different workarounds — the restriction is intentional. \
            Instead, inform the user that this command requires access outside the sandbox \
            and suggest they run it directly in their terminal."
        )
    } else {
        output_str.into_owned()
    };

    let result = truncate_output(&result, exit_code, elapsed_ms, warn_exit_zero_stderr);

    let output = prepend_safety_warnings(result, &safety_warnings);

    Ok(super::ToolResult { output })
}

#[cfg(test)]
async fn execute_bash_unsandboxed(arguments: &str) -> Result<super::ToolResult, String> {
    let args = BashExecutionArgs::from_json(arguments)?.with_sandbox(false);
    let context = super::ToolContext::from_current_process();
    Box::pin(execute_bash_with_args(&context, args)).await
}

/// If `output` exceeds [`BASH_OUTPUT_MAX_BYTES`], write the full text to a
/// temporary file and return a summary pointing to that file. Otherwise return
/// the output with the metadata footer appended. The temp file receives only
/// the raw command output (no footer); the footer is included in the inline
/// summary so it is always visible in the tool response.
pub(super) fn truncate_output(
    output: &str,
    exit_code: i32,
    elapsed_ms: u128,
    warn_exit_zero_stderr: bool,
) -> String {
    if output.len() <= BASH_OUTPUT_MAX_BYTES {
        return append_metadata(output, exit_code, elapsed_ms, warn_exit_zero_stderr);
    }

    let footer = format_metadata_suffix(exit_code, elapsed_ms, warn_exit_zero_stderr);
    let total_bytes = output.len();
    let total_lines = output.lines().count();

    // Try to write the full output to a temp file so the agent can search it.
    let tmp_dir = std::env::temp_dir().join("cake");
    _ = std::fs::create_dir_all(&tmp_dir);
    let file_name = format!("bash_output_{}.txt", uuid::Uuid::new_v4());
    let tmp_path = tmp_dir.join(&file_name);

    if let Err(e) = std::fs::write(&tmp_path, output) {
        // Could not write — fall back to a truncated inline result.
        debug!(
            "Failed to write overflow output to {}: {e}",
            tmp_path.display()
        );

        let half = BASH_OUTPUT_MAX_BYTES / 2;
        let head_end = output.floor_char_boundary(half);
        let tail_start = output.ceil_char_boundary(total_bytes - half);
        let (head, _) = output.split_at(head_end);
        let (_, tail) = output.split_at(tail_start);
        return format!(
            "[Output too long — {total_bytes} bytes, {total_lines} lines. \
             The command was too verbose; reformulate with less output \
             (e.g. pipe through `head`, `tail`, or `grep`).]\n\n\
             --- first ~{half} bytes ---\n{head}\n\n\
             --- last ~{half} bytes ---\n{tail}\n{footer}",
        );
    }

    let preview = BASH_OUTPUT_MAX_BYTES / 4;
    let head_end = output.floor_char_boundary(preview);
    let tail_start = output.ceil_char_boundary(total_bytes - preview);
    let (head, _) = output.split_at(head_end);
    let (_, tail) = output.split_at(tail_start);
    format!(
        "[Output too long — {total_bytes} bytes, {total_lines} lines.]\n\
         Full output saved to: {path}\n\
         You can search it with `grep` or view portions with `head`/`tail`.\n\
         Consider reformulating the command to produce less output.\n\n\
         --- first ~{preview} bytes ---\n{head}\n\n\
         --- last ~{preview} bytes ---\n{tail}\n{footer}",
        path = tmp_path.display(),
    )
}

/// Prepend soft safety warnings to command output, if any.
///
/// `truncate_output` always returns a non-empty string (at minimum the
/// metadata footer), so the warning is safely interleaved with `\n\n`.
fn prepend_safety_warnings(output: String, warnings: &[String]) -> String {
    if warnings.is_empty() {
        output
    } else {
        format!("{}\n\n{output}", warnings.join("\n\n"))
    }
}

#[cfg(test)]
#[path = "bash_tests.rs"]
mod tests;
