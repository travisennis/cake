use serde::Deserialize;
use std::path::{Path, PathBuf};
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
use crate::config::toolbox::ToolboxProcessGuard;
use crate::session_telemetry::{CompensationEventTelemetry, CompensationKind};
use crate::time_format::format_seconds_tenths;

#[cfg(test)]
use crate::config::settings::DEFAULT_BASH_OUTPUT_MAX_BYTES;

/// Maximum number of null bytes or control characters (excluding common whitespace)
/// allowed before considering output as binary.
const BINARY_NULL_BYTE_THRESHOLD: usize = 8;

/// Non-printable character ratio threshold (30%).
/// When more than this percentage of bytes are non-printable (excluding
/// common whitespace and high bytes), the data is considered binary.
const BINARY_RATIO_THRESHOLD_PERCENT: usize = 30;

/// Size of one read chunk captured from a Bash child pipe.
const READ_CHUNK_BYTES: usize = 8192;
const BYTES_PER_KIB: u128 = 1024;
const TENTHS_PER_KIB: u128 = 10;
const EXIT_ZERO_STDERR_WARNING: &str = "[stderr output present despite exit 0]";
const EMPTY_SEARCH_NO_MATCH_ANNOTATION: &str = "(no matches)";

/// Maximum number of bytes the Bash tool will return inline (the compiled
/// default for `[limits] bash_output_max_bytes`).
/// Output exceeding this limit is written to a temporary file and the agent
/// receives a truncated message with a path to the full output.
///
/// Test-only: production reads the configured budget from
/// `context.limits.bash_output_max_bytes`.
#[cfg(test)]
pub(super) const BASH_OUTPUT_MAX_BYTES: usize = DEFAULT_BASH_OUTPUT_MAX_BYTES as usize;

/// Floor for the model-supplied Bash timeout in seconds. A `0` timeout would
/// fail instantly, so requests below the floor are raised to it.
const BASH_TIMEOUT_MIN_SECS: u64 = 1;

/// Ceiling for the model-supplied Bash timeout in seconds. Larger values are
/// capped so a runaway command cannot pin the process for an unbounded time.
const BASH_TIMEOUT_MAX_SECS: u64 = 600;

/// Bounded grace period granted to a Bash child after `SIGTERM` before the
/// process group is force-killed on timeout. Long enough for a well-behaved
/// child to run a cleanup handler (temp files, sockets, locks), short enough
/// that total termination time stays bounded by the configured timeout plus
/// this grace.
const TERMINATE_GRACE_PERIOD: Duration = Duration::from_secs(3);

/// Bound for reaping the direct child after the forceful `SIGKILL`.
///
/// `SIGKILL` cannot be blocked, but a child parked in uninterruptible I/O (a
/// stuck NFS mount, a wedged block device) is not reaped until that I/O
/// returns, so the reap is bounded and a timed-out Bash call cannot outlive
/// its timeout. The armed `ToolboxProcessGuard` still delivers the kill.
const TERMINATE_REAP_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum characters of judge-provided message text allowed into the agent
/// loop, so a verbose or compromised judge cannot flood the model-visible
/// tool error or output.
const JUDGE_MESSAGE_MAX_CHARS: usize = 1000;

/// Arguments for bash execution, including the sandbox policy
struct BashExecutionArgs {
    command: String,
    timeout: u64,
    policy: super::sandbox::SandboxPolicy,
    /// The model's raw working-directory request, unresolved. [`parse_bash_call`]
    /// resolves and validates it against the invocation workspace and the
    /// sandbox's grants; the resolved directory travels to the executor as an
    /// explicit argument instead of being written back into this field.
    cwd: Option<PathBuf>,
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
            cwd: Option<String>,
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
            cwd: args.cwd.map(PathBuf::from),
            reason: args.reason,
        })
    }
}

/// Resolve and validate the optional per-call Bash working directory.
///
/// Relative paths are rooted at the invocation directory. A directory is
/// admitted when the sandbox grants it, when it is inside the invocation
/// workspace, or when the policy applies no sandbox at all. Canonicalization
/// happens before the check, so a symlink cannot escape every grant.
///
/// Selecting a directory grants the command nothing: the sandbox still governs
/// every access from wherever the command starts.
fn resolve_bash_cwd(
    context: &super::ToolContext,
    config: &super::sandbox::SandboxConfig,
    requested: Option<&Path>,
) -> Result<PathBuf, String> {
    let Some(requested) = requested else {
        return Ok(context.cwd.clone());
    };

    let candidate = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        context.cwd.join(requested)
    };
    let workspace = context.cwd.canonicalize().map_err(|e| {
        format!(
            "Invalid Bash working directory '{}': {e}",
            context.cwd.display()
        )
    })?;
    let canonical = candidate.canonicalize().map_err(|e| {
        format!(
            "Invalid Bash working directory '{}': path not found or not accessible: {e}",
            requested.display()
        )
    })?;

    if !canonical.is_dir() {
        return Err(format!(
            "Invalid Bash working directory '{}': path is not a directory",
            requested.display()
        ));
    }
    // `DangerFullAccess` applies no sandbox, so a grant check would be stricter
    // than the policy in force.
    let admitted = context.sandbox_policy == super::sandbox::SandboxPolicy::DangerFullAccess
        || canonical.starts_with(&workspace)
        || config.is_path_selectable(&canonical);
    if !admitted {
        return Err(format!(
            "Invalid Bash working directory '{}': path is outside the invocation workspace '{}' \
             and outside every granted directory. A cwd must be the invocation workspace, a \
             directory granted with `--add-dir` or `[sandbox]` in settings, or a skill directory.",
            requested.display(),
            workspace.display()
        ));
    }

    Ok(canonical)
}

