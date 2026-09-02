//! User-defined toolbox tool discovery and describe-protocol parsing.
//!
//! Toolbox tools are user-provided executables discovered from toolbox
//! directories (`CAKE_TOOLBOX`, `--toolbox`, the project-local
//! `.cake/tools`, and the default `~/.config/cake/tools`).
//! Each executable implements a two-action protocol selected by the
//! `TOOLBOX_ACTION` environment variable:
//!
//! - `describe`: print the tool's name, description, and parameter schema
//!   to stdout, in either JSON or line-based text format.
//! - `execute`: read arguments from stdin and write output to stdout.
//!
//! This module owns discovery (directory scanning and filtering) and the
//! describe half of the protocol. The execute half lives in
//! `clients::tools::toolbox` alongside the other tool executors.
//!
//! Toolbox tools run as separate processes without cake's OS sandbox; they
//! are user-provided and trusted. They are therefore never offered under
//! the read-only sandbox policy.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tracing::{debug, warn};

use crate::types::ReplaySafety;

/// Prefix applied to every registered toolbox tool name.
pub const TOOLBOX_PREFIX: &str = "tb__";

/// Environment variable holding colon-separated toolbox directories.
pub const TOOLBOX_ENV_VAR: &str = "CAKE_TOOLBOX";

/// Timeout for a `describe` invocation at startup.
const DESCRIBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Cap on describe stdout — far beyond any legitimate schema. Exceeding it
/// marks the tool broken and it is skipped.
const DESCRIBE_MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Cap on captured describe stderr; beyond this stderr is drained but
/// discarded.
const DESCRIBE_MAX_STDERR_BYTES: usize = 10_000;

/// Default `execute` timeout when the describe output declares none.
/// Matches the Bash tool's default.
pub const DEFAULT_EXECUTE_TIMEOUT_SECS: u64 = 60;

/// Communication format detected during `describe` and reused for `execute`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolboxFormat {
    /// Describe output is JSON; execute receives a JSON object on stdin.
    Json,
    /// Describe output is line-based text; execute receives `key=value`
    /// lines on stdin.
    Text,
}

/// An executable discovered in a toolbox directory, before `describe` runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolboxEntry {
    /// File name as discovered (display/debugging only; the describe
    /// output's `name` field is authoritative for registration).
    pub filename: String,
    /// Full path to the executable.
    pub path: PathBuf,
}

/// A toolbox tool with a parsed describe schema, ready for registration.
#[derive(Clone, Debug)]
pub struct ToolboxTool {
    /// Registered name with prefix: `tb__<name>`.
    pub registered_name: String,
    /// Name from the describe output (authoritative, not the filename).
    pub original_name: String,
    /// Path to the executable.
    pub path: PathBuf,
    /// Description from the describe output.
    pub description: String,
    /// Parameter schema as a JSON Schema object.
    pub parameters: serde_json::Value,
    /// Communication format detected during describe.
    pub format: ToolboxFormat,
    /// Execute timeout in seconds (describe `timeout` field, JSON format
    /// only; defaults to [`DEFAULT_EXECUTE_TIMEOUT_SECS`]).
    pub timeout_secs: u64,
    /// Replay declaration from the describe manifest. Missing declarations
    /// default to [`ReplaySafety::Never`].
    pub replay: ReplaySafety,
}

/// Resolve the ordered list of toolbox directories to scan.
///
/// - `env_value` unset: the default directory is scanned.
/// - `env_value` set and non-empty: its colon-separated entries are scanned
///   instead of the default.
/// - `env_value` set to the empty string: the global/default directory is not
///   scanned.
///
/// `extra_dirs` (from `--toolbox`) are appended after the environment
/// directories, and the active project's `.cake/tools` directory is appended
/// after those explicit directories. Earlier directories take precedence for
/// tool name conflicts.
///
/// Relative environment and flag entries are anchored to `base_dir` — the
/// directory Cake was invoked from — so they stay stable even if the process
/// cwd changes later (e.g. via `--worktree`). The project-local directory is
/// anchored to `project_dir`, which is the active worktree when one is used.
pub fn toolbox_directories(
    env_value: Option<&str>,
    extra_dirs: &[PathBuf],
    default_dir: &Path,
    base_dir: &Path,
    project_dir: &Path,
) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = env_value.map_or_else(
        || vec![default_dir.to_path_buf()],
        |value| {
            value
                .split(':')
                .filter(|part| !part.is_empty())
                .map(PathBuf::from)
                .collect()
        },
    );
    dirs.extend(extra_dirs.iter().cloned());
    let mut seen = HashSet::new();
    dirs.into_iter()
        .map(|dir| {
            if dir.is_absolute() {
                dir
            } else {
                base_dir.join(dir)
            }
        })
        .chain(std::iter::once(project_dir.join(".cake").join("tools")))
        // Automatic project-local discovery can duplicate an explicitly
        // configured directory (the documented `CAKE_TOOLBOX=.cake/tools`
        // setup is one example). Avoid running an unsandboxed describe action
        // twice while retaining the first source's precedence.
        .filter(|dir| seen.insert(toolbox_directory_identity(dir)))
        .collect()
}

