use serde::Deserialize;
use std::process::Stdio;
use std::time::Instant;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::time::{Duration, timeout};
use tracing::debug;

#[cfg(test)]
use crate::clients::judge::JudgeContext;
use crate::clients::judge::{
    JudgeDecision, JudgeOutcome, JudgeRequest, evaluate_command_observed, judge_is_enabled,
    repo_state_digest,
};
use crate::clients::tools::secure_temp_dir::secure_temp_dir;
use crate::config::settings::JUDGE_BYPASS_ENV;
use crate::config::toolbox::ToolboxProcessGuard;
use crate::session_telemetry::{CompensationEventTelemetry, CompensationKind};
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
const EMPTY_SEARCH_NO_MATCH_ANNOTATION: &str = "(no matches)";

/// Maximum number of bytes the Bash tool will return inline.
/// Output exceeding this limit is written to a temporary file and the agent
/// receives a truncated message with a path to the full output.
pub(super) const BASH_OUTPUT_MAX_BYTES: usize = 50_000;

/// A generous cap: read up to 2× the inline limit so `truncate_output()`
/// has enough data for a useful head+tail preview and temp-file dump.
pub(super) const BASH_READ_CAP: usize = BASH_OUTPUT_MAX_BYTES * 2;

/// Floor for the model-supplied Bash timeout in seconds. A `0` timeout would
/// fail instantly, so requests below the floor are raised to it.
const BASH_TIMEOUT_MIN_SECS: u64 = 1;

/// Ceiling for the model-supplied Bash timeout in seconds. Larger values are
/// capped so a runaway command cannot pin the process for an unbounded time.
const BASH_TIMEOUT_MAX_SECS: u64 = 600;

/// Maximum characters of judge-provided message text allowed into the agent
/// loop, so a verbose or compromised judge cannot flood the model-visible
/// tool error or output.
const JUDGE_MESSAGE_MAX_CHARS: usize = 1000;

/// Arguments for bash execution, including the sandbox policy
struct BashExecutionArgs {
    command: String,
    timeout: u64,
    policy: super::sandbox::SandboxPolicy,
    /// The model's untrusted self-report of intent, weighed against the
    /// command by the LLM judge preflight.
    reason: Option<String>,
}