/// Parse one Bash call, build the sandbox configuration once, and resolve the
/// effective working directory.
///
/// Parsing and resolution happen together, once, so the child process, the
/// command-safety judge request, the repository digest, and the sandbox-denial
/// scan all receive the same validated directory and the same grants, and no
/// caller can reach the executor with an unresolved request.
fn parse_bash_call(
    context: &super::ToolContext,
    arguments: &str,
) -> Result<(BashExecutionArgs, PathBuf, super::sandbox::SandboxConfig), String> {
    let args = BashExecutionArgs::from_json(arguments, context.sandbox_policy)?;
    let config = super::sandbox::SandboxConfig::build(context);
    let cwd = resolve_bash_cwd(context, &config, args.cwd.as_deref())?;
    Ok((args, cwd, config))
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
                "cwd": {
                    "type": "string",
                    "description": "Optional working directory for this command. Relative paths resolve from the invocation working directory. The directory must be the invocation working directory or a directory the sandbox already grants (`--add-dir`, settings, or a skill directory)."
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
///
/// `output` is the combined model-visible stream; `stderr` is kept separate so
/// the sandbox-initialization marker cannot be confused with command stdout.
fn is_sandbox_violation(sandbox_applied: bool, success: bool, output: &str, stderr: &str) -> bool {
    if !sandbox_applied || success {
        return false;
    }

    if is_sandbox_initialization_failure(sandbox_applied, stderr) {
        return false;
    }

    output.contains("Operation not permitted")
        || output.contains("os error 1")
        || (output.contains("Permission denied") && output.contains("sandbox"))
}

/// The filesystem operation a path token in a shell command needed, inferred
/// from the command text. Executable command tokens (the leading command or a
/// bare word resolved via `PATH`) are `Execute`; file and directory arguments
/// are `Read` or `Write`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SandboxPathOperation {
    Execute,
    Read,
    Write,
}

impl SandboxPathOperation {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Execute => "execute",
            Self::Read => "read",
            Self::Write => "write",
        }
    }
}

/// A path referenced by a shell command, with the operation the command likely
/// needed for it. Write targets are allowed to be absent because commands such
/// as `touch` and shell redirections commonly create them.
#[derive(Debug)]
struct SandboxPathRef {
    path: PathBuf,
    operation: SandboxPathOperation,
}

impl SandboxPathRef {
    fn permission_label(&self) -> String {
        format!(
            "sandbox: {} {}",
            self.operation.as_str(),
            self.path.display()
        )
    }
}

/// Scan a failed sandboxed command for path tokens that fall outside the
/// sandbox's allowed directories. This names the missing grant instead of
/// echoing the generic "Operation not permitted". Existing read/execute paths
/// are considered, while write targets may be absent because the denied
/// operation itself is often a create. Only path-like tokens are considered,
/// avoiding false positives on shell words, flags, and operators. A bare word
/// resolves via `PATH` as an executable only in command position (the leading
/// word, or the word after a command separator); bare words in argument
/// position resolve relative to `cwd` as a file path.
struct SandboxScanState<'a> {
    expect_command: bool,
    current_command: &'a str,
    pending_operation: Option<SandboxPathOperation>,
}

impl SandboxScanState<'_> {
    const fn new() -> Self {
        Self {
            expect_command: true,
            current_command: "",
            pending_operation: None,
        }
    }

    const fn reset_command(&mut self) {
        self.expect_command = true;
        self.current_command = "";
        self.pending_operation = None;
    }
}

fn denied_path_refs_in_command(
    command: &str,
    cwd: &Path,
    config: &super::sandbox::SandboxConfig,
) -> Vec<SandboxPathRef> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let mut refs: Vec<SandboxPathRef> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut state = SandboxScanState::new();
    let tokens: Vec<&str> = command.split_whitespace().collect();

    for (index, raw) in tokens.iter().copied().enumerate() {
        let last_argument = is_last_command_argument(&tokens, index);
        let Some(resolved) = scan_sandbox_token(raw, &home, cwd, &mut state, last_argument) else {
            continue;
        };
        if seen.insert((resolved.path.clone(), resolved.operation)) {
            refs.push(resolved);
        }
    }

    refs.into_iter()
        .filter(|r| match r.operation {
            SandboxPathOperation::Write => !config.is_path_writable(&r.path),
            SandboxPathOperation::Execute | SandboxPathOperation::Read => {
                !config.is_path_allowed(&r.path)
            },
        })
        .collect()
}

/// Preserve the existing model-facing path-detail helper while the Bash
/// executor carries structured references for completion audit labels.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "retained for focused path-detail unit tests")
)]
pub(super) fn denied_paths_in_command(
    command: &str,
    cwd: &Path,
    config: &super::sandbox::SandboxConfig,
) -> Vec<String> {
    denied_path_refs_in_command(command, cwd, config)
        .iter()
        .map(format_path_ref)
        .collect()
}

fn scan_sandbox_token<'a>(
    raw: &'a str,
    home: &Path,
    cwd: &Path,
    state: &mut SandboxScanState<'a>,
    last_argument: bool,
) -> Option<SandboxPathRef> {
    let token = strip_shell_quotes(raw);
    if token.is_empty() {
        return None;
    }
    if let Some(target) = write_redirection_target(token) {
        return scan_write_redirection(target, home, cwd, state);
    }
    if is_command_separator(token) {
        state.reset_command();
        return None;
    }
    if is_shell_noise(token) {
        return None;
    }

    let was_command = state.expect_command;
    let operation = state.pending_operation.take().unwrap_or_else(|| {
        if was_command {
            SandboxPathOperation::Execute
        } else {
            command_argument_operation(state.current_command, last_argument)
        }
    });
    let resolved = resolve_path_token(token, home, cwd, was_command, operation);
    if was_command {
        state.current_command = command_name(token);
        state.expect_command = false;
    }
    resolved
}

fn scan_write_redirection<'a>(
    target: &'a str,
    home: &Path,
    cwd: &Path,
    state: &mut SandboxScanState<'a>,
) -> Option<SandboxPathRef> {
    if target.is_empty() {
        state.pending_operation = Some(SandboxPathOperation::Write);
        return None;
    }
    resolve_path_token(target, home, cwd, false, SandboxPathOperation::Write)
}

/// Format a denied path reference as a bullet line naming the operation.
fn format_path_ref(r: &SandboxPathRef) -> String {
    format!("  - {} ({})", r.path.display(), r.operation.as_str())
}

/// Remove a surrounding layer of shell quotes from a token.
fn strip_shell_quotes(token: &str) -> &str {
    token.trim_matches(|c| matches!(c, '"' | '\'' | '`'))
}

/// Shell tokens that are never path references: operators, redirections,
/// variable expansions, and flags.
fn is_shell_noise(token: &str) -> bool {
    if token.starts_with('-') || token.starts_with('$') {
        return true;
    }
    if token.contains('>') || token.contains('<') || token.contains('=') {
        return true;
    }
    is_command_separator(token) || matches!(token, "!" | "[" | "]")
}