/// Return a stable identity for an existing directory, including symlinked
/// paths, while retaining missing paths for diagnostics/discovery filtering.
fn toolbox_directory_identity(dir: &Path) -> PathBuf {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

/// Scan the given directories for toolbox executables, in order.
///
/// Within a directory, entries are sorted by filename for deterministic
/// registration order. Hidden files (dot-prefixed), `.md`/`.txt` files,
/// directories, and non-executable files are skipped. Missing or unreadable
/// directories are skipped silently (an absent default directory is the
/// common case).
pub fn discover_toolbox_entries(dirs: &[PathBuf]) -> Vec<ToolboxEntry> {
    let mut entries = Vec::new();
    for dir in dirs {
        let Ok(read_dir) = std::fs::read_dir(dir) else {
            debug!(target: "cake", "toolbox directory not readable, skipping: {}", dir.display());
            continue;
        };
        let mut dir_entries: Vec<ToolboxEntry> = read_dir
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let filename = entry.file_name().to_string_lossy().into_owned();
                let path = entry.path();
                if should_skip_file(&filename, &path) {
                    return None;
                }
                Some(ToolboxEntry { filename, path })
            })
            .collect();
        dir_entries.sort_by(|a, b| a.filename.cmp(&b.filename));
        entries.extend(dir_entries);
    }
    entries
}

/// Apply the discovery filters: hidden files, `.md`/`.txt` extensions,
/// directories, and non-executable files are all skipped.
fn should_skip_file(filename: &str, path: &Path) -> bool {
    if filename.starts_with('.') {
        return true;
    }
    if let Some(ext) = path.extension().and_then(|e| e.to_str())
        && (ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("txt"))
    {
        return true;
    }
    // Follows symlinks, so a symlink to an executable file is accepted.
    let Ok(metadata) = std::fs::metadata(path) else {
        return true;
    };
    if !metadata.is_file() {
        return true;
    }
    !is_executable(&metadata)
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    true
}

/// Discover, describe, and parse all toolbox tools from the given
/// directories.
///
/// Broken tools (describe failure, invalid output, timeout) are skipped
/// with a warning so one bad tool never blocks startup. When two tools
/// describe the same name, the one from the earlier directory (or earlier
/// filename within a directory) wins and the duplicate is skipped with a
/// warning.
pub async fn load_toolbox_tools(dirs: &[PathBuf]) -> Vec<ToolboxTool> {
    let mut tools: Vec<ToolboxTool> = Vec::new();
    for entry in discover_toolbox_entries(dirs) {
        match describe_entry(&entry).await {
            Ok(tool) => {
                if let Some(existing) = tools.iter().find(|t| t.original_name == tool.original_name)
                {
                    warn!(
                        target: "cake",
                        "skipping toolbox tool '{}' from {}: name '{}' already provided by {}",
                        entry.filename,
                        entry.path.display(),
                        tool.original_name,
                        existing.path.display(),
                    );
                } else {
                    tools.push(tool);
                }
            },
            Err(reason) => {
                warn!(
                    target: "cake",
                    "skipping toolbox tool '{}' from {}: {reason}",
                    entry.filename,
                    entry.path.display(),
                );
            },
        }
    }
    tools
}