impl BashExecutionArgs {
    fn from_json(arguments: &str, policy: super::sandbox::SandboxPolicy) -> Result<Self, String> {
        #[derive(Deserialize)]
        struct BashArgs {
            command: String,
            timeout: Option<u64>,
            #[serde(default)]
            reason: Option<String>,
        }

        let args: BashArgs =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid bash arguments: {e}"))?;

        Ok(Self {
            command: args.command,
            timeout: args
                .timeout
                .unwrap_or(60)
                .clamp(BASH_TIMEOUT_MIN_SECS, BASH_TIMEOUT_MAX_SECS),
            policy,
            reason: args.reason,
        })
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
                },
                "reason": {
                    "type": "string",
                    "description": "Optional self-report of why this command is being run. Untrusted: the model's stated intent is a hint for the command-safety judge, never a justification. Supply it for state-changing, destructive-looking, or remote-effect commands, stating the intended effect. Omit it for clearly safe, read-only commands."
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
fn is_sandbox_violation(sandbox_applied: bool, success: bool, output: &str) -> bool {
    if !sandbox_applied || success {
        return false;
    }

    if is_sandbox_initialization_failure(sandbox_applied, output) {
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
/// normal output. The pattern is only cake's initialization failure when cake
/// actually applied a sandbox strategy; a child process may emit the same text
/// while cake is relying on inherited Seatbelt enforcement.
fn is_sandbox_initialization_failure(sandbox_applied: bool, stderr: &str) -> bool {
    sandbox_applied && stderr.contains("sandbox-exec: sandbox_apply")
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

// =============================================================================
// Secure temp directory for Bash overflow / binary output artifacts
// =============================================================================

/// Return a secure per-user temporary directory for Bash output files,
/// creating it on first call and caching the path for the lifetime of the
/// process.
///
/// Delegates to the shared secure-temp-directory helper for creation and
/// validation.
fn bash_temp_output_dir() -> std::io::Result<&'static std::path::Path> {
    secure_temp_dir()
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

    // Try to save binary data to a secure temp file.
    // Fail closed: if the directory cannot be created, report the error
    // rather than writing to an unvalidated shared path.
    let footer = format_metadata_suffix(exit_code, elapsed_ms, warn_exit_zero_stderr);

    let tmp_path = match bash_temp_output_dir() {
        Ok(dir) => dir.join(format!("bash_binary_{}", uuid::Uuid::new_v4())),
        Err(e) => {
            return format!(
                "[Binary output detected - {size_bytes} bytes ({size_kb} KB)]\n\
                 Detected type: {}\n\
                 Failed to save binary data to temp file: could not create \
                 secure directory: {e}\n\
                 The command produced binary output which cannot be displayed as text.\n\
                 {}",
                mime_type.unwrap_or("unknown"),
                footer
            );
        },
    };

    match std::fs::write(&tmp_path, data) {
        Ok(()) => {
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

fn annotate_empty_search_result(
    command: &str,
    output: String,
    exit_code: i32,
    stderr: &str,
) -> String {
    if exit_code == 1
        && output.is_empty()
        && stderr.is_empty()
        && command_starts_with_search(command)
    {
        EMPTY_SEARCH_NO_MATCH_ANNOTATION.to_string()
    } else {
        output
    }
}

fn command_starts_with_search(command: &str) -> bool {
    let Some(first_token) = command.split_whitespace().next() else {
        return false;
    };
    let command_name = first_token.rsplit('/').next().unwrap_or(first_token);
    matches!(command_name, "rg" | "ripgrep" | "grep" | "egrep" | "fgrep")
}

#[cfg(unix)]
fn terminate_process_group(child: &Child) {
    if let Some(pid) = child.id() {
        #[expect(
            clippy::cast_possible_wrap,
            reason = "PIDs fit in i32 on all supported Unix targets"
        )]
        // SAFETY: the PID belongs to the live child, whose process group ID
        // equals its PID because `process_group(0)` is set before spawning.
        // A negative PID in `kill(2)` targets every process in that group.
        unsafe {
            libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn terminate_process_group(child: &mut Child) {
    let _ = child.start_kill();
}

/// Execute a bash command. Tests use this convenience wrapper; the tool
/// executor calls [`execute_bash_for_call`] to attribute judge attempts.
#[cfg(test)]
pub(super) async fn execute_bash(
    context: &super::ToolContext,
    arguments: &str,
) -> Result<super::ToolResult, super::ToolError> {
    execute_bash_for_call(context, arguments, None).await
}

/// Execute a bash command, attributing judge attempts to the originating tool
/// call when `call_id` is present.
pub(super) async fn execute_bash_for_call(
    context: &super::ToolContext,
    arguments: &str,
    call_id: Option<String>,
) -> Result<super::ToolResult, super::ToolError> {
    let args = BashExecutionArgs::from_json(arguments, context.sandbox_policy)?;
    Box::pin(execute_bash_with_args(context, args, call_id)).await
}

#[expect(
    clippy::too_many_lines,
    reason = "bash execution spans safety checks, sandbox setup, and output handling"
)]
async fn execute_bash_with_args(
    context: &super::ToolContext,
    args: BashExecutionArgs,
    call_id: Option<String>,
) -> Result<super::ToolResult, super::ToolError> {
    // Command-safety preflight: the LLM judge is the only non-sandbox command
    // gate. A block prevents spawn and returns the judge's message as the tool
    // error; a warn prepends guidance to the output; a judge failure fails
    // closed (blocks) with an explanation. Judge decisions and denials are
    // recorded as telemetry compensation events.
    let preflight = bash_judge_preflight(context, &args, call_id).await?;
    let judge_warnings = preflight.warnings;
    let judge_events = preflight.compensation_events;

    let use_sandbox = args.policy != super::sandbox::SandboxPolicy::DangerFullAccess;

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

    // Apply sandbox if enabled.
    // Keep the guard alive until the child has finished so sandbox
    // resources (e.g., macOS temp profile files) are cleaned up
    // deterministically.
    let sandbox_guard = if use_sandbox {
        if let Some(strategy) = super::sandbox::detect_platform()
            .map_err(|e| judge_tool_error(judge_events.clone(), e))?
        {
            Some(
                strategy
                    .apply(&mut command, &sandbox_config)
                    .map_err(|e| judge_tool_error(judge_events.clone(), e))?,
            )
        } else {
            None
        }
    } else {
        tracing::debug!("Sandbox disabled; running without filesystem restrictions");
        None
    };
    let sandbox_applied = sandbox_guard.is_some();

    // Place the child in its own process group so that SIGKILL to the
    // negative PID kills all descendants, not just the direct child.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }

    // Spawn the command with piped stdout/stderr for streaming
    let mut child = command.spawn().map_err(|e| {
        judge_tool_error(
            judge_events.clone(),
            format!("Failed to spawn command: {e}"),
        )
    })?;

    // RAII guard: kills the whole process group on drop unless defused.
    // This ensures Ctrl-C or any other future cancellation terminates
    // descendant processes, not just the direct child.
    let mut guard = ToolboxProcessGuard::new(child.id());

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| judge_tool_error(judge_events.clone(), "Failed to capture stdout"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| judge_tool_error(judge_events.clone(), "Failed to capture stderr"))?;

    // The configured timeout covers the full lifecycle: reading both
    // streams, optionally killing the process group on cap, and reaping
    // the child.  This prevents hangs when a command closes both captured
    // streams while continuing to run — the wait is bounded by the same
    // timeout as the read loop.
    #[expect(
        clippy::large_futures,
        reason = "Bash lifecycle covers read, kill, and wait"
    )]
    let lifecycle_result = timeout(Duration::from_secs(args.timeout), async {
        let mut buf = Vec::with_capacity(BASH_OUTPUT_MAX_BYTES);
        let mut stderr_buf = Vec::new();
        let mut tmp_stdout = [0u8; 8192];
        let mut tmp_stderr = [0u8; 8192];
        let mut hit_cap = false;

        // Read both streams concurrently, interleaved, with a timeout.
        // stderr is also captured separately so sandbox-init errors can be
        // detected from stderr alone, avoiding false positives when a user
        // command prints the literal string `sandbox-exec: sandbox_apply`.
        loop {
            tokio::select! {
                n = stdout.read(&mut tmp_stdout) => {
                    let n = n.map_err(|e| format!("stdout read error: {e}"))?;
                    if n == 0 {
                        // stdout closed — read remaining stderr
                        loop {
                            let n = stderr.read(&mut tmp_stderr).await
                                .map_err(|e| format!("stderr read error: {e}"))?;
                            if n == 0 { break; }
                            buf.extend_from_slice(&tmp_stderr[..n]);
                            stderr_buf.extend_from_slice(&tmp_stderr[..n]);
                            if buf.len() >= BASH_READ_CAP { hit_cap = true; break; }
                        }
                        break;
                    }
                    buf.extend_from_slice(&tmp_stdout[..n]);
                    if buf.len() >= BASH_READ_CAP { hit_cap = true; break; }
                }
                n = stderr.read(&mut tmp_stderr) => {
                    let n = n.map_err(|e| format!("stderr read error: {e}"))?;
                    if n == 0 {
                        // stderr closed — read remaining stdout
                        loop {
                            let n = stdout.read(&mut tmp_stdout).await
                                .map_err(|e| format!("stdout read error: {e}"))?;
                            if n == 0 { break; }
                            buf.extend_from_slice(&tmp_stdout[..n]);
                            if buf.len() >= BASH_READ_CAP { hit_cap = true; break; }
                        }
                        break;
                    }
                    buf.extend_from_slice(&tmp_stderr[..n]);
                    stderr_buf.extend_from_slice(&tmp_stderr[..n]);
                    if buf.len() >= BASH_READ_CAP { hit_cap = true; break; }
                }
            }
        }

        // If we hit the cap, terminate the process group so descendants
        // do not survive.
        if hit_cap {
            #[cfg(unix)]
            terminate_process_group(&child);
            #[cfg(not(unix))]
            terminate_process_group(&mut child);
        }

        let status = child.wait().await.ok();
        Ok::<_, String>((buf, stderr_buf, hit_cap, status))
    })
    .await;

    let (buf, stderr_buf, hit_cap, status) = match lifecycle_result {
        Ok(Ok(tuple)) => tuple,
        Ok(Err(e)) => return Err(judge_tool_error(judge_events, e)),
        Err(_) => {
            // Timed out: terminate the process group and reap with a
            // short grace period so the OS has time to deliver SIGKILL.
            #[cfg(unix)]
            terminate_process_group(&child);
            #[cfg(not(unix))]
            terminate_process_group(&mut child);
            // Grace period: give the OS time to deliver SIGKILL.
            let _grace = timeout(Duration::from_secs(5), child.wait()).await;
            return Err(judge_tool_error(
                judge_events,
                format!("Command timed out after {} seconds", args.timeout),
            ));
        },
    };

    // Normal completion (or hit_cap with group already killed by
    // terminate_process_group above): defuse the guard so it does not
    // send a harmless-but-unnecessary SIGKILL to the reaped group.
    guard.defuse();

    let elapsed_ms = start_time.elapsed().as_millis();
    let stderr_str = String::from_utf8_lossy(&stderr_buf);
    let success = status
        .as_ref()
        .is_some_and(std::process::ExitStatus::success);
    let exit_code = status.and_then(|s| s.code()).unwrap_or(-1);
    let warn_exit_zero_stderr = should_warn_exit_zero_stderr(success, &stderr_str);

    // Check for binary data before converting to string. Judge warnings still
    // prepend here: a `warn` verdict ran the command, so its guidance must
    // reach the model even when the output is binary.
    if is_binary_data(&buf) {
        let mut compensation_events = judge_events;
        let spilled = buf.len() > BASH_OUTPUT_MAX_BYTES;
        push_truncation_event_if(&mut compensation_events, "Bash", hit_cap, spilled);
        let output = handle_binary_output(&buf, exit_code, elapsed_ms, warn_exit_zero_stderr);
        return Ok(super::ToolResult {
            output: prepend_safety_warnings(output, &judge_warnings),
            compensation_events,
        });
    }

    let output_str = String::from_utf8_lossy(&buf);

    if is_sandbox_initialization_failure(sandbox_applied, &stderr_str) {
        return Err(judge_tool_error(
            judge_events,
            format!(
                "{}\n\n\
                macOS sandbox unavailable: sandbox-exec could not apply a sandbox profile, \
                so the requested command did not run. This commonly happens when cake is \
                itself running inside another Seatbelt sandbox. Run cake with \
                --sandbox danger-full-access (or set CAKE_SANDBOX=off) to run Bash \
                commands without filesystem sandboxing.",
                output_str.trim_end()
            ),
        ));
    }

    let result = if output_str.is_empty() {
        String::new()
    } else if hit_cap {
        format!("{output_str}\n[... output truncated at {BASH_READ_CAP} bytes ...]")
    } else if success {
        output_str.into_owned()
    } else if is_sandbox_violation(sandbox_applied, success, &output_str) {
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

    let result = annotate_empty_search_result(&args.command, result, exit_code, &stderr_str);
    let spilled = result.len() > BASH_OUTPUT_MAX_BYTES;
    let result = truncate_output(&result, exit_code, elapsed_ms, warn_exit_zero_stderr);
    let mut compensation_events = judge_events;
    push_truncation_event_if(&mut compensation_events, "Bash", hit_cap, spilled);

    let output = prepend_safety_warnings(result, &judge_warnings);

    Ok(super::ToolResult {
        output,
        compensation_events,
    })
}

/// Push an `output_truncation` compensation event when a Bash run hit the
/// read cap (process killed) or its output spilled to a temp file. One event
/// per run that truncated, whichever mechanism fired.
fn push_truncation_event_if(
    events: &mut Vec<CompensationEventTelemetry>,
    tool: &str,
    read_cap: bool,
    spilled: bool,
) {
    if read_cap || spilled {
        events.push(CompensationEventTelemetry::new(
            CompensationKind::OutputTruncation,
            Some(tool.to_string()),
        ));
    }
}

/// The result of a judge preflight that allows the command to run: the
/// warnings to prepend to the output plus the telemetry events recorded for
/// this call.
struct JudgePreflight {
    warnings: Vec<String>,
    compensation_events: Vec<CompensationEventTelemetry>,
}

/// Run the LLM-judge command-safety preflight for one Bash call.
///
/// The judge is the only non-sandbox command gate (`ExecPlan` Milestone 5):
/// every command is evaluated before spawn, and the judge stays active under
/// `danger-full-access` and `CAKE_SANDBOX=off`, matching the old guard's
/// independence from the sandbox.
///
/// Every judge decision and every fail-closed denial is recorded as a
/// telemetry compensation event (verdict + code + latency, bypass, or failure
/// class), including on the `Err` path, so the gate's behavior stays
/// observable even when the tool call fails.
async fn bash_judge_preflight(
    context: &super::ToolContext,
    args: &BashExecutionArgs,
    call_id: Option<String>,
) -> Result<JudgePreflight, super::ToolError> {
    // Empty commands have nothing to judge; `bash -c ""` is harmless and the
    // old guard skipped them too.
    if args.command.trim().is_empty() {
        return Ok(JudgePreflight {
            warnings: Vec::new(),
            compensation_events: Vec::new(),
        });
    }

    let Some(judge) = context.judge.as_deref() else {
        return Err(fail_closed_tool_error(
            "missing_context",
            "the command-safety judge is not configured for this run",
        ));
    };

    // The emergency bypass short-circuits before any judge setup: a disabled
    // judge must not fail on an unusable model or rubric, because the bypass
    // is the recovery path when judge configuration is broken. The bypass is
    // still recorded so the escape hatch cannot be used silently.
    let bypass_env = std::env::var(JUDGE_BYPASS_ENV).ok();
    if !judge_is_enabled(&judge.settings, bypass_env.as_deref()) {
        return Ok(JudgePreflight {
            warnings: Vec::new(),
            compensation_events: vec![CompensationEventTelemetry::judge_bypass()],
        });
    }

    let client = judge
        .judge_client()
        .map_err(|e| fail_closed_tool_error(e.class, &e.message))?;
    let request = JudgeRequest::new(
        args.command.clone(),
        context.cwd.clone(),
        args.reason.clone(),
    )
    .with_repo_digest(repo_state_digest(&context.cwd))
    .with_call_id(call_id);

    let evaluation = evaluate_command_observed(
        client,
        &judge.settings,
        request,
        bypass_env.as_deref(),
        false,
    )
    .await;
    // Persist finalized attempts as soon as judging completes: an interrupted
    // command (for example Ctrl-C on a hung Bash call) cancels the agent
    // future before the tool result and its compensation events are recorded,
    // so waiting for that path would drop the attempts.
    record_judge_attempts(judge, &evaluation.attempts);
    observed_evaluation_to_preflight(evaluation)
}

/// Persist finalized judge attempts through the run's telemetry sink, if any.
fn record_judge_attempts(
    judge: &crate::clients::judge::JudgeContext,
    attempts: &[crate::session_telemetry::JudgeAttemptTelemetry],
) {
    if let Some(sink) = &judge.record_attempt {
        for attempt in attempts {
            sink.record(attempt.clone());
        }
    }
}

fn observed_evaluation_to_preflight(
    evaluation: crate::clients::judge::JudgeEvaluation,
) -> Result<JudgePreflight, super::ToolError> {
    // Cumulative wall time across every attempt, including the backoff waits
    // between them, so the verdict/fail-closed latency reflects the whole
    // judge operation after a bounded recovery.
    let latency_ms = evaluation.attempts.iter().fold(0_u64, |acc, attempt| {
        acc.saturating_add(attempt.total_ms)
            .saturating_add(attempt.retry_delay_ms)
    });
    match evaluation.outcome {
        Ok(outcome) => judge_preflight_outcome(outcome, latency_ms),
        Err(error) => {
            let class = error.error_class();
            Err(fail_closed_tool_error(class, error))
        },
    }
}

/// Map a judge-path outcome to the preflight result: the warnings to prepend
/// plus the telemetry event recorded for the decision.
///
/// A `warn` verdict prepends its message as a `NOTICE`; an allow verdict, an
/// allowlist-overridden block (the verdict and the override flag are
/// preserved for telemetry), and the bypass run unannotated; a real `block`
/// blocks with the verdict reason as the tool error, carrying the verdict
/// event on the error path.
fn judge_preflight_outcome(
    outcome: JudgeOutcome,
    latency_ms: u64,
) -> Result<JudgePreflight, super::ToolError> {
    match outcome {
        JudgeOutcome::Bypassed => Ok(JudgePreflight {
            warnings: Vec::new(),
            compensation_events: vec![CompensationEventTelemetry::judge_bypass()],
        }),
        JudgeOutcome::Verdict {
            verdict,
            overridden,
        } => {
            let event = CompensationEventTelemetry::judge_verdict(
                verdict.decision.as_str(),
                verdict.code.as_deref(),
                latency_ms,
                overridden,
            );
            match verdict.decision {
                JudgeDecision::Block if !overridden => Err(super::ToolError {
                    message: format!(
                        "BLOCKED\n\nReason: {}",
                        sanitize_judge_message(&verdict.message)
                    ),
                    compensation_events: vec![event],
                }),
                JudgeDecision::Warn => Ok(JudgePreflight {
                    warnings: vec![format!(
                        "NOTICE: {}",
                        sanitize_judge_message(&verdict.message)
                    )],
                    compensation_events: vec![event],
                }),
                JudgeDecision::Block | JudgeDecision::Allow => Ok(JudgePreflight {
                    warnings: Vec::new(),
                    compensation_events: vec![event],
                }),
            }
        },
    }
}

/// Format a fail-closed block message for judge failures: the command is not
/// executed and the model sees why.
fn fail_closed_message(error: impl std::fmt::Display) -> String {
    format!(
        "BLOCKED\n\nThe command-safety judge was unavailable, so this command was \
         not executed (fail-closed).\n\n{error}"
    )
}

/// Build the tool error for a fail-closed denial, recording the failure class
/// so the denial is observable in telemetry.
fn fail_closed_tool_error(class: &'static str, error: impl std::fmt::Display) -> super::ToolError {
    super::ToolError {
        message: fail_closed_message(error),
        compensation_events: vec![CompensationEventTelemetry::judge_fail_closed(class)],
    }
}

/// Attach the judge preflight's telemetry events to a post-preflight tool
/// error, so an `allow`/`warn` verdict stays observable even when the command
/// itself fails after the gate (timeout, spawn failure, sandbox-init failure,
/// lifecycle read error). Without this the verdict recorded by the preflight
/// would be silently dropped on exactly the sessions the metrics exist to
/// analyze (`review F-001`).
fn judge_tool_error(
    events: Vec<CompensationEventTelemetry>,
    message: impl Into<String>,
) -> super::ToolError {
    super::ToolError {
        message: message.into(),
        compensation_events: events,
    }
}

/// Judge messages are model-generated text entering the agent loop. Cap the
/// length and strip control characters so a compromised or confused judge
/// cannot inject terminal control sequences or unbounded text into the
/// model-visible tool error or output. Line breaks are kept for readability.
fn sanitize_judge_message(message: &str) -> String {
    message
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
        .take(JUDGE_MESSAGE_MAX_CHARS)
        .collect()
}

/// Run one Bash call unsandboxed in a fresh temporary directory with a
/// bypassed judge, used by tests that exercise Bash output handling rather
/// than the judge path: the judge is never called and commands run ungated.
#[cfg(test)]
async fn execute_bash_unsandboxed(arguments: &str) -> Result<super::ToolResult, super::ToolError> {
    execute_bash_with_judge(arguments, Some(bypassed_judge_context())).await
}

/// Run one Bash call unsandboxed in a fresh temporary directory with an
/// explicit judge context, so tests can drive the judge preflight (`None`
/// exercises the fail-closed path).
///
/// The working directory is a fresh temp dir: tests never execute against the
/// developer's real checkout. Tests needing a specific fixture directory use
/// [`execute_bash_with_judge_in`] instead.
#[cfg(test)]
async fn execute_bash_with_judge(
    arguments: &str,
    judge: Option<std::sync::Arc<JudgeContext>>,
) -> Result<super::ToolResult, super::ToolError> {
    let dir = tempfile::tempdir().expect("hermetic temp dir for bash test");
    let result = execute_bash_with_judge_in(arguments, judge, dir.path()).await;
    drop(dir);
    result
}

/// Run one Bash call unsandboxed in an explicit fixture working directory
/// with an explicit judge context (`None` exercises the fail-closed path).
///
/// The working directory is explicit so tests never execute against the
/// developer's real checkout: pass a hermetic fixture (a `tempfile` directory,
/// or a temporary Git repository when a repository is needed).
#[cfg(test)]
async fn execute_bash_with_judge_in(
    arguments: &str,
    judge: Option<std::sync::Arc<JudgeContext>>,
    cwd: &std::path::Path,
) -> Result<super::ToolResult, super::ToolError> {
    let args =
        BashExecutionArgs::from_json(arguments, super::sandbox::SandboxPolicy::DangerFullAccess)?;
    let mut context = super::ToolContext::from_current_process();
    context.cwd = cwd.to_path_buf();
    context.judge = judge;
    Box::pin(execute_bash_with_args(&context, args, None)).await
}

/// A judge context with the emergency bypass enabled, used by tests that
/// exercise Bash output handling rather than the judge path: the judge is
/// never called and commands run ungated.
#[cfg(test)]
fn bypassed_judge_context() -> std::sync::Arc<JudgeContext> {
    use crate::config::model::{ApiType, ModelConfig};
    use std::collections::HashMap;

    let model_config = ModelConfig {
        model: "bypass/model".to_string(),
        api_type: ApiType::ChatCompletions,
        base_url: "http://127.0.0.1:9".to_string(),
        api_key_env: "JUDGE_TEST_KEY".to_string(),
        provider: None,
        provider_headers: None,
        temperature: None,
        top_p: None,
        max_output_tokens: None,
        context_window: None,
        reasoning_effort: None,
        reasoning_summary: None,
        reasoning_max_tokens: None,
        providers: vec![],
    };
    std::sync::Arc::new(JudgeContext {
        settings: crate::config::settings::JudgeSettings {
            enabled: false,
            ..crate::config::settings::JudgeSettings::default()
        },
        agent_model: crate::config::model::ResolvedModelConfig {
            model_config,
            api_key: String::new(),
        },
        models: HashMap::new(),
        client: std::sync::OnceLock::new(),
        record_attempt: None,
    })
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

    // Try to write the full output to a secure temp file so the agent can
    // search it.  Fail closed: if the directory cannot be created or the
    // write fails, fall back to the inline truncated result.
    let write_result = match bash_temp_output_dir() {
        Ok(dir) => {
            let file_name = format!("bash_output_{}.txt", uuid::Uuid::new_v4());
            let tmp_path = dir.join(&file_name);
            std::fs::write(&tmp_path, output).map(|()| tmp_path)
        },
        Err(e) => {
            debug!("Failed to create secure Bash temp dir: {e}; fall back to inline truncation");
            Err(e)
        },
    };

    match write_result {
        Ok(tmp_path) => {
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
        },
        Err(e) => {
            // Could not write — fall back to a truncated inline result.
            debug!("Failed to write overflow output to temp file: {e}");

            let half = BASH_OUTPUT_MAX_BYTES / 2;
            let head_end = output.floor_char_boundary(half);
            let tail_start = output.ceil_char_boundary(total_bytes - half);
            let (head, _) = output.split_at(head_end);
            let (_, tail) = output.split_at(tail_start);
            format!(
                "[Output too long — {total_bytes} bytes, {total_lines} lines. \
                 The command was too verbose; reformulate with less output \
                 (e.g. pipe through `head`, `tail`, or `grep`).]\n\n\
                 --- first ~{half} bytes ---\n{head}\n\n\
                 --- last ~{half} bytes ---\n{tail}\n{footer}",
            )
        },
    }
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