/// Shell tokens that end one command and start another. The next bare word is
/// a command resolved via `PATH` rather than a file argument.
fn is_command_separator(token: &str) -> bool {
    matches!(
        token,
        "|" | "||"
            | "&&"
            | "&"
            | ";"
            | ";;"
            | "("
            | ")"
            | "{"
            | "}"
            | "then"
            | "do"
            | "else"
            | "fi"
            | "done"
    )
}

/// Resolve a single command token to an on-disk path and its likely operation.
///
/// Bare words resolve via `PATH` as an executable only in command position
/// (`in_command_position`); otherwise they are file arguments relative to
/// `cwd`, so an argument named like a `PATH` executable stays a file read.
fn resolve_path_token(
    token: &str,
    home: &Path,
    cwd: &Path,
    in_command_position: bool,
    operation: SandboxPathOperation,
) -> Option<SandboxPathRef> {
    if let Some(rest) = token.strip_prefix("~/") {
        return existing_ref(home.join(rest), operation);
    }
    if token == "~" {
        return existing_ref(home.to_path_buf(), operation);
    }
    if token.starts_with('/') {
        return existing_ref(PathBuf::from(token), operation);
    }
    if token.contains('/') {
        return existing_ref(cwd.join(token), operation);
    }
    // Bare word: a command name resolves via `PATH` as an executable; otherwise
    // fall back to a path relative to the working directory as a file argument.
    if in_command_position && let Some(found) = lookup_executable_in_path(token) {
        return Some(SandboxPathRef {
            path: found,
            operation: SandboxPathOperation::Execute,
        });
    }
    existing_ref(cwd.join(token), operation)
}

/// Wrap `path` as a reference when it exists, or when it is a write target that
/// may be created by the command. The operation is supplied by shell position
/// and command semantics rather than by the target file's mode.
fn existing_ref(path: PathBuf, operation: SandboxPathOperation) -> Option<SandboxPathRef> {
    if !path.exists() && operation != SandboxPathOperation::Write {
        return None;
    }
    Some(SandboxPathRef { path, operation })
}

/// Infer whether ordinary arguments to a command are likely read or write
/// targets. This is diagnostic metadata only; the OS sandbox remains the
/// enforcement boundary and this intentionally does not try to parse shell.
fn command_argument_operation(command: &str, last_argument: bool) -> SandboxPathOperation {
    match command {
        "cp" | "install" | "ln" | "mv" => {
            if last_argument {
                SandboxPathOperation::Write
            } else {
                SandboxPathOperation::Read
            }
        },
        "chmod" | "chown" | "chgrp" | "mkdir" | "mkfifo" | "mknod" | "rm" | "rmdir" | "shred"
        | "tee" | "touch" | "truncate" | "unlink" => SandboxPathOperation::Write,
        _ => SandboxPathOperation::Read,
    }
}

/// Return whether no later token is an ordinary argument in this shell command.
/// Redirection operators and their targets are skipped because they are handled
/// separately as write references.
fn is_last_command_argument(tokens: &[&str], index: usize) -> bool {
    let mut skip_redirection_target = false;
    for raw in tokens.iter().skip(index + 1) {
        let token = strip_shell_quotes(raw);
        if skip_redirection_target {
            skip_redirection_target = false;
            continue;
        }
        if let Some(target) = write_redirection_target(token) {
            skip_redirection_target = target.is_empty();
            continue;
        }
        if is_command_separator(token) {
            return true;
        }
        if is_shell_noise(token) {
            continue;
        }
        return false;
    }
    true
}

/// Return a write redirection's target when `token` is a standalone or
/// descriptor-prefixed redirection. A non-empty target is returned directly;
/// an empty target means the next shell token is the target.
fn write_redirection_target(token: &str) -> Option<&str> {
    let index = token.find('>')?;
    let prefix = token.get(..index)?;
    if !(prefix.is_empty() || prefix == "&" || prefix.chars().all(|c| c.is_ascii_digit())) {
        return None;
    }
    let rest = token.get(index + 1..)?;
    let target = rest.strip_prefix('>').unwrap_or(rest);
    (!target.starts_with('&')).then_some(target)
}

/// Return the executable-like name used for command-specific argument
/// inference, without interpreting shell quoting or expansions.
fn command_name(token: &str) -> &str {
    token.rsplit('/').next().unwrap_or(token)
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_file() && std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(_path: &Path) -> bool {
    false
}

/// Resolve a bare command name to its executable location via `PATH`.
fn lookup_executable_in_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).find_map(|dir| {
            let full = dir.join(name);
            is_executable_file(&full).then_some(full)
        })
    })
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

/// Return the captured output for a sandbox initialization failure, if any.
///
/// The caller checks this before binary output so a fail-closed setup error
/// cannot be turned into an ordinary binary artifact response.
fn sandbox_initialization_output<'a>(
    sandbox_applied: bool,
    stderr: &str,
    data: &'a [u8],
) -> Option<std::borrow::Cow<'a, str>> {
    is_sandbox_initialization_failure(sandbox_applied, stderr)
        .then(|| String::from_utf8_lossy(data))
}