/// Run `describe` on a discovered executable and parse the output.
async fn describe_entry(entry: &ToolboxEntry) -> Result<ToolboxTool, String> {
    let mut command = tokio::process::Command::new(&entry.path);
    command
        .env("TOOLBOX_ACTION", "describe")
        .env("AGENT", "cake")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    place_in_own_process_group(&mut command);

    let mut child = command
        .spawn()
        .map_err(|e| format!("failed to run describe: {e}"))?;

    // Kills the whole process group on every path where the tool did not
    // exit on its own (timeout, output cap, read errors), so descendants a
    // describe script spawned cannot outlive the scan.
    let mut guard = ToolboxProcessGuard::new(child.id());

    let mut stdout = child.stdout.take().ok_or("failed to capture stdout")?;
    let mut stderr = child.stderr.take().ok_or("failed to capture stderr")?;

    // Bounded reads keep a broken tool that streams endless output (e.g.
    // `yes`) from exhausting memory before the timeout fires; the timeout
    // envelope also covers reaping, and on expiry the dropped child is
    // killed via kill_on_drop (with the guard reaching its descendants).
    let (streams, status) = tokio::time::timeout(DESCRIBE_TIMEOUT, async {
        let streams = read_streams_bounded(
            &mut stdout,
            &mut stderr,
            DESCRIBE_MAX_OUTPUT_BYTES,
            DESCRIBE_MAX_STDERR_BYTES,
        )
        .await?;
        if streams.stdout_capped {
            _ = child.kill().await;
            _ = child.wait().await;
            return Err(format!(
                "describe output exceeded {DESCRIBE_MAX_OUTPUT_BYTES} bytes"
            ));
        }
        let status = child
            .wait()
            .await
            .map_err(|e| format!("failed to wait for describe: {e}"))?;
        Ok((streams, status))
    })
    .await
    .map_err(|_elapsed| format!("describe timed out after {}s", DESCRIBE_TIMEOUT.as_secs()))??;

    // The tool exited on its own; anything it deliberately left running is
    // its business.
    guard.defuse();

    if !status.success() {
        let stderr = String::from_utf8_lossy(&streams.stderr);
        return Err(format!("describe exited with {status}: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&streams.stdout);
    parse_describe_output(&stdout, &entry.path)
}

/// Place the spawned tool in its own process group (Unix) so the guard's
/// group kill can reach every descendant, not just the direct child.
#[cfg(unix)]
pub fn place_in_own_process_group(command: &mut tokio::process::Command) {
    use std::os::unix::process::CommandExt;
    command.as_std_mut().process_group(0);
}

/// No-op on non-Unix platforms; `kill_on_drop` still terminates the
/// immediate child.
#[cfg(not(unix))]
pub fn place_in_own_process_group(_command: &mut tokio::process::Command) {}

/// RAII guard that kills a toolbox tool's entire process tree on drop,
/// unless [`defuse`](Self::defuse) has been called.
///
/// Mirrors the hook runtime's process-tree termination (`HookProcessGuard`
/// in `src/hooks.rs`): on Unix the guard sends `SIGKILL` to the tool's
/// process group, which equals its PID because the spawn goes through
/// [`place_in_own_process_group`]. On other platforms it is a no-op — the
/// `kill_on_drop(true)` on the `Command` still terminates the immediate
/// child on drop.
#[cfg(unix)]
#[derive(Debug)]
pub struct ToolboxProcessGuard {
    pid: Option<u32>,
}

#[cfg(unix)]
impl ToolboxProcessGuard {
    #[must_use]
    pub const fn new(pid: Option<u32>) -> Self {
        Self { pid }
    }

    /// Prevent the guard from killing the process group on drop.
    pub const fn defuse(&mut self) {
        self.pid = None;
    }

    /// Defuse after normal completion, or leave the guard armed when cake
    /// stopped the direct child and its descendants still need termination.
    pub const fn defuse_if_completed(&mut self, completed: bool) {
        if completed {
            self.defuse();
        }
    }
}

#[cfg(unix)]
impl Drop for ToolboxProcessGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            #[expect(
                unsafe_code,
                reason = "libc::kill requires unsafe; see SAFETY comment below"
            )]
            // SAFETY: pid was obtained from `Child::id()` at a point when the
            // child was still alive, and the child's process group ID equals
            // its PID because the spawn calls `place_in_own_process_group`.
            // A negative PID in `kill(2)` targets every process in the group
            // |pid|, which is exactly what we need to reach any descendant
            // pipelines, background jobs, etc.
            unsafe {
                ::libc::kill(-(pid as ::libc::pid_t), ::libc::SIGKILL);
            }
        }
    }
}

/// No-op guard for non-Unix platforms.
#[cfg(not(unix))]
#[derive(Debug)]
pub struct ToolboxProcessGuard;

#[cfg(not(unix))]
impl ToolboxProcessGuard {
    #[must_use]
    pub fn new(_pid: Option<u32>) -> Self {
        Self
    }

    pub fn defuse(&mut self) {}

    pub fn defuse_if_completed(&mut self, _completed: bool) {}
}