fn sandbox_initialization_tool_error(
    output: &str,
    judge_events: Vec<CompensationEventTelemetry>,
) -> super::ToolError {
    judge_tool_error(
        judge_events,
        format!(
            "{}\n\n\
            macOS sandbox unavailable: sandbox-exec could not apply a sandbox profile, \
            so the requested command did not run. This commonly happens when cake is \
            itself running inside another Seatbelt sandbox. Run cake with \
            --sandbox danger-full-access (or set CAKE_SANDBOX=off) to run Bash \
            commands without filesystem sandboxing.",
            output.trim_end()
        ),
    )
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

/// Send `signal` to every process in the process group `pgid` (Unix).
///
/// The child is spawned with `process_group(0)`, so its process group ID
/// equals its PID and a negative PID targets the whole group. The group id is
/// passed in rather than read from the child because [`Child::id`] returns
/// `None` once the child has been reaped, and the group must still be
/// signalled after the direct child is gone.
#[cfg(unix)]
fn signal_process_group(pgid: Option<u32>, signal: libc::c_int) {
    if let Some(pid) = pgid {
        // SAFETY: the PID was read from `Child::id()` while the child was
        // alive, and its process group ID equals its PID because
        // `process_group(0)` is set before spawning. A negative PID in
        // `kill(2)` targets every process in that group.
        unsafe {
            libc::kill(-(pid as libc::pid_t), signal);
        }
    }
}

/// Immediately force-kill the child's process group, rejecting the
/// cooperative phase. Used by the read-cap path, where the goal is to stop a
/// runaway producer as fast as possible.
#[cfg(unix)]
fn terminate_process_group(child: &Child) {
    signal_process_group(child.id(), libc::SIGKILL);
}

#[cfg(not(unix))]
fn terminate_process_group(child: &mut Child) {
    let _ = child.start_kill();
}

/// Terminate the child's process group cooperatively, then forcefully.
///
/// `SIGTERM` is sent to the whole group so a well-behaved child can run its
/// cleanup handler. After waiting up to `grace` for the direct child to exit,
/// `SIGKILL` is sent to the same group --- reaching any descendant that
/// ignored the cooperative signal, including when the direct child itself
/// exited during the grace window --- and the child is reaped under
/// [`TERMINATE_REAP_TIMEOUT`]. Both phases are bounded.
#[cfg(unix)]
async fn terminate_process_group_gracefully(child: &mut Child, grace: Duration) {
    // Capture the group id before waiting: `Child::id()` is `None` once the
    // child has been reaped, so resolving the group after the cooperative
    // window would silently skip the forceful phase whenever the direct child
    // exits on `SIGTERM` and leaves a descendant behind.
    let pgid = child.id();
    signal_process_group(pgid, libc::SIGTERM);
    // Cooperative phase: a bounded window for the child to exit on its own.
    // The status is discarded here whether the child exited or the window
    // lapsed; a survivor is force-killed below.
    drop(timeout(grace, child.wait()).await);
    // Forceful phase: kill the captured group, so a descendant that ignored
    // SIGTERM does not survive the direct child that did not.
    signal_process_group(pgid, libc::SIGKILL);
    // Reap the child under a bound. Its status is intentionally discarded:
    // the timed-out run reports the timeout error, not an exit code.
    drop(timeout(TERMINATE_REAP_TIMEOUT, child.wait()).await);
}

#[cfg(not(unix))]
async fn terminate_process_group_gracefully(child: &mut Child, _grace: Duration) {
    // Non-Unix has no cooperative signal: force-kill the direct child now,
    // then reap under the same bound as the Unix path.
    drop(child.start_kill());
    drop(timeout(TERMINATE_REAP_TIMEOUT, child.wait()).await);
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
    let (args, cwd, sandbox_config) = parse_bash_call(context, arguments)?;
    Box::pin(execute_bash_with_args(
        context,
        args,
        cwd,
        &sandbox_config,
        call_id,
    ))
    .await
}

/// One prepared Bash invocation: the configured child command plus the
/// platform-sandbox guard that must stay alive until the child has finished,
/// so sandbox resources (e.g., macOS temp profile files) are cleaned up
/// deterministically.
struct PreparedBashCommand {
    command: Command,
    sandbox_applied: bool,
    _sandbox_guard: Option<super::sandbox::SandboxGuard>,
}

/// Remove inherited Git variables that can redirect commands away from their
/// working directory. The caller must run this after sandbox application because
/// macOS replaces the command with a `sandbox-exec` wrapper.
fn scrub_ambient_git_environment(command: &mut Command) {
    for var in crate::config::git::AMBIENT_ENV_VARS {
        command.env_remove(var);
    }
}

/// Build the bash child command and apply the sandbox strategy required by
/// the policy. Sandbox-setup failures carry the preflight's telemetry events,
/// so an allow/warn verdict stays observable even when the command never runs.
fn prepare_bash_command(
    args: &BashExecutionArgs,
    cwd: &Path,
    sandbox_config: &super::sandbox::SandboxConfig,
    judge_events: &[CompensationEventTelemetry],
) -> Result<PreparedBashCommand, super::ToolError> {
    let use_sandbox = args.policy != super::sandbox::SandboxPolicy::DangerFullAccess;

    // Create command with proper stdio configuration
    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(&args.command)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    // Apply sandbox if enabled.
    let sandbox_guard = if use_sandbox {
        if let Some(strategy) = super::sandbox::detect_platform()
            .map_err(|e| judge_tool_error(judge_events.to_vec(), e))?
        {
            Some(
                strategy
                    .apply(&mut command, sandbox_config)
                    .map_err(|e| judge_tool_error(judge_events.to_vec(), e))?,
            )
        } else {
            None
        }
    } else {
        tracing::debug!("Sandbox disabled; running without filesystem restrictions");
        None
    };

    // Sandbox application may replace the Command (macOS wraps it with
    // sandbox-exec), so scrub after application. These variables can redirect
    // Git away from the Bash working directory and must not escape into the
    // child, regardless of the selected sandbox policy.
    scrub_ambient_git_environment(&mut command);

    Ok(PreparedBashCommand {
        command,
        sandbox_applied: sandbox_guard.is_some(),
        _sandbox_guard: sandbox_guard,
    })
}

/// A completed Bash child run before output formatting: the combined stream
/// capture, the stderr-only capture, whether the read cap cut the capture,
/// and the reaped exit status (`None` when the child was killed and could not
/// report a code).
struct ChildRun {
    buf: Vec<u8>,
    stderr_buf: Vec<u8>,
    hit_cap: bool,
    status: Option<std::process::ExitStatus>,
}

/// Own a Bash lifecycle task until its result is consumed. Dropping this task
/// or its `finish` future aborts the worker, which then drops the child's
/// process-group guard and kills descendants as well as the direct child.
struct BashChildTask(tokio::task::JoinHandle<Result<ChildRun, String>>);

impl BashChildTask {
    async fn finish(mut self) -> Result<ChildRun, String> {
        match (&mut self.0).await {
            Ok(result) => result,
            // A panic in capture or reaping used to unwind the Bash call.
            // Preserve that failure instead of turning it into a tool error.
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(error) => Err(format!("Bash lifecycle task failed: {error}")),
        }
    }
}

impl Drop for BashChildTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Spawn the prepared command and drive its full lifecycle under the
/// configured timeout: concurrent stream capture, killing the process group
/// when the cap cuts the capture or the timeout fires, and reaping the child.
/// The returned error carries only the model-visible message; the caller
/// attaches the preflight's telemetry events.
async fn run_bash_child(
    mut command: Command,
    timeout_secs: u64,
    read_cap: Option<usize>,
    initial_capacity: usize,
) -> Result<ChildRun, String> {
    // Spawn the command with piped stdout/stderr for streaming
    let mut child = command
        .spawn()
        .map_err(|e| format!("Failed to spawn command: {e}"))?;

    // RAII guard: kills the whole process group on drop unless defused.
    // This ensures Ctrl-C or any other future cancellation terminates
    // descendant processes, not just the direct child.
    let mut guard = ToolboxProcessGuard::new(child.id());

    let mut stdout = child.stdout.take().ok_or("Failed to capture stdout")?;
    let mut stderr = child.stderr.take().ok_or("Failed to capture stderr")?;

    // The configured timeout covers the full lifecycle: reading both
    // streams, optionally killing the process group on cap, and reaping
    // the child.  This prevents hangs when a command closes both captured
    // streams while continuing to run — the wait is bounded by the same
    // timeout as the read loop.
    let lifecycle_result = timeout(Duration::from_secs(timeout_secs), async {
        let captured = Box::pin(capture_streams(
            &mut stdout,
            &mut stderr,
            read_cap,
            initial_capacity,
        ))
        .await;
        let CapturedStreams {
            buf,
            stderr_buf,
            hit_cap,
        } = captured?;

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
        Ok(Err(e)) => return Err(e),
        Err(_) => {
            // Timed out: terminate the process group cooperatively, then
            // forcefully after the bounded grace period, so a well-behaved
            // child can run its cleanup handler before being killed.
            terminate_process_group_gracefully(&mut child, TERMINATE_GRACE_PERIOD).await;
            return Err(format!("Command timed out after {timeout_secs} seconds"));
        },
    };

    // Normal completion (or hit_cap with group already killed by
    // terminate_process_group above): defuse the guard so it does not
    // send a harmless-but-unnecessary SIGKILL to the reaped group.
    guard.defuse();

    Ok(ChildRun {
        buf,
        stderr_buf,
        hit_cap,
        status,
    })
}

/// The two captures of one Bash run: the interleaved combined view shown to
/// the model and the stderr-only view used for sandbox diagnostics.
struct CapturedStreams {
    buf: Vec<u8>,
    stderr_buf: Vec<u8>,
    hit_cap: bool,
}

/// Read both pipes concurrently, interleaved, until both close or the read
/// cap cuts the combined capture.
async fn capture_streams(
    stdout: &mut tokio::process::ChildStdout,
    stderr: &mut tokio::process::ChildStderr,
    read_cap: Option<usize>,
    initial_capacity: usize,
) -> Result<CapturedStreams, String> {
    // Bound the initial allocation by the read cap: the read loop never
    // holds more than `read_cap` bytes, so a large configured inline cap
    // alone must not trigger a huge upfront allocation.
    let mut buf = Vec::with_capacity(initial_capacity);
    let mut stderr_buf = Vec::new();
    let mut tmp_stdout = [0u8; READ_CHUNK_BYTES];
    let mut tmp_stderr = [0u8; READ_CHUNK_BYTES];
    let mut hit_cap = false;

    loop {
        tokio::select! {
            n = stdout.read(&mut tmp_stdout) => {
                let Some(n) = checked_chunk_len(n, "stdout")? else {
                    // stdout closed — read remaining stderr
                    if drain_to_eof(
                        &mut *stderr,
                        "stderr",
                        &mut buf,
                        Some(&mut stderr_buf),
                        &mut tmp_stderr,
                        read_cap,
                    )
                    .await?
                    {
                        hit_cap = true;
                    }
                    break;
                };
                let take = take_chunk_within_cap(&mut buf, None, &tmp_stdout[..n], read_cap);
                if take < n { hit_cap = true; break; }
            }
            n = stderr.read(&mut tmp_stderr) => {
                let Some(n) = checked_chunk_len(n, "stderr")? else {
                    // stderr closed — read remaining stdout
                    if drain_to_eof(
                        &mut *stdout,
                        "stdout",
                        &mut buf,
                        None,
                        &mut tmp_stdout,
                        read_cap,
                    )
                    .await?
                    {
                        hit_cap = true;
                    }
                    break;
                };
                let take = take_chunk_within_cap(
                    &mut buf,
                    Some(&mut stderr_buf),
                    &tmp_stderr[..n],
                    read_cap,
                );
                if take < n { hit_cap = true; break; }
            }
        }
    }

    Ok(CapturedStreams {
        buf,
        stderr_buf,
        hit_cap,
    })
}

/// Map one pipe read result to its byte count: a clean EOF (0 bytes) closes
/// the pipe, an IO error fails the capture with the model-visible message.
fn checked_chunk_len(
    read: std::io::Result<usize>,
    stream_name: &'static str,
) -> Result<Option<usize>, String> {
    match read {
        Ok(0) => Ok(None),
        Ok(n) => Ok(Some(n)),
        Err(e) => Err(format!("{stream_name} read error: {e}")),
    }
}

/// Append one chunk to the combined capture — and to the stderr-only capture
/// when reading stderr — keeping the buffer within the configured `read_cap`
/// (see [`take_within_cap`]). Returns the number of bytes kept; fewer than
/// the chunk length means the cap was hit.
fn take_chunk_within_cap(
    buf: &mut Vec<u8>,
    stderr_buf: Option<&mut Vec<u8>>,
    chunk: &[u8],
    read_cap: Option<usize>,
) -> usize {
    let take = take_within_cap(chunk.len(), read_cap, buf.len());
    buf.extend_from_slice(&chunk[..take]);
    if let Some(stderr_buf) = stderr_buf {
        stderr_buf.extend_from_slice(&chunk[..take]);
    }
    take
}

/// After the other pipe closed, read this one to EOF, appending kept bytes to
/// the combined capture (and to the stderr-only capture when draining stderr).
/// Returns whether the cap cut a chunk, which stops the caller's read loop.
async fn drain_to_eof<R: tokio::io::AsyncRead + Unpin>(
    stream: &mut R,
    stream_name: &'static str,
    buf: &mut Vec<u8>,
    mut stderr_buf: Option<&mut Vec<u8>>,
    tmp: &mut [u8],
    read_cap: Option<usize>,
) -> Result<bool, String> {
    loop {
        let n = stream
            .read(tmp)
            .await
            .map_err(|e| format!("{stream_name} read error: {e}"))?;
        if n == 0 {
            return Ok(false);
        }
        // Reborrow the optional stderr capture each iteration so the drain
        // loop keeps appending without giving up ownership.
        #[expect(
            clippy::option_as_ref_deref,
            reason = "explicit reborrow of Option<&mut Vec<u8>>"
        )]
        let take = take_chunk_within_cap(
            buf,
            stderr_buf.as_mut().map(|capture| &mut **capture),
            &tmp[..n],
            read_cap,
        );
        if take < n {
            return Ok(true);
        }
    }
}

/// Collect the denied-path references to append to a `[Sandbox restriction]`
/// notice and to `task_complete.permission_denials`. Only scans the command
/// when the run looks like a sandbox denial; otherwise it returns an empty
/// list. Kept off the hot path and out of `execute_bash_with_args` so that
/// function's cyclomatic complexity stays within the gate.
fn sandbox_denials(
    command: &str,
    cwd: &Path,
    config: &super::sandbox::SandboxConfig,
    sandbox_applied: bool,
    success: bool,
    output: &str,
    stderr: &str,
) -> Vec<SandboxPathRef> {
    if !is_sandbox_violation(sandbox_applied, success, output, stderr) {
        return Vec::new();
    }
    denied_path_refs_in_command(command, cwd, config)
}

/// Select the model-visible text form of a completed run's combined output:
/// empty stays empty, a read-cap cut gets a truncation marker, and a failed
/// sandboxed run that looks like a denial gets the sandbox-restriction notice.
fn compose_text_output(
    output: &str,
    stderr: &str,
    hit_cap: bool,
    read_cap: Option<usize>,
    success: bool,
    sandbox_applied: bool,
    denials: &[String],
) -> String {
    if output.is_empty() {
        String::new()
    } else if hit_cap {
        // `hit_cap` is only set when a read cap exists.
        let cap = read_cap.unwrap_or_default();
        format!("{output}\n[... output truncated at {cap} bytes ...]")
    } else if success {
        output.to_owned()
    } else if is_sandbox_violation(sandbox_applied, success, output, stderr) {
        format!(
            "{output}\n\n\
            [Sandbox restriction]: This command was blocked by the filesystem sandbox. \
            The sandbox restricts file access to the project directory and standard system paths.\
            {}\n\
            Do NOT retry with different workarounds — the restriction is intentional. \
            Instead, inform the user that this command requires access outside the sandbox \
            and suggest they run it directly in their terminal.",
            sandbox_denial_detail(denials)
        )
    } else {
        output.to_owned()
    }
}