/// Captured stream data from a toolbox tool process.
pub struct BoundedStreams {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// True when stdout exceeded its cap and reading stopped early.
    pub stdout_capped: bool,
}

/// Read a child's stdout and stderr concurrently with hard memory caps.
///
/// stdout reading stops as soon as it exceeds `stdout_cap` (the caller
/// decides whether that is an error or a truncation); stderr keeps
/// draining so the child cannot block on a full pipe, but only the first
/// `stderr_cap` bytes are kept. Shared by the describe path here and the
/// execute path in `clients::tools::toolbox`.
pub async fn read_streams_bounded(
    stdout: &mut tokio::process::ChildStdout,
    stderr: &mut tokio::process::ChildStderr,
    stdout_cap: usize,
    stderr_cap: usize,
) -> Result<BoundedStreams, String> {
    use tokio::io::AsyncReadExt;

    let mut out_buf: Vec<u8> = Vec::new();
    let mut err_buf: Vec<u8> = Vec::new();
    // Heap-allocated so the future stays small (clippy::large_futures).
    let mut tmp_out = vec![0u8; 8192];
    let mut tmp_err = vec![0u8; 8192];
    let mut out_open = true;
    let mut err_open = true;

    while out_open || err_open {
        tokio::select! {
            n = stdout.read(&mut tmp_out), if out_open => {
                let n = n.map_err(|e| format!("stdout read error: {e}"))?;
                if n == 0 {
                    out_open = false;
                } else {
                    out_buf.extend_from_slice(&tmp_out[..n]);
                    if out_buf.len() > stdout_cap {
                        return Ok(BoundedStreams {
                            stdout: out_buf,
                            stderr: err_buf,
                            stdout_capped: true,
                        });
                    }
                }
            }
            n = stderr.read(&mut tmp_err), if err_open => {
                let n = n.map_err(|e| format!("stderr read error: {e}"))?;
                if n == 0 {
                    err_open = false;
                } else if err_buf.len() < stderr_cap {
                    err_buf.extend_from_slice(&tmp_err[..n]);
                    err_buf.truncate(stderr_cap);
                }
            }
        }
    }

    Ok(BoundedStreams {
        stdout: out_buf,
        stderr: err_buf,
        stdout_capped: false,
    })
}

/// Parse describe output, auto-detecting JSON vs. text format.
fn parse_describe_output(stdout: &str, path: &Path) -> Result<ToolboxTool, String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err("describe produced no output".to_string());
    }
    let (parsed, format) = match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(value) => (parse_json_describe(&value)?, ToolboxFormat::Json),
        Err(_) => (parse_text_describe(trimmed)?, ToolboxFormat::Text),
    };
    validate_tool_name(&parsed.name)?;
    Ok(ToolboxTool {
        registered_name: format!("{TOOLBOX_PREFIX}{}", parsed.name),
        original_name: parsed.name,
        path: path.to_path_buf(),
        description: parsed.description,
        parameters: parsed.parameters,
        format,
        timeout_secs: parsed.timeout_secs.unwrap_or(DEFAULT_EXECUTE_TIMEOUT_SECS),
        replay: parsed.replay,
    })
}

fn validate_parameter_schema(schema: &serde_json::Value) -> Result<(), String> {
    jsonschema::draft202012::new(schema)
        .map(|_| ())
        .map_err(|error| {
            format!("describe parameter schema is not valid JSON Schema draft 2020-12: {error}")
        })
}

/// Fields shared by both describe formats before assembly.
struct ParsedDescribe {
    name: String,
    description: String,
    parameters: serde_json::Value,
    timeout_secs: Option<u64>,
    replay: ReplaySafety,
}

/// Maximum length of a tool's original name. OpenAI-compatible tool APIs
/// reject function names longer than 64 characters, and registration adds
/// the 4-character `tb__` prefix.
const MAX_TOOL_NAME_LEN: usize = 64 - TOOLBOX_PREFIX.len();

/// Registered tool names are sent to model providers; restrict them to a
/// conservative character set and length so a toolbox script cannot
/// produce a name that invalidates API requests.
fn validate_tool_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("describe output has an empty name".to_string());
    }
    if name.len() > MAX_TOOL_NAME_LEN {
        return Err(format!(
            "tool name '{name}' is {} characters; the maximum is {MAX_TOOL_NAME_LEN} \
             (64-character provider limit minus the '{TOOLBOX_PREFIX}' prefix)",
            name.len()
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "tool name '{name}' contains characters outside [A-Za-z0-9_-]"
        ));
    }
    Ok(())
}