/// Build the denial-detail paragraph appended to a `[Sandbox restriction]`
/// notice. When one or more denied paths were found, the missing grant is
/// named so the model reports the specific path instead of guessing.
fn sandbox_denial_detail(denials: &[String]) -> String {
    if denials.is_empty() {
        return String::new();
    }
    format!(
        "\n\nThe following path(s) are outside the allowed directories:\n{}\n\
        Add each path (or its parent directory) to `directories` in `.cake/settings.toml` \
        (or pass it with `--add-dir`) to grant that access.",
        denials.join("\n")
    )
}

async fn execute_bash_with_args(
    context: &super::ToolContext,
    args: BashExecutionArgs,
    cwd: PathBuf,
    sandbox_config: &super::sandbox::SandboxConfig,
    call_id: Option<String>,
) -> Result<super::ToolResult, super::ToolError> {
    // Command-safety preflight: the LLM judge is the only non-sandbox command
    // gate. A block prevents spawn and returns the judge's message as the tool
    // error; a warn prepends guidance to the output; a judge failure fails
    // closed (blocks) with an explanation. Judge decisions and denials are
    // recorded as telemetry compensation events.
    let preflight = bash_judge_preflight(context, &args, &cwd, call_id).await?;
    let judge_warnings = preflight.warnings;
    let judge_events = preflight.compensation_events;

    // Output budgets resolved from `[limits]`; `None` means unlimited.
    let output_max = context.limits.bash_output_max_bytes;
    let read_cap = context.limits.bash_read_cap;

    let start_time = Instant::now();

    let PreparedBashCommand {
        mut command,
        sandbox_applied,
        _sandbox_guard: sandbox_guard,
    } = prepare_bash_command(&args, &cwd, sandbox_config, &judge_events)?;

    // Place the child in its own process group so that SIGKILL to the
    // negative PID kills all descendants, not just the direct child.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }

    // Bound the initial allocation by the read cap: the read loop never
    // holds more than `read_cap` bytes, so a large configured inline cap
    // alone must not trigger a huge upfront allocation.
    let initial_capacity = read_cap
        .zip(output_max)
        .map_or(0, |(read, max)| read.min(max));

    let timeout_secs = args.timeout;
    let child_task = BashChildTask(tokio::spawn(async move {
        let result = run_bash_child(command, timeout_secs, read_cap, initial_capacity).await;
        // The sandbox profile must remain alive until the child is reaped.
        drop(sandbox_guard);
        result
    }));

    let ChildRun {
        buf,
        stderr_buf,
        hit_cap,
        status,
    } = child_task
        .finish()
        .await
        .map_err(|e| judge_tool_error(judge_events.clone(), e))?;

    let elapsed_ms = start_time.elapsed().as_millis();
    let stderr_str = String::from_utf8_lossy(&stderr_buf);
    let success = status
        .as_ref()
        .is_some_and(std::process::ExitStatus::success);
    let exit_code = status.and_then(|s| s.code()).unwrap_or(-1);
    let warn_exit_zero_stderr = should_warn_exit_zero_stderr(success, &stderr_str);
    if let Some(output_str) = sandbox_initialization_output(sandbox_applied, &stderr_str, &buf) {
        return Err(sandbox_initialization_tool_error(&output_str, judge_events));
    }
    if is_binary_data(&buf) {
        // Judge warnings still prepend here: a `warn` verdict ran the
        // command, so its guidance must reach the model even when the
        // output is binary.
        let mut compensation_events = judge_events;
        let spilled = output_max.is_some_and(|max| buf.len() > max);
        push_truncation_event_if(&mut compensation_events, "Bash", hit_cap, spilled);
        let output = handle_binary_output(&buf, exit_code, elapsed_ms, warn_exit_zero_stderr);
        return Ok(super::ToolResult {
            output: prepend_safety_warnings(output, &judge_warnings),
            compensation_events,
            permission_denials: Vec::new(),
        });
    }
    let output_str = String::from_utf8_lossy(&buf);
    let denials = sandbox_denials(
        &args.command,
        &cwd,
        sandbox_config,
        sandbox_applied,
        success,
        &output_str,
        &stderr_str,
    );
    let denial_details = denials.iter().map(format_path_ref).collect::<Vec<_>>();
    let result = compose_text_output(
        &output_str,
        &stderr_str,
        hit_cap,
        read_cap,
        success,
        sandbox_applied,
        &denial_details,
    );
    let permission_denials = denials
        .iter()
        .map(SandboxPathRef::permission_label)
        .collect();

    let result = annotate_empty_search_result(&args.command, result, exit_code, &stderr_str);
    let spilled = output_max.is_some_and(|max| result.len() > max);
    let result = truncate_output(
        &result,
        output_max,
        exit_code,
        elapsed_ms,
        warn_exit_zero_stderr,
    );
    let mut compensation_events = judge_events;
    push_truncation_event_if(&mut compensation_events, "Bash", hit_cap, spilled);

    let output = prepend_safety_warnings(result, &judge_warnings);

    Ok(super::ToolResult {
        output,
        compensation_events,
        permission_denials,
    })
}