/// Parse the JSON describe format.
///
/// Supports the compact `args` form (`{"param": ["type", "description"]}`)
/// and the full `inputSchema` form (a top-level object JSON Schema). A `?`
/// suffix on a compact-form type marks the parameter optional. An optional
/// `timeout` field (seconds) overrides the execute timeout. An optional
/// `replay` field accepts `"safe"` or `"never"`; it defaults to `"never"`.
fn parse_json_describe(value: &serde_json::Value) -> Result<ParsedDescribe, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "describe JSON must be an object".to_string())?;
    let name = object
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "describe JSON is missing a string 'name' field".to_string())?
        .to_string();
    let description = object
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let timeout_secs = match object.get("timeout") {
        None => None,
        Some(v) => Some(
            v.as_u64()
                .filter(|&t| t > 0)
                .ok_or_else(|| "describe 'timeout' must be a positive integer".to_string())?,
        ),
    };
    let replay = parse_replay_declaration(object.get("replay"))?;
    let parameters = parse_json_parameters(object)?;

    Ok(ParsedDescribe {
        name,
        description,
        parameters,
        timeout_secs,
        replay,
    })
}

fn parse_json_parameters(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<serde_json::Value, String> {
    object.get("inputSchema").map_or_else(
        || {
            object
                .get("args")
                .map_or_else(|| Ok(empty_input_schema()), args_to_input_schema)
        },
        normalize_input_schema,
    )
}

/// Parse the optional replay declaration shared by toolbox describe formats.
fn parse_replay_declaration(value: Option<&serde_json::Value>) -> Result<ReplaySafety, String> {
    match value {
        None => Ok(ReplaySafety::Never),
        Some(serde_json::Value::String(value)) => match value.as_str() {
            "safe" => Ok(ReplaySafety::Safe),
            "never" => Ok(ReplaySafety::Never),
            _ => Err("describe 'replay' must be either 'safe' or 'never'".to_string()),
        },
        Some(_) => Err("describe 'replay' must be either 'safe' or 'never'".to_string()),
    }
}

/// Require the full schema to match the executor's object-only argument
/// contract. JSON Schema permits an omitted `type`; make the object
/// restriction explicit in that case so providers cannot generate a
/// top-level value the executor will later reject.
fn normalize_input_schema(schema: &serde_json::Value) -> Result<serde_json::Value, String> {
    let mut schema = schema
        .as_object()
        .cloned()
        .ok_or_else(|| "describe 'inputSchema' must be a JSON object".to_string())?;
    match schema.get("type") {
        None => {
            schema.insert(
                "type".to_string(),
                serde_json::Value::String("object".to_string()),
            );
        },
        Some(serde_json::Value::String(type_name)) if type_name == "object" => {},
        Some(_) => {
            return Err("describe 'inputSchema' must describe a top-level object".to_string());
        },
    }
    let schema = serde_json::Value::Object(schema);
    validate_parameter_schema(&schema)?;
    Ok(schema)
}

/// Convert the compact `args` map to a JSON Schema object.
fn args_to_input_schema(args: &serde_json::Value) -> Result<serde_json::Value, String> {
    let map = args
        .as_object()
        .ok_or_else(|| "describe 'args' must be a JSON object".to_string())?;
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for (param, spec) in map {
        let pair = spec
            .as_array()
            .filter(|a| a.len() == 2)
            .and_then(|a| Some((a[0].as_str()?, a[1].as_str()?)))
            .ok_or_else(|| {
                format!("describe arg '{param}' must be a [\"type\", \"description\"] pair")
            })?;
        let (raw_type, description) = pair;
        let (type_name, optional) = split_optional_type(raw_type);
        validate_param_type(param, type_name)?;
        if !optional {
            required.push(serde_json::Value::String(param.clone()));
        }
        properties.insert(
            param.clone(),
            serde_json::json!({ "type": type_name, "description": description }),
        );
    }
    Ok(assemble_input_schema(properties, required))
}

/// Parse the line-based text describe format:
///
/// ```text
/// name: run_tests
/// description: Run the test suite.
/// description: Additional description lines are concatenated.
/// pattern: string? Optional test filter pattern.
/// ```
///
/// Parameter lines require an explicit type; a `?` suffix marks the
/// parameter optional. Empty lines are ignored.
fn parse_text_describe(stdout: &str) -> Result<ParsedDescribe, String> {
    let mut name: Option<String> = None;
    let mut description_lines: Vec<String> = Vec::new();
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    let mut replay = ReplaySafety::Never;

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        parse_text_describe_line(
            line,
            &mut name,
            &mut description_lines,
            &mut properties,
            &mut required,
            &mut replay,
        )?;
    }

    let name = name.ok_or_else(|| "describe output has no 'name:' line".to_string())?;
    Ok(ParsedDescribe {
        name,
        description: description_lines.join("\n"),
        parameters: assemble_input_schema(properties, required),
        timeout_secs: None,
        replay,
    })
}

fn parse_text_describe_line(
    line: &str,
    name: &mut Option<String>,
    description_lines: &mut Vec<String>,
    properties: &mut serde_json::Map<String, serde_json::Value>,
    required: &mut Vec<serde_json::Value>,
    replay: &mut ReplaySafety,
) -> Result<(), String> {
    let (key, value) = line
        .split_once(':')
        .ok_or_else(|| format!("unparseable describe line: '{line}'"))?;
    let (key, value) = (key.trim(), value.trim());
    if key == "replay"
        && let Some(declaration) = text_replay_declaration(value)
    {
        *replay = declaration;
        return Ok(());
    }
    match key {
        "name" => *name = Some(value.to_string()),
        "description" => description_lines.push(value.to_string()),
        param => insert_text_parameter(properties, required, param, value)?,
    }
    Ok(())
}

fn text_replay_declaration(value: &str) -> Option<ReplaySafety> {
    match value {
        "safe" => Some(ReplaySafety::Safe),
        "never" => Some(ReplaySafety::Never),
        _ => None,
    }
}

/// Insert one unique text-format parameter into the assembled schema.
fn insert_text_parameter(
    properties: &mut serde_json::Map<String, serde_json::Value>,
    required: &mut Vec<serde_json::Value>,
    param: &str,
    value: &str,
) -> Result<(), String> {
    if properties.contains_key(param) {
        return Err(format!(
            "duplicate text toolbox parameter '{param}' in describe output"
        ));
    }
    let (schema, optional) = parse_text_parameter(param, value)?;
    if !optional {
        required.push(serde_json::Value::String(param.to_string()));
    }
    properties.insert(param.to_string(), schema);
    Ok(())
}

/// Parse and validate one text-format parameter declaration.
fn parse_text_parameter(param: &str, value: &str) -> Result<(serde_json::Value, bool), String> {
    validate_text_argument_name(param)?;
    let (raw_type, description) = match value.split_once(char::is_whitespace) {
        Some((type_name, rest)) => (type_name, rest.trim()),
        None => (value, ""),
    };
    let (type_name, optional) = split_optional_type(raw_type);
    validate_param_type(param, type_name)?;
    Ok((
        serde_json::json!({ "type": type_name, "description": description }),
        optional,
    ))
}

/// Enforce the names that the line-based execute protocol can encode as a
/// single `key=value` record. Shared by describe parsing and execution so a
/// text tool cannot advertise a parameter that every invocation rejects.
pub fn validate_text_argument_name(name: &str) -> Result<(), String> {
    if name.contains(['\r', '\n', '=']) {
        return Err(format!(
            "invalid text toolbox argument name '{name}': names cannot contain '=', carriage returns, or newlines"
        ));
    }
    Ok(())
}

/// Split a trailing `?` optional marker off a parameter type.
fn split_optional_type(raw_type: &str) -> (&str, bool) {
    raw_type
        .strip_suffix('?')
        .map_or((raw_type, false), |t| (t, true))
}

/// Reject parameter types outside the JSON Schema primitives that both
/// describe formats support.
fn validate_param_type(param: &str, type_name: &str) -> Result<(), String> {
    match type_name {
        "string" | "number" | "integer" | "boolean" => Ok(()),
        other => Err(format!(
            "parameter '{param}' has unsupported type '{other}' \
             (expected string, number, integer, or boolean)"
        )),
    }
}

fn empty_input_schema() -> serde_json::Value {
    serde_json::json!({ "type": "object", "properties": {} })
}

fn assemble_input_schema(
    properties: serde_json::Map<String, serde_json::Value>,
    required: Vec<serde_json::Value>,
) -> serde_json::Value {
    let mut schema = serde_json::json!({
        "type": "object",
        "properties": serde_json::Value::Object(properties),
    });
    if !required.is_empty() {
        schema["required"] = serde_json::Value::Array(required);
    }
    schema
}

#[cfg(test)]
#[path = "toolbox_tests.rs"]
mod toolbox_tests;