/// Bytes of an `n`-byte read chunk to append when `buffered` bytes are already
/// held, so the capture never exceeds a configured `read_cap`. Without a cap
/// the whole chunk is kept; with a cap the chunk is cut at the remaining
/// budget so the buffer holds at most `read_cap` bytes.
fn take_within_cap(n: usize, read_cap: Option<usize>, buffered: usize) -> usize {
    match read_cap {
        Some(cap) if buffered < cap => n.min(cap - buffered),
        Some(_) => 0,
        None => n,
    }
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

/// A preflight for a call the judge never evaluated because it is disabled.
///
/// The bypass event records the escape hatch so it cannot be used silently, and
/// carries the originating call's linkage like every other judge event.
fn bypassed_preflight(raw_call_id: Option<&str>) -> JudgePreflight {
    let event = CompensationEventTelemetry::judge_bypass().with_call_id(raw_call_id);
    JudgePreflight {
        warnings: Vec::new(),
        compensation_events: vec![event],
    }
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
/// observable even when the tool call fails. Each event also carries the
/// one-way digest of the transcript call it judged, so session metrics can
/// pair a call with its outcome by transcript call (#404).
async fn bash_judge_preflight(
    context: &super::ToolContext,
    args: &BashExecutionArgs,
    cwd: &Path,
    call_id: Option<String>,
) -> Result<JudgePreflight, super::ToolError> {
    // The transcript call id links this Bash call to the judge request, the
    // judge attempts, and the recorded events. Borrowed once here: the events
    // digest it at construction and the judge observer only ever holds the
    // digest, so the raw identifier never reaches telemetry.
    let raw_call_id = call_id.as_deref();

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
            raw_call_id,
        ));
    };
    // The emergency bypass short-circuits before any judge setup: a disabled
    // judge must not fail on an unusable model or rubric, because the bypass
    // is the recovery path when judge configuration is broken. The bypass is
    // still recorded so the escape hatch cannot be used silently. The value
    // was captured once when the run's judge context was built, so this path
    // never reads the process-global environment.
    let bypass_env = judge.bypass_env.as_deref();
    if !judge_is_enabled(&judge.settings, bypass_env) {
        return Ok(bypassed_preflight(raw_call_id));
    }

    judge_enabled_preflight(context, args, cwd, call_id, judge, bypass_env).await
}

/// Run the judge once the configuration has passed the empty-command, missing
/// context, and bypass checks.
///
/// Splitting this out of [`bash_judge_preflight`] keeps the fail-closed branch
/// count in one function at the change-risk ratchet's allowed level; the script
/// observation adds no decision point here.
async fn judge_enabled_preflight(
    context: &super::ToolContext,
    args: &BashExecutionArgs,
    cwd: &Path,
    call_id: Option<String>,
    judge: &crate::clients::judge::JudgeContext,
    bypass_env: Option<&str>,
) -> Result<JudgePreflight, super::ToolError> {
    let raw_call_id = call_id.as_deref();
    // Collection runs before judge configuration resolution: a referenced
    // script that cannot be collected is a fail-closed denial on its own, and
    // an unusable judge configuration must not mask that explanation.
    let request = script_judge_request(context, args, cwd, raw_call_id)?;
    let observation_note = script_observation_note(&request);
    let client = judge
        .judge_client()
        .map_err(|e| fail_closed_tool_error(e.class, &e.message, raw_call_id))?;

    let evaluation =
        evaluate_command_observed(client, &judge.settings, request, bypass_env, false).await;
    // Persist finalized attempts as soon as judging completes: an interrupted
    // command (for example Ctrl-C on a hung Bash call) cancels the agent
    // future before the tool result and its compensation events are recorded,
    // so waiting for that path would drop the attempts.
    record_judge_attempts(judge, &evaluation.attempts);
    record_typesafe_shadow(judge, evaluation.shadow.as_ref(), raw_call_id);
    observed_evaluation_to_preflight(evaluation, raw_call_id)
        .map_err(|mut error| {
            if let Some(note) = &observation_note {
                error.message.push_str("\n\n");
                error.message.push_str(note);
            }
            error
        })
        .map(|mut preflight| {
            preflight.warnings.extend(observation_note);
            preflight
        })
}

/// Build the judge request for one Bash call, collecting any directly
/// referenced script as bounded untrusted evidence.
fn script_judge_request(
    context: &super::ToolContext,
    args: &BashExecutionArgs,
    cwd: &Path,
    raw_call_id: Option<&str>,
) -> Result<JudgeRequest, super::ToolError> {
    let mut request =
        JudgeRequest::new(args.command.clone(), cwd.to_path_buf(), args.reason.clone())
            .with_repo_digest(repo_state_digest(cwd))
            .with_call_id(raw_call_id.map(String::from));
    request.script_evidence =
        crate::clients::tools::script_evidence::collect(context, &args.command).map_err(|detail| {
            // The generic unavailable-judge helper would misdescribe this
            // failure: collection happens before any provider call, so the
            // block reuses the fail-closed prefix and keeps its own wording.
            super::ToolError {
                message: format!(
                    "BLOCKED\n\nReferenced script evidence could not be collected, so the command was not executed and the judge was not called. {detail}\nUse an existing readable regular UTF-8 script within the configured Read grants (at most 32 KiB), or inline the intended command for judgment."
                ),
                compensation_events: vec![
                    CompensationEventTelemetry::judge_fail_closed("script_evidence")
                        .with_call_id(raw_call_id),
                ],
            }
        })?;
    Ok(request)
}

/// The notice a call reports when the judge saw a referenced script, naming the
/// observed path without echoing contents.
fn script_observation_note(request: &JudgeRequest) -> Option<String> {
    request.script_evidence.as_ref().map(|evidence| {
        // JSON escaping keeps model-supplied paths from injecting control characters.
        format!(
            "Safety judge inspected referenced script {} (untrusted contents; dependencies and later changes not covered).",
            serde_json::json!(evidence.path)
        )
    })
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

/// Persist one `TypeSafe` observation through the run's telemetry sink, if any.
///
/// The record is emitted under `mode = "shadow"` and `mode = "cascade"` alike:
/// under the cascade the observation is the deciding evidence for a fast
/// approval and the reason for a fallback, so the record carries both. A fast
/// approval is identifiable from it, the `judge_verdict` event, and the absence
/// of a `judge_attempt` for the same call (ADR 034).
///
/// The optional observation adds no decision point to `bash_judge_preflight`:
/// the change-risk ratchet scores any new branch there as a regression, so both
/// `Option` checks live here instead.
fn record_typesafe_shadow(
    judge: &crate::clients::judge::JudgeContext,
    shadow: Option<&crate::clients::typesafe::TypeSafeObservation>,
    call_id: Option<&str>,
) {
    let (Some(observation), Some(sink)) = (shadow, judge.record_attempt.as_ref()) else {
        return;
    };
    sink.record_typesafe(
        crate::session_telemetry::TypeSafeShadowTelemetry {
            elapsed_ms: u64::try_from(observation.elapsed.as_millis()).unwrap_or(u64::MAX),
            model: observation.model.clone(),
            probability: observation.probability,
            call_id: None,
            usage_input_tokens: observation.usage_input_tokens,
            usage_output_tokens: observation.usage_output_tokens,
            failure_class: observation.failure_class.map(str::to_string),
        },
        call_id,
    );
}

fn observed_evaluation_to_preflight(
    evaluation: crate::clients::judge::JudgeEvaluation,
    raw_call_id: Option<&str>,
) -> Result<JudgePreflight, super::ToolError> {
    // The decision latency. A cascade fast approval made no judge call, so the
    // fast leg's elapsed time is the whole decision; it lands in the existing
    // `judge_verdict` latency field rather than a new record field, which is
    // what keeps the cascade measurable without changing the telemetry shape
    // (ADR 034). Every other decision is the cumulative wall time across every
    // attempt, including the backoff waits between them, so the
    // verdict/fail-closed latency reflects the whole judge operation after a
    // bounded recovery.
    let latency_ms = match &evaluation.outcome {
        Ok(JudgeOutcome::FastApproved { elapsed, .. }) => {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        },
        _ => evaluation.attempts.iter().fold(0_u64, |acc, attempt| {
            acc.saturating_add(attempt.total_ms)
                .saturating_add(attempt.retry_delay_ms)
        }),
    };
    match evaluation.outcome {
        Ok(outcome) => judge_preflight_outcome(outcome, latency_ms, raw_call_id),
        Err(error) => {
            let class = error.error_class();
            Err(fail_closed_tool_error(class, error, raw_call_id))
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
    raw_call_id: Option<&str>,
) -> Result<JudgePreflight, super::ToolError> {
    match outcome {
        JudgeOutcome::Bypassed => Ok(bypassed_preflight(raw_call_id)),
        // A fast approval emits the unannotated `allow` event an allow verdict
        // emits, carrying the fast leg's elapsed time as the latency. It is
        // preceded by no `judge_attempt` record, which is what identifies it in
        // telemetry (ADR 034). A fast approval adds no warning: it approved the
        // command as observational, so there is nothing to prepend.
        JudgeOutcome::FastApproved { .. } => Ok(JudgePreflight {
            warnings: Vec::new(),
            compensation_events: vec![
                CompensationEventTelemetry::judge_verdict(
                    JudgeDecision::Allow.as_str(),
                    None,
                    latency_ms,
                    false,
                )
                .with_call_id(raw_call_id),
            ],
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
            )
            .with_call_id(raw_call_id);
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
fn fail_closed_tool_error(
    class: &'static str,
    error: impl std::fmt::Display,
    raw_call_id: Option<&str>,
) -> super::ToolError {
    let event = CompensationEventTelemetry::judge_fail_closed(class).with_call_id(raw_call_id);
    super::ToolError {
        message: fail_closed_message(error),
        compensation_events: vec![event],
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
    let mut context = super::ToolContext::from_current_process();
    context.cwd = cwd.to_path_buf();
    context.judge = judge;
    context.sandbox_policy = super::sandbox::SandboxPolicy::DangerFullAccess;
    let (args, effective_cwd, sandbox_config) = parse_bash_call(&context, arguments)?;
    Box::pin(execute_bash_with_args(
        &context,
        args,
        effective_cwd,
        &sandbox_config,
        None,
    ))
    .await
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
        bypass_env: None,
        models: HashMap::new(),
        client: std::sync::OnceLock::new(),
        record_attempt: None,
    })
}

/// If `output` exceeds `max_bytes`, write the full text to a temporary file
/// and return a summary pointing to that file. Otherwise return the output
/// with the metadata footer appended. `max_bytes` is the `[limits]
/// bash_output_max_bytes` budget; `None` (unlimited) passes everything
/// through. The temp file receives only the raw command output (no footer);
/// the footer is included in the inline summary so it is always visible in
/// the tool response.
pub(super) fn truncate_output(
    output: &str,
    max_bytes: Option<usize>,
    exit_code: i32,
    elapsed_ms: u128,
    warn_exit_zero_stderr: bool,
) -> String {
    let Some(max_bytes) = max_bytes else {
        return append_metadata(output, exit_code, elapsed_ms, warn_exit_zero_stderr);
    };
    if output.len() <= max_bytes {
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
            let preview = max_bytes / 4;
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

            let half = max_bytes / 2;
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

#[cfg(test)]
#[path = "bash_issue_366_tests.rs"]
mod issue_366_tests;

#[cfg(test)]
#[path = "bash_judge_linkage_tests.rs"]
mod judge_linkage_tests;
