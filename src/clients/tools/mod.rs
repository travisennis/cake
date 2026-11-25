//! Tool definitions and execution for the AI agent.
//!
//! This module provides the tool interface that allows the AI agent to interact
//! with the host system through controlled operations. File tools validate
//! target paths against the shared `PathGrants` table built from the working
//! directory, temporary directories, configured directories, built-in
//! toolchain and system paths, skill directories, and Cake state.
//!
//! # Available Tools
//!
//! - `Bash` - Execute shell commands with timeout and output capture
//! - `Read` - Read file contents with line range support
//! - `ReadImage` - Read an image file for a vision-capable model
//! - `Edit` - Make targeted edits to files using literal search-replace
//! - `Write` - Create or overwrite files with content
//!
//! User-defined toolbox tools (`tb__*`, see `config::toolbox` and the
//! `toolbox` submodule) can be registered alongside the built-ins.
//!
//! # Security
//!
//! Built-in file tools validate target paths against the shared `PathGrants`
//! table. `Read` and `ReadImage` accept read-write and read-only grants;
//! `Edit` and `Write` accept only read-write grants; credential-bearing paths
//! are denied before either class. Bash uses the OS-sandbox projection and is
//! additionally constrained by its OS sandbox. Toolbox tools are user-provided
//! trusted executables that run outside the OS sandbox and are never registered
//! under the read-only sandbox policy.

use serde::Serialize;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use crate::config::mcp::McpExposure;
use crate::config::settings::ToolLimits;
use crate::gates::composer::GatePipeline;
use crate::session_telemetry::{CompensationEventTelemetry, CompensationKind};
use crate::types::{ImagePart, ReplaySafety};

mod bash_session;
mod bash_session_core;
mod sandbox;

pub use sandbox::{GrantReport, GrantReportInputs, resolve_sandbox_policy_with_source};
pub use sandbox::{SandboxPolicy, resolve_linked_worktree_dirs, resolve_sandbox_policy};

pub fn compute_temp_directories() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    dirs.push(PathBuf::from("/tmp"));
    if let Ok(canonical) = std::fs::canonicalize("/tmp")
        && canonical.as_path() != Path::new("/tmp")
    {
        dirs.push(canonical);
    }

    dirs.push(PathBuf::from("/var/folders"));
    if let Ok(canonical) = std::fs::canonicalize("/var/folders")
        && canonical.as_path() != Path::new("/var/folders")
    {
        dirs.push(canonical);
    }

    if let Ok(tmpdir) = std::env::var("TMPDIR") {
        let tmpdir_path = PathBuf::from(&tmpdir);
        dirs.push(tmpdir_path.clone());
        if let Ok(canonical) = std::fs::canonicalize(&tmpdir)
            && canonical != tmpdir_path
        {
            dirs.push(canonical);
        }
    }

    dirs
}

/// Directory context used by tool execution and sandbox construction.
#[derive(Clone, Debug)]
pub struct ToolContext {
    pub cwd: PathBuf,
    /// Resolved sandbox policy applied to model-generated shell commands.
    sandbox_policy: SandboxPolicy,
    /// The single resolved grant table shared by the OS sandbox and the
    /// in-process file tools, built once at construction.
    path_grants: sandbox::PathGrants,
    /// The Bash-facing projection of `path_grants`, built once because every
    /// Bash call in the run consumes the same fixed grants.
    sandbox_config: sandbox::SandboxConfig,
    /// The ordered command-policy pipeline for the Bash preflight. `None` when
    /// the run has no gate configured; the Bash tool fails closed on an absent
    /// pipeline rather than running a command ungated.
    pub gates: Option<Arc<GatePipeline>>,

    /// Resolved tool limits from `[limits]` settings. Defaults to the
    /// compiled constants; `with_limits` applies configured overrides.
    pub limits: ToolLimits,
    /// Per-run Read history used by Edit's stale-read and coverage
    /// diagnostics. Shared across clones so rebuilding the context for gates
    /// preserves observations from the current run.
    read_tracker: Arc<read_tracker::ReadTracker>,
    bash_sessions: bash_session_core::SessionRegistry,
}

impl ToolContext {
    /// Build a tool context using the same temp directory discovery as the
    /// existing process-global cache.
    pub fn new(
        cwd: PathBuf,
        additional_dirs: &[PathBuf],
        skill_dirs: &[PathBuf],
        settings_dirs: &[PathBuf],
        sandbox_policy: SandboxPolicy,
    ) -> Self {
        Self::with_temp_dirs_and_policy(
            cwd,
            &compute_temp_directories(),
            additional_dirs,
            skill_dirs,
            settings_dirs,
            sandbox_policy,
        )
    }

    /// The resolved sandbox policy for this context.
    pub(crate) const fn sandbox_policy(&self) -> SandboxPolicy {
        self.sandbox_policy
    }

    /// The cached grant table. The OS sandbox projects its writable and
    /// read-execute views from this value; the in-process file tools consume
    /// the same value in the validator.
    const fn path_grants(&self) -> &sandbox::PathGrants {
        &self.path_grants
    }

    /// The cached Bash sandbox configuration derived from `path_grants`.
    const fn sandbox_config(&self) -> &sandbox::SandboxConfig {
        &self.sandbox_config
    }

    /// Attach the command-policy pipeline used by the Bash preflight.
    pub fn with_gates(mut self, gates: Option<Arc<GatePipeline>>) -> Self {
        self.gates = gates;
        self
    }

    /// Record the latest successful Read for `path` in this run.
    pub(crate) fn record_read(&self, path: &Path, observation: read_tracker::ReadObservation) {
        self.read_tracker.record_read(path, observation);
    }

    /// Record a successful Edit so a later Edit can warn when the file has
    /// not been re-read since.
    pub(crate) fn record_edit(&self, path: &Path) {
        self.read_tracker.record_edit(path);
    }

    /// Return this run's latest Read for `path` and whether an Edit has
    /// invalidated it.
    pub(crate) fn read_status(&self, path: &Path) -> Option<read_tracker::ReadStatus> {
        self.read_tracker.status(path)
    }

    /// Attach resolved tool limits from the `[limits]` settings section.
    /// Absent keys already resolved to the compiled defaults inside
    /// `ToolLimits`, so this is a plain override.
    pub fn with_limits(mut self, limits: ToolLimits) -> Self {
        self.bash_sessions = bash_session_core::SessionRegistry::new(
            limits.bash_session_max,
            limits.bash_session_output_max_bytes,
            std::time::Duration::from_secs(limits.bash_session_exited_ttl_seconds),
        );
        self.limits = limits;
        self
    }

    fn with_temp_dirs_and_policy(
        cwd: PathBuf,
        temp_dirs: &[PathBuf],
        additional_dirs: &[PathBuf],
        skill_dirs: &[PathBuf],
        settings_dirs: &[PathBuf],
        sandbox_policy: SandboxPolicy,
    ) -> Self {
        let path_grants = Self::build_path_grants(
            sandbox_policy,
            &cwd,
            temp_dirs,
            additional_dirs,
            settings_dirs,
            skill_dirs,
        );
        let sandbox_config = sandbox::SandboxConfig::from_grants(&path_grants);
        Self {
            cwd,
            sandbox_policy,
            path_grants,
            sandbox_config,
            gates: None,
            limits: ToolLimits::defaults(),
            read_tracker: Arc::new(read_tracker::ReadTracker::default()),
            bash_sessions: bash_session_core::SessionRegistry::new(
                ToolLimits::defaults().bash_session_max,
                ToolLimits::defaults().bash_session_output_max_bytes,
                std::time::Duration::from_secs(
                    ToolLimits::defaults().bash_session_exited_ttl_seconds,
                ),
            ),
        }
    }

    fn build_path_grants(
        sandbox_policy: SandboxPolicy,
        cwd: &Path,
        temp_dirs: &[PathBuf],
        additional_dirs: &[PathBuf],
        settings_dirs: &[PathBuf],
        skill_dirs: &[PathBuf],
    ) -> sandbox::PathGrants {
        sandbox::PathGrants::build_with_home_from_env(
            sandbox_policy,
            cwd,
            temp_dirs,
            additional_dirs,
            settings_dirs,
            skill_dirs,
        )
    }

    /// Build a context from the current process environment with no configured
    /// extra directories.
    pub(crate) fn from_current_process() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::with_temp_dirs_and_policy(
            cwd,
            &compute_temp_directories(),
            &[],
            &[],
            &[],
            SandboxPolicy::WorkspaceWrite,
        )
    }
}

mod bash;
mod edit;
mod file_access;
mod file_replace;
mod json_repair;
pub(in crate::clients) mod mcp;
mod read;
mod read_image;
mod read_tracker;
mod scheduling;
mod script_evidence;
mod secure_temp_dir;
mod skill_search;
mod tool_search;
mod toolbox;
mod write;

/// Crate-visible so `clients` can re-export it for hook payload parsing; the
/// hook view of `tool_input` must use the same repair pass as executors (#185).
pub use json_repair::repair_json_args;
pub(super) use mcp::mcp_tool_entry;
pub use read::extract_path as read_extract_path;
pub(super) use scheduling::{ScheduledToolCall, schedule_tool_calls};
pub use skill_search::{
    SKILL_SEARCH_NAME, SkillSearchEntry, skill_search_enabled, skill_search_entry,
};
pub use tool_search::{
    TOOL_SEARCH_NAME, ToolSearchCatalog, ToolSearchEntry, tool_search_enabled, tool_search_entry,
};
pub(super) use toolbox::toolbox_tool_entry;

/// Compensation events derivable from a tool call's arguments and outcome,
/// computed in the agent loop so the detection rules cannot drift from the
/// tool parse paths and so events survive calls that fail after a repair.
///
/// - `json_repair`: the conservative repair pass modified the arguments and
///   the repaired payload parses as valid JSON. Applies to every registered
///   repair-using tool (Edit, Write, `tb__*`), whether or not the call later
///   failed; an unregistered tool never reaches an argument parser.
/// - `edit_invalid_arguments`: a registered Edit call failed and its
///   arguments still fail to parse after the repair pass.
pub(super) fn argument_compensation_events(
    name: &str,
    arguments: &str,
    was_error: bool,
    registered: bool,
    repairs_arguments: bool,
) -> Vec<CompensationEventTelemetry> {
    let mut events = Vec::new();
    if registered && repairs_arguments {
        let repaired = repair_json_args(arguments);
        if repaired != arguments && serde_json::from_str::<serde_json::Value>(&repaired).is_ok() {
            events.push(CompensationEventTelemetry::new(
                CompensationKind::JsonRepair,
                Some(name.to_string()),
            ));
        }
    }
    if was_error
        && registered
        && name == "Edit"
        && edit::arguments_are_invalid(&strip_intent(arguments))
    {
        events.push(CompensationEventTelemetry::new(
            CompensationKind::EditInvalidArguments,
            None,
        ));
    }
    events
}

/// Push an `output_truncation` compensation event when a tool's output hit an
/// inline cap or spilled to a temp file. One event per truncated run.
pub(super) fn push_output_truncation_event_if(
    events: &mut Vec<CompensationEventTelemetry>,
    tool_name: &str,
    truncated: bool,
) {
    if truncated {
        events.push(CompensationEventTelemetry::new(
            CompensationKind::OutputTruncation,
            Some(tool_name.to_string()),
        ));
    }
}

/// Maximum characters of context to show around a parse-failure position.
const JSON_PARSE_CONTEXT_WIDTH: usize = 80;

/// Format a JSON parse error with a bounded context window, caret marker,
/// and targeted hint keyed off the error kind.
///
/// The serde `line`/`column` is converted to a byte offset in `payload`
///(which should be the payload after repair, i.e. the string that was
/// actually fed to the JSON parser).
pub(super) fn format_json_parse_error(
    payload: &str,
    err: &serde_json::Error,
    tool_name: &str,
    expected_shape: &str,
) -> String {
    let error_msg = err.to_string();
    let line = err.line();
    let column = err.column();

    let context = extract_json_error_context(payload, line, column);

    let mut msg = format!("Invalid {tool_name} arguments: {error_msg}");

    if let Some((snippet, caret)) = context {
        _ = std::fmt::Write::write_fmt(&mut msg, format_args!("\n\nContext:\n{snippet}\n{caret}"));
    }

    let hint = json_error_hint(&error_msg);
    _ = std::fmt::Write::write_fmt(&mut msg, format_args!("\nHint: {hint}"));
    _ = std::fmt::Write::write_fmt(&mut msg, format_args!("\nExpected shape: {expected_shape}"));

    msg
}

/// Convert a 1-indexed line/column position to a byte offset in `s`.
fn line_col_to_offset(s: &str, line: usize, column: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut current_line: usize = 1;
    let mut line_start: usize = 0;

    for offset in 0..bytes.len() {
        if current_line == line {
            let col_byte_offset = line_start + column.saturating_sub(1);
            if col_byte_offset <= bytes.len() {
                return Some(col_byte_offset);
            }
            return None;
        }
        if bytes[offset] == b'\n' {
            current_line += 1;
            line_start = offset + 1;
        }
    }

    if current_line == line {
        let col_byte_offset = line_start + column.saturating_sub(1);
        if col_byte_offset <= bytes.len() {
            return Some(col_byte_offset);
        }
    }

    None
}

/// Extract an escaped context snippet and caret line around `line`/`column`.
fn extract_json_error_context(
    payload: &str,
    line: usize,
    column: usize,
) -> Option<(String, String)> {
    let offset = line_col_to_offset(payload, line, column)?;

    let half = JSON_PARSE_CONTEXT_WIDTH / 2;
    let start = offset.saturating_sub(half);
    let mut end = offset + half;
    if end > payload.len() {
        end = payload.len();
    }

    let before = payload.get(start..offset).unwrap_or_default();
    let after = payload.get(offset..end).unwrap_or_default();

    let escaped_before = escape_json_context(before);
    let escaped_after = escape_json_context(after);

    let snippet = format!("    {escaped_before}{escaped_after}");
    let caret = format!("    {:>width$}", "^", width = escaped_before.len());

    Some((snippet, caret))
}

/// Escape control characters in a JSON context window for display.
fn escape_json_context(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\0' => out.push_str("\\0"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() && (c as u32) <= 0x1F => {
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "String write_fmt always succeeds"
                )]
                let _ =
                    std::fmt::Write::write_fmt(&mut out, format_args!("\\u{{{:04X}}}", c as u32));
            },
            c => out.push(c),
        }
    }
    out
}

/// Produce a targeted hint based on the serde error message text.
fn json_error_hint(error_msg: &str) -> &'static str {
    let msg = error_msg.split(" at line ").next().unwrap_or(error_msg);

    if msg.contains("control character") {
        "A raw control character (tab, newline, etc.) was found inside a JSON string. \
         Must escape as \\t, \\n, or \\r."
    } else if msg.contains("trailing characters") || msg.contains("trailing data") {
        "Content continued after the closing brace of the arguments object. \
         Check for extra data after the final `}`."
    } else if msg.contains("missing field `path`") {
        "The `path` field is required at the top level. Provide `path` before `edits`/`content`."
    } else if msg.contains("missing field") {
        "A required field is missing. Double-check the expected shape above."
    } else if msg.contains("unknown field") {
        "An unrecognized field name was provided. Check spelling against the expected shape."
    } else if msg.contains("key must be a string") {
        "Object keys must be double-quoted strings."
    } else if msg.contains("EOF while parsing") || msg.contains("end of input") {
        "The JSON input ended unexpectedly. Check for unclosed braces, brackets, or quotes."
    } else {
        "Check the payload structure against the expected shape."
    }
}

/// Tool definition sent in API requests.
///
/// Represents a function tool that the AI model can call during conversation.
/// Each tool has a name, description, and JSON schema for its parameters.
///
#[derive(Serialize, Clone, Debug)]
pub struct Tool {
    #[serde(rename = "type")]
    pub(super) type_: String,
    pub(super) name: String,
    pub(super) description: String,
    pub(super) parameters: serde_json::Value,
}

/// Name of the optional, display-only field every tool schema advertises.
const INTENT_PROPERTY: &str = "intent";

/// Schema description for [`INTENT_PROPERTY`].
const INTENT_PROPERTY_DESCRIPTION: &str = "Optional. One short, user-facing sentence in the present progressive describing what this \
     call does, for example \"Reading the parser tests\". Shown to the user instead of the raw \
     arguments.";

/// Add the optional, display-only `intent` property to one tool schema.
///
/// Every tool definition, including toolbox tools, advertises the same field
/// so the model has one consistent place to describe a call. A tool that
/// already declares `intent` keeps its own schema.
fn add_intent_property(parameters: &mut serde_json::Value) {
    let Some(schema) = parameters.as_object_mut() else {
        return;
    };
    let object_schema = schema
        .get("type")
        .is_none_or(|value| value.as_str() == Some("object"))
        || schema.contains_key("properties");
    if !object_schema {
        return;
    }
    if !schema.contains_key("properties") {
        schema.insert("properties".to_string(), serde_json::json!({}));
    }
    let Some(properties) = schema
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    properties
        .entry(INTENT_PROPERTY.to_string())
        .or_insert_with(|| {
            serde_json::json!({
                "type": "string",
                "description": INTENT_PROPERTY_DESCRIPTION,
            })
        });
}

/// Remove the display-only `intent` field before a tool parses its arguments.
///
/// Tool schemas advertise `intent` centrally, but it is call metadata for the
/// user interface, not tool input: strict parsers such as Edit's
/// `deny_unknown_fields` and toolbox processes must never see it. Arguments
/// that do not parse as a JSON object, or that contain no `intent`, are
/// returned unchanged so the tool's own error reporting still describes the
/// model's payload.
fn strip_intent(arguments: &str) -> String {
    if !arguments.contains(INTENT_PROPERTY) {
        return arguments.to_string();
    }
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return arguments.to_string();
    };
    let Some(object) = value.as_object_mut() else {
        return arguments.to_string();
    };
    if object.remove(INTENT_PROPERTY).is_none() {
        return arguments.to_string();
    }
    serde_json::to_string(&value).unwrap_or_else(|_| arguments.to_string())
}

/// Return whether raw file bytes contain a NUL byte.
///
/// Read and Edit use the same conservative binary-file signal while keeping
/// their tool-specific error messages and bounded reader behavior separate.
pub(super) fn contains_null_byte(bytes: &[u8]) -> bool {
    bytes.contains(&0)
}

/// Decode bytes as UTF-8 without lossy replacement.
///
/// Read uses this for delivered line content and Edit uses it for the complete
/// bounded file, so both tools share the same validation primitive.
pub(super) const fn decode_utf8(bytes: &[u8]) -> Result<&str, std::str::Utf8Error> {
    std::str::from_utf8(bytes)
}

/// Result of executing a tool.
///
/// Contains the output string from tool execution, which may be stdout/stderr
/// for Bash or file contents for Read operations, plus any model-compensation
/// events observed while running the tool (recorded to session telemetry by
/// the agent loop). Permission denials are kept separate because they are
/// task-completion audit data, not model-compensation telemetry.
#[derive(Debug)]
pub struct ToolResult {
    pub output: String,
    /// Inline images produced by the tool (for example a file the model asked
    /// to see). Empty for every text-only result.
    pub images: Vec<ImagePart>,
    pub compensation_events: Vec<CompensationEventTelemetry>,
    /// Stable labels for policy denials observed after a tool was allowed to run.
    /// The agent prefixes each label with the tool and call identity before it
    /// writes `task_complete.permission_denials`.
    pub permission_denials: Vec<String>,
}

/// Error from executing a tool.
///
/// Carries the model-visible message plus any model-compensation events
/// observed while the tool failed (for example a gate `block` decision or a
/// fail-closed denial), so they still reach session telemetry on the error
/// path instead of being dropped.
#[derive(Debug, Clone)]
pub struct ToolError {
    pub message: String,
    pub compensation_events: Vec<CompensationEventTelemetry>,
}

impl ToolError {
    /// Build an error with no compensation events (the common case).
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            compensation_events: Vec::new(),
        }
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl<T: Into<String>> From<T> for ToolError {
    fn from(message: T) -> Self {
        Self::new(message)
    }
}

type ToolFuture = Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send>>;
type ToolExecutor = Arc<dyn Fn(Arc<ToolContext>, String, String) -> ToolFuture + Send + Sync>;

/// Extracts the canonical path a tool's call targets for scheduling from its
/// arguments.
type PathTargetFn = Arc<dyn Fn(&ToolContext, &str) -> Result<PathBuf, String> + Send + Sync>;

/// How a scheduled call touches the canonical path its resolver returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PathTouch {
    /// Observes the path's bytes without changing them (`Read`, `ReadImage`).
    Observe,
    /// Writes the path's bytes (Edit, Write).
    Mutate,
}

/// Declared semantics of a registered tool, consumed by same-path scheduling,
/// read-only sandbox filtering, and hook payload shaping.
///
/// Defaults fail closed (#277): an unmarked entry is treated as parsing its
/// arguments strictly, as unsafe to offer under the read-only sandbox policy,
/// and as non-mutating for scheduling purposes. Registration is the single
/// place that states what a tool is.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag describes an independent tool behavior"
)]
#[derive(Clone)]
pub(super) struct ToolCapabilities {
    /// The executor applies the conservative JSON repair pass before parsing
    /// its arguments. The hook layer must present exactly this set of tools
    /// with the repaired `tool_input` view, so hook policy and execution
    /// agree on what will run (#185).
    pub(super) repairs_arguments: bool,
    /// The tool cannot mutate host state in-process, so it stays available
    /// under the read-only sandbox policy.
    pub(super) read_safe: bool,
    /// Whether an interrupted call may be re-executed after recovery. The
    /// conservative default is [`ReplaySafety::Never`].
    pub(super) replay_safety: ReplaySafety,
    /// Resolves the canonical path this call targets and how it touches that
    /// path; drives same-path serialization.
    pub(super) path_target: Option<(PathTargetFn, PathTouch)>,
    /// The tool sends image bytes to the provider, so it is only offered to a
    /// model that declares image support. A text-only model would otherwise
    /// receive a tool whose output the provider rejects.
    pub(super) requires_image_support: bool,
    /// The tool's argument parser ignores unknown top-level fields, so the
    /// display-only `intent` field can reach it and skip a parse-and-reserialize
    /// pass. Strict parsers, toolbox processes, and MCP servers keep it stripped.
    pub(super) tolerates_intent: bool,
}

impl ToolCapabilities {
    const fn conservative() -> Self {
        Self {
            repairs_arguments: false,
            read_safe: false,
            replay_safety: ReplaySafety::Never,
            path_target: None,
            requires_image_support: false,
            tolerates_intent: false,
        }
    }
}

/// Registered behavior for a callable tool.
///
/// This keeps the model-facing definition and execution entry point together
/// so adding a tool only requires one registry entry. The executor is a
/// shared closure so entries can capture per-tool state (e.g. a toolbox
/// tool's executable path and protocol format).
#[derive(Clone)]
pub(super) struct ToolEntry {
    definition: Tool,
    execute: ToolExecutor,
    capabilities: ToolCapabilities,
}

impl ToolEntry {
    fn new(
        definition: Tool,
        execute: impl Fn(Arc<ToolContext>, String, String) -> ToolFuture + Send + Sync + 'static,
    ) -> Self {
        Self {
            definition,
            execute: Arc::new(execute),
            capabilities: ToolCapabilities::conservative(),
        }
    }

    /// Mark the tool as unable to mutate host state in-process, keeping it
    /// available under the read-only sandbox policy.
    const fn read_safe(mut self) -> Self {
        self.capabilities.read_safe = true;
        self
    }

    /// Mark the tool as safe to re-execute after recovery when the current
    /// registry declaration still agrees.
    const fn replay_safe(mut self) -> Self {
        self.capabilities.replay_safety = ReplaySafety::Safe;
        self
    }

    /// Set the replay declaration for a tool supplied by an external manifest.
    const fn with_replay_safety(mut self, replay_safety: ReplaySafety) -> Self {
        self.capabilities.replay_safety = replay_safety;
        self
    }

    /// Declare that the executor applies the conservative JSON repair pass
    /// before parsing arguments, so hooks see the repaired payload (#185).
    const fn repairs_arguments(mut self) -> Self {
        self.capabilities.repairs_arguments = true;
        self
    }

    /// Declare that the tool sends image bytes to the provider, so it is
    /// registered only for a model that declares image support.
    const fn requires_images(mut self) -> Self {
        self.capabilities.requires_image_support = true;
        self
    }

    /// Declare that the executor's parser ignores unknown top-level fields, so
    /// the display-only `intent` field can be left in place.
    const fn tolerates_intent(mut self) -> Self {
        self.capabilities.tolerates_intent = true;
        self
    }

    /// Attach the resolver for the canonical path this tool's call mutates,
    /// enabling same-path serialization in scheduling.
    fn mutates_path(
        mut self,
        target: impl Fn(&ToolContext, &str) -> Result<PathBuf, String> + Send + Sync + 'static,
    ) -> Self {
        self.capabilities.path_target = Some((Arc::new(target), PathTouch::Mutate));
        self
    }

    /// Attach the resolver for the canonical path this tool's call observes,
    /// so the call serializes with same-path mutations in scheduling.
    fn observes_path(
        mut self,
        target: impl Fn(&ToolContext, &str) -> Result<PathBuf, String> + Send + Sync + 'static,
    ) -> Self {
        self.capabilities.path_target = Some((Arc::new(target), PathTouch::Observe));
        self
    }
}

/// Registry of tools available to an agent.
#[derive(Clone)]
pub(super) struct ToolRegistry {
    entries: Vec<ToolEntry>,
    /// Cached model-facing tool definitions, computed once at construction.
    definitions: Vec<Tool>,
    /// Monotonic revision of the model-facing surface.
    revision: u64,
}

impl ToolRegistry {
    /// Return the cached model-facing tool definitions.
    pub(super) fn definitions(&self) -> &[Tool] {
        &self.definitions
    }

    /// Return the revision that advances whenever the definitions change.
    pub(super) const fn revision(&self) -> u64 {
        self.revision
    }

    /// Remove every tool not declared read-safe.
    ///
    /// The read-only sandbox policy uses this so the model never sees tools
    /// it cannot use: Edit and Write bypass the OS sandbox (which only wraps
    /// Bash), and toolbox tools run as unsandboxed external processes, so
    /// omitting them is what makes the policy's no-mutation guarantee hold
    /// for the whole agent, not just shell commands.
    pub(super) fn remove_bash_tools(&mut self) {
        self.entries
            .retain(|entry| !matches!(entry.definition.name.as_str(), "Bash" | "BashSession"));
        self.refresh_definitions();
    }

    pub(super) fn retain_read_safe_tools(&mut self) {
        self.entries.retain(|entry| entry.capabilities.read_safe);
        self.refresh_definitions();
    }

    /// Remove every tool that needs a capability the model does not have.
    ///
    /// A tool whose output the provider cannot accept must never reach the
    /// model: rather than registering it and failing at request time, the
    /// registry omits it, so the prompt tool list and the request tools array
    /// agree with what the model can actually use.
    pub(super) fn retain_model_supported_tools(&mut self, supports_images: bool) {
        if !supports_images {
            self.entries
                .retain(|entry| !entry.capabilities.requires_image_support);
        }
        self.refresh_definitions();
    }

    /// Retain selected names, including `BashSession` when Bash is selected so
    /// every yielded command has a model-visible controller.
    pub(super) fn retain_enabled_tools(&mut self, enabled: &[String]) {
        let bash_enabled = enabled.iter().any(|name| name == "Bash");
        self.entries.retain(|entry| {
            enabled.iter().any(|name| name == &entry.definition.name)
                || (bash_enabled && entry.definition.name == "BashSession")
        });
        self.refresh_definitions();
    }

    /// Rebuild the model-facing definitions after the registry changes.
    ///
    /// Built-in descriptions can recommend another built-in tool. Keep those
    /// recommendations in the definition only when that tool is also
    /// available to the model; otherwise a restricted tool selection would
    /// tell the model to call a tool it cannot use. Toolbox descriptions are
    /// user-provided and are left unchanged.
    fn refresh_definitions(&mut self) {
        self.revision = self.revision.saturating_add(1);
        let available_names: Vec<String> = self
            .entries
            .iter()
            .map(|entry| entry.definition.name.clone())
            .collect();
        self.definitions = self
            .entries
            .iter()
            .map(|entry| {
                let mut definition = entry.definition.clone();
                definition.description = filter_builtin_description(
                    &definition.name,
                    &definition.description,
                    &available_names,
                );
                add_intent_property(&mut definition.parameters);
                definition
            })
            .collect();
    }

    /// Append a tool entry and refresh the cached definitions.
    pub(super) fn push_entry(&mut self, entry: ToolEntry) {
        self.entries.push(entry);
        self.refresh_definitions();
    }

    /// Return the enabled tool names.
    pub(super) fn names(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| entry.definition.name.clone())
            .collect()
    }

    /// Execute a registered tool by name.
    pub(super) async fn execute(
        &self,
        context: Arc<ToolContext>,
        name: &str,
        call_id: &str,
        arguments: &str,
    ) -> Result<ToolResult, ToolError> {
        let Some(entry) = self.find(name) else {
            return Err(ToolError::new(format!("Unknown tool: {name}")));
        };

        let arguments = if entry.capabilities.tolerates_intent {
            arguments.to_string()
        } else {
            strip_intent(arguments)
        };
        (entry.execute)(context, call_id.to_string(), arguments).await
    }

    /// Return whether a tool with `name` is registered.
    pub(super) fn has(&self, name: &str) -> bool {
        self.find(name).is_some()
    }

    /// Remove one registered tool by name, returning whether it was present.
    pub(super) fn remove_by_name(&mut self, name: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.definition.name != name);
        let removed = self.entries.len() != before;
        if removed {
            self.refresh_definitions();
        }
        removed
    }

    /// Return whether a registered tool's executor applies the conservative
    /// JSON repair pass before parsing its arguments. Unregistered names are
    /// never repaired: they never reach an argument parser.
    pub(super) fn repairs_arguments(&self, name: &str) -> bool {
        self.find(name)
            .is_some_and(|entry| entry.capabilities.repairs_arguments)
    }

    /// Return the replay declaration for a registered tool. Unknown tools
    /// default to `never` so a missing registry entry fails closed.
    pub(super) fn replay_safety(&self, name: &str) -> ReplaySafety {
        self.find(name).map_or(ReplaySafety::Never, |entry| {
            entry.capabilities.replay_safety
        })
    }

    /// Return the canonical path a registered tool's call targets for
    /// scheduling and how the call touches it, using the resolver declared at
    /// registration.
    pub(super) fn path_target(
        &self,
        context: &ToolContext,
        name: &str,
        arguments: &str,
    ) -> Option<(Result<PathBuf, String>, PathTouch)> {
        let (target, touch) = self.find(name)?.capabilities.path_target.as_ref()?;
        Some((target(context, arguments), *touch))
    }

    /// Return how a registered tool's calls touch the canonical path its
    /// resolver targets, without resolving a path. Unknown tools and tools
    /// that declare no path target return `None`.
    pub(super) fn path_touch(&self, name: &str) -> Option<PathTouch> {
        Some(self.find(name)?.capabilities.path_target.as_ref()?.1)
    }

    fn find(&self, name: &str) -> Option<&ToolEntry> {
        self.entries
            .iter()
            .find(|entry| entry.definition.name == name)
    }
}

/// Access level for a validated path
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PathAccess {
    /// Path is in a read-write grant (working directory, temp directory, or
    /// configured writable directory).
    ReadWrite,
    /// Path is in a read-only grant (built-in toolchain or system path,
    /// `--add-dir`, skill directory, Cake state, or a path demoted by the
    /// `read-only` policy).
    ReadOnly,
}

/// Result of path validation containing the canonical path and access level
#[derive(Debug)]
pub(super) struct ValidatedPath {
    pub canonical: std::path::PathBuf,
    pub access: PathAccess,
}

/// Classification of a canonical path against the shared grant table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathClassification {
    /// The path is credential-bearing and wins over every ordinary grant.
    Denied,
    /// The path is inside an ordinary grant.
    Allowed(PathAccess),
    /// The path is outside every ordinary grant.
    Outside,
}

/// Validate that a path exists and is inside the shared grant table.
///
/// The working directory, temporary directories, and settings directories
/// grant read-write access; built-in toolchain and system paths, additional
/// (`--add-dir`) directories, skill directories, and Cake state grant read-only
/// access. Credential-bearing paths are denied before either class is
/// considered.
///
/// Returns the canonical path along with its access level.
pub(super) fn validate_path(
    context: &ToolContext,
    path_str: &str,
) -> Result<ValidatedPath, String> {
    validate_path_with_grants(path_str, context.path_grants())
}

/// Canonicalize a path and classify it against an explicit grant table.
fn validate_path_with_grants(
    path_str: &str,
    grants: &sandbox::PathGrants,
) -> Result<ValidatedPath, String> {
    let canonical = Path::new(path_str)
        .canonicalize()
        .map_err(|e| format!("Path not found or not accessible '{path_str}': {e}"))?;

    match classify_resolved_path(&canonical, grants) {
        PathClassification::Allowed(access) => Ok(ValidatedPath { canonical, access }),
        PathClassification::Denied => Err(credential_path_denied(Path::new(path_str))),
        PathClassification::Outside => Err(format!(
            "Path '{}' is outside the working directory",
            canonical.display()
        )),
    }
}

/// Classify an already-canonicalized path against the shared grant table.
///
/// The credential denylist is checked first, then read-write grants, then
/// read-only grants. The same precedence decides existing-path validation and
/// prospective-write scheduling so overlapping grants resolve identically
/// whether or not the target exists yet (#277).
fn classify_resolved_path(canonical: &Path, grants: &sandbox::PathGrants) -> PathClassification {
    if grants.is_denied(canonical) {
        return PathClassification::Denied;
    }

    if grants
        .read_write
        .iter()
        .any(|dir| path_within(canonical, dir))
    {
        return PathClassification::Allowed(PathAccess::ReadWrite);
    }

    if grants
        .read_only
        .iter()
        .any(|dir| path_within(canonical, dir))
    {
        return PathClassification::Allowed(PathAccess::ReadOnly);
    }

    PathClassification::Outside
}

/// The single diagnostic for a path denied by the credential policy.
///
/// The message names the requested path and the policy classification but
/// never reads or includes the file contents.
fn credential_path_denied(path: &Path) -> String {
    format!(
        "Path '{}' is denied by the credential-path policy. In-process file tools do not expose credential-bearing files or directories.",
        path.display()
    )
}

/// Check if a canonical path starts with the given directory, comparing
/// against the directory's canonical form too so a symlinked grant
/// directory (e.g., /tmp → /private/tmp on macOS) matches either spelling.
fn path_within(canonical: &Path, dir: &Path) -> bool {
    if canonical.starts_with(dir) {
        return true;
    }
    dir.canonicalize()
        .is_ok_and(|canon_dir| canonical.starts_with(&canon_dir))
}

/// Validate that an existing read target is inside the shared grants.
///
/// Read-write and read-only grants are both accepted. This is a convenience
/// function for read operations that do not need to inspect the access level.
pub(super) fn validate_path_in_cwd(
    context: &ToolContext,
    path_str: &str,
) -> Result<std::path::PathBuf, String> {
    validate_path(context, path_str).map(|vp| vp.canonical)
}

/// Validate that an existing path is writable (inside a read-write grant).
/// Returns the canonical path if valid, or an error if the path is denied or
/// read-only. The read-only diagnostic retains its existing `--add-dir`
/// wording.
pub(super) fn validate_path_for_write(
    context: &ToolContext,
    path_str: &str,
) -> Result<std::path::PathBuf, String> {
    let validated = validate_path(context, path_str)?;
    if validated.access == PathAccess::ReadOnly {
        return Err(format!(
            "Path '{}' is read-only (added via --add-dir). Write operations are not allowed.",
            validated.canonical.display()
        ));
    }
    Ok(validated.canonical)
}

/// Walk a path root-to-leaf, resolving existing components via the filesystem
/// (following symlinks) and collecting non-existing components for lexical
/// normalisation.
///
/// Returns `(existing_base, pending_components)` where `existing_base` is the
/// deepest canonicalised prefix that exists on disk, and `pending_components`
/// is the lexically-normalised remainder.
///
/// `..` components before a non-existing segment are resolved through the
/// filesystem (preserving symlink semantics). `..` components within the
/// non-existing segment cancel a preceding pending normal component.
pub(super) fn resolve_write_path(path: &Path) -> (PathBuf, Vec<OsString>) {
    let mut resolved = PathBuf::new();
    let mut pending = Vec::new();
    let mut missing = false;

    for component in path.components() {
        resolve_write_path_component(component, &mut resolved, &mut pending, &mut missing);
    }

    (resolved, pending)
}

fn resolve_write_path_component(
    component: std::path::Component<'_>,
    resolved: &mut PathBuf,
    pending: &mut Vec<OsString>,
    missing: &mut bool,
) {
    match component {
        std::path::Component::RootDir => {
            *resolved = PathBuf::from("/");
            pending.clear();
            *missing = false;
        },
        std::path::Component::Prefix(prefix) => {
            resolved.push(prefix.as_os_str());
            pending.clear();
            *missing = false;
        },
        std::path::Component::CurDir => {},
        std::path::Component::Normal(name) => {
            resolve_normal_write_component(name, resolved, pending, missing);
        },
        std::path::Component::ParentDir => {
            resolve_parent_write_component(resolved, pending, missing);
        },
    }
}

fn resolve_normal_write_component(
    name: &OsStr,
    resolved: &mut PathBuf,
    pending: &mut Vec<OsString>,
    missing: &mut bool,
) {
    if *missing {
        pending.push(name.to_os_string());
        return;
    }

    let candidate = resolved.join(name);
    if candidate.exists() {
        *resolved = candidate.canonicalize().unwrap_or(candidate);
    } else {
        *missing = true;
        pending.push(name.to_os_string());
    }
}

fn resolve_parent_write_component(
    resolved: &mut PathBuf,
    pending: &mut Vec<OsString>,
    missing: &mut bool,
) {
    if !*missing {
        let candidate = resolved.join("..");
        if candidate.exists() {
            *resolved = candidate.canonicalize().unwrap_or(candidate);
        } else {
            *missing = true;
            pending.push(OsStr::new("..").to_os_string());
        }
    } else if !pending.is_empty() {
        pending.pop();
        if pending.is_empty() {
            *missing = false;
        }
    } else if resolved.as_os_str().is_empty() || resolved == Path::new("/") {
    } else {
        resolved.pop();
    }
}

/// Find a dangling symlink among the unresolved suffix of a write path.
///
/// `std::fs::write` follows a symlink and creates its target, so a dangling
/// link in the working directory could otherwise reach a credential path or
/// an ungranted location without the canonical target ever being validated.
fn dangling_symlink_below(base: &Path, pending: &[OsString]) -> Option<PathBuf> {
    let mut current = base.to_path_buf();
    for component in pending {
        current.push(component);
        let Ok(metadata) = std::fs::symlink_metadata(&current) else {
            continue;
        };
        if metadata.file_type().is_symlink() && current.canonicalize().is_err() {
            return Some(current);
        }
    }
    None
}

/// Resolve a write-target path for scheduling without requiring the file to
/// exist.  Returns a stable path key for grouping same-file mutations even
/// when the file hasn't been created yet.
///
/// For existing files, delegates to the standard `validate_path_for_write`.
/// For nonexistent paths, resolves the deepest existing ancestor via the
/// filesystem, canonicalises that, validates permissions, and appends the
/// lexically-normalised remainder.
pub(super) fn resolve_path_for_write_scheduling(
    context: &ToolContext,
    path_str: &str,
) -> Result<PathBuf, String> {
    let path = Path::new(path_str);

    if path.exists() {
        return validate_path_for_write(context, path_str);
    }

    resolve_nonexistent_path(context, path, false)
}

/// Resolve a read-target path for scheduling without requiring the file to
/// exist. Returns the same stable path key as
/// [`resolve_path_for_write_scheduling`] so a Read and a same-turn Write of a
/// file that does not exist yet share one serialization group instead of
/// racing on whether the read observes the file at all.
///
/// Read-write and read-only grants are accepted: reads observe them, while
/// writes reject read-only grants.
pub(super) fn resolve_path_for_read_scheduling(
    context: &ToolContext,
    path_str: &str,
) -> Result<PathBuf, String> {
    let path = Path::new(path_str);

    if path.exists() {
        return validate_path_in_cwd(context, path_str);
    }

    resolve_nonexistent_path(context, path, true)
}

/// Resolve the canonical key for a path that does not exist yet, validating
/// it against the shared grant table. Read-write grants are always accepted;
/// `allow_read_only` accepts read-only grants too.
fn resolve_nonexistent_path(
    context: &ToolContext,
    path: &Path,
    allow_read_only: bool,
) -> Result<PathBuf, String> {
    let (resolved_base, pending) = resolve_write_path(path);

    if let Some(link) = dangling_symlink_below(&resolved_base, &pending) {
        return Err(format!(
            "Path '{}' traverses a dangling symlink '{}' and cannot be written.",
            path.display(),
            link.display()
        ));
    }

    let mut final_path = resolved_base.clone();
    for component in &pending {
        final_path = final_path.join(component);
    }

    let grants = context.path_grants();
    if grants.is_denied(&final_path) {
        return Err(credential_path_denied(path));
    }

    let canonical_base = resolved_base.canonicalize().map_err(|e| {
        format!(
            "Parent directory not found '{}': {e}",
            resolved_base.display()
        )
    })?;

    match classify_resolved_path(&canonical_base, grants) {
        PathClassification::Allowed(PathAccess::ReadWrite) => {},
        PathClassification::Allowed(PathAccess::ReadOnly) if allow_read_only => {},
        PathClassification::Allowed(PathAccess::ReadOnly) => {
            return Err(format!(
                "Path '{}' is in a read-only directory (added via --add-dir). Write operations are not allowed.",
                path.display()
            ));
        },
        PathClassification::Denied => return Err(credential_path_denied(path)),
        PathClassification::Outside => {
            return Err(format!(
                "Path '{}' is outside the working directory",
                path.display()
            ));
        },
    }

    Ok(final_path)
}

fn execute_bash_tool(context: Arc<ToolContext>, call_id: String, arguments: String) -> ToolFuture {
    Box::pin(async move { bash::execute_bash_for_call(&context, &arguments, Some(call_id)).await })
}

fn execute_bash_session_tool(
    context: Arc<ToolContext>,
    _call_id: String,
    arguments: String,
) -> ToolFuture {
    Box::pin(async move { bash_session::execute(&context, &arguments).await })
}

fn execute_edit_tool(context: Arc<ToolContext>, _call_id: String, arguments: String) -> ToolFuture {
    Box::pin(async move {
        tokio::task::spawn_blocking(move || edit::execute_edit(&context, &arguments))
            .await
            .map_err(|e| ToolError::new(format!("Task join error: {e}")))?
            .map_err(ToolError::from)
    })
}

fn execute_read_tool(context: Arc<ToolContext>, _call_id: String, arguments: String) -> ToolFuture {
    Box::pin(async move {
        tokio::task::spawn_blocking(move || read::execute_read(&context, &arguments))
            .await
            .map_err(|e| ToolError::new(format!("Task join error: {e}")))?
            .map_err(ToolError::from)
    })
}

fn execute_read_image_tool(
    context: Arc<ToolContext>,
    _call_id: String,
    arguments: String,
) -> ToolFuture {
    Box::pin(async move {
        tokio::task::spawn_blocking(move || read_image::execute_read_image(&context, &arguments))
            .await
            .map_err(|e| ToolError::new(format!("Task join error: {e}")))?
            .map_err(ToolError::from)
    })
}

fn execute_write_tool(
    context: Arc<ToolContext>,
    _call_id: String,
    arguments: String,
) -> ToolFuture {
    Box::pin(async move {
        tokio::task::spawn_blocking(move || write::execute_write(&context, &arguments))
            .await
            .map_err(|e| ToolError::new(format!("Task join error: {e}")))?
            .map_err(ToolError::from)
    })
}

/// One tool discovered from an external server, for prompt listing.
///
/// This is deliberately independent of the live MCP connection so prompt
/// assembly needs no running server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalToolInfo {
    /// Registered model-visible name.
    pub name: String,
    /// Description advertised by the server.
    pub description: String,
}

/// One connected MCP server, for the compact prompt summary.
///
/// `enabled_tool_count` is already filtered by the active `--tools` selection,
/// so a server with no model-accessible tools is omitted from the summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalServerInfo {
    /// Local server name.
    pub name: String,
    /// Optional one-line description from configuration.
    pub description: Option<String>,
    /// Effective exposure for this run.
    pub exposure: McpExposure,
    /// Total tools discovered on the server.
    pub tool_count: usize,
    /// Tools remaining after the exact-name allowlist.
    pub enabled_tool_count: usize,
    /// Whether the server starts on demand and its tools are not yet known.
    pub lazy: bool,
}

/// Format the tool list with the optional `SkillSearch` built-in.
pub fn format_tool_list_section_with_mcp_and_skill_search(
    sandbox_policy: SandboxPolicy,
    toolbox_tools: &[crate::config::toolbox::ToolboxTool],
    mcp_servers: &[ExternalServerInfo],
    mcp_tools: &[ExternalToolInfo],
    enabled_tools: Option<&[String]>,
    supports_images: bool,
    has_skill_search: bool,
) -> String {
    let mut registry = default_tool_registry();
    registry.retain_model_supported_tools(supports_images);
    if sandbox_policy == SandboxPolicy::ReadOnly {
        registry.retain_read_safe_tools();
    }
    apply_enabled_tool_selection(&mut registry, enabled_tools);
    let mut s = String::from("## Available tools\n\n");
    let toolbox_lines = if sandbox_policy == SandboxPolicy::ReadOnly {
        &[]
    } else {
        toolbox_tools
    };
    let has_toolbox_tools = toolbox_lines
        .iter()
        .any(|tool| is_enabled_tool(&tool.registered_name, enabled_tools));
    let has_mcp_tools = mcp_tools
        .iter()
        .any(|tool| is_enabled_tool(&tool.name, enabled_tools));
    let summarized_servers: Vec<&ExternalServerInfo> = mcp_servers
        .iter()
        .filter(|server| {
            (server.enabled_tool_count > 0 || server.lazy) && server.exposure != McpExposure::Hidden
        })
        .collect();
    let has_deferred_tools = tool_search_enabled(enabled_tools)
        && summarized_servers
            .iter()
            .any(|server| server.exposure == McpExposure::Deferred);
    append_tool_lines(&mut s, &registry, toolbox_lines, mcp_tools, enabled_tools);
    if has_deferred_tools {
        s.push_str(
            "- **ToolSearch**: Search MCP tools that are not loaded yet and make matching \
             tools available on the next turn.\n",
        );
    }
    if has_skill_search {
        s.push_str(
            "- **SkillSearch**: Search the full skill catalog by name and description, \
             including skills omitted from the prompt. Returns the SKILL.md location to read.\n",
        );
    }
    append_mcp_server_summary(&mut s, &summarized_servers);
    let has_tools = has_any_enabled_tools(
        &registry,
        has_toolbox_tools,
        has_mcp_tools || !summarized_servers.is_empty(),
    ) || has_deferred_tools
        || has_skill_search;
    if has_tools {
        s.push_str(
            "\nEvery tool call may include an optional `intent` string: one short, \
             present-progressive sentence describing what the call does, written for the user. \
             Include it on every call when you can.\n",
        );
    }
    append_tool_availability_footer(&mut s, has_tools, has_deferred_tools);
    s
}

/// Append one model-visible line for each enabled built-in, toolbox, or MCP tool.
fn append_tool_lines(
    output: &mut String,
    registry: &ToolRegistry,
    toolbox_tools: &[crate::config::toolbox::ToolboxTool],
    mcp_tools: &[ExternalToolInfo],
    enabled_tools: Option<&[String]>,
) {
    for (name, desc) in registry
        .definitions()
        .iter()
        .map(|def| (&def.name, &def.description))
        .chain(
            toolbox_tools
                .iter()
                .filter(|tool| is_enabled_tool(&tool.registered_name, enabled_tools))
                .map(|tool| (&tool.registered_name, &tool.description)),
        )
        .chain(
            mcp_tools
                .iter()
                .filter(|tool| is_enabled_tool(&tool.name, enabled_tools))
                .map(|tool| (&tool.name, &tool.description)),
        )
    {
        let first_sentence = desc
            .split('.')
            .next()
            .unwrap_or(desc)
            .trim()
            .trim_end_matches('.');
        _ = writeln!(output, "- **{name}**: {first_sentence}.");
    }
}

/// Bound on one server description in the prompt summary.
const MAX_SERVER_DESCRIPTION_CHARS: usize = 200;

fn append_mcp_server_summary(s: &mut String, servers: &[&ExternalServerInfo]) {
    if servers.is_empty() {
        return;
    }
    s.push_str("\n## MCP servers\n\n");
    for server in servers {
        let tools = if server.lazy {
            "tools load on demand".to_string()
        } else {
            match server.enabled_tool_count {
                1 if server.tool_count == 1 => "1 tool".to_string(),
                count if count == server.tool_count => format!("{count} tools"),
                1 => format!("1 of {} tools", server.tool_count),
                count => format!("{count} of {} tools", server.tool_count),
            }
        };
        let mut line = format!(
            "- **{}** (`{}`, {tools})",
            server.name,
            server.exposure.as_str()
        );
        if let Some(description) = server.description.as_deref()
            && let Some(description) = concise_description(description)
        {
            line.push_str(": ");
            line.push_str(&description);
            line.push('.');
        }
        line.push('\n');
        s.push_str(&line);
    }
}

/// Collapse a description to one bounded sentence for the server summary.
fn concise_description(description: &str) -> Option<String> {
    let one_line = description.split_whitespace().collect::<Vec<_>>().join(" ");
    let first_sentence = one_line.split('.').next().unwrap_or(&one_line).trim();
    if first_sentence.is_empty() {
        return None;
    }
    let mut bounded: String = first_sentence
        .chars()
        .take(MAX_SERVER_DESCRIPTION_CHARS)
        .collect();
    if first_sentence.chars().count() > MAX_SERVER_DESCRIPTION_CHARS {
        bounded.push_str("...");
    }
    Some(bounded)
}

fn has_any_enabled_tools(
    registry: &ToolRegistry,
    has_toolbox_tools: bool,
    has_mcp_tools: bool,
) -> bool {
    !registry.definitions().is_empty() || has_toolbox_tools || has_mcp_tools
}

fn append_tool_availability_footer(s: &mut String, has_tools: bool, has_deferred_tools: bool) {
    if has_deferred_tools {
        s.push_str(
            "\nOnly these tools are available directly. Use `ToolSearch` to load tools from \
             deferred MCP servers. There is no Glob, Grep, or LS tool.",
        );
    } else if has_tools {
        s.push_str("\nOnly these tools are available. There is no Glob, Grep, or LS tool.");
    } else {
        s.push_str("\nNo tools are available.");
    }
}

const BUILTIN_TOOL_NAMES: &[&str] = &["Bash", "BashSession", "Read", "ReadImage", "Edit", "Write"];

fn filter_builtin_description(
    tool_name: &str,
    description: &str,
    available_names: &[String],
) -> String {
    if !["Bash", "Read", "ReadImage", "Edit", "Write"].contains(&tool_name) {
        return description.to_string();
    }

    let unavailable_names: Vec<&str> = BUILTIN_TOOL_NAMES
        .iter()
        .copied()
        .filter(|name| {
            *name != tool_name && !available_names.iter().any(|available| available == name)
        })
        .collect();
    if unavailable_names.is_empty() {
        return description.to_string();
    }

    description
        .split_inclusive('\n')
        .filter(|line| {
            !unavailable_names
                .iter()
                .any(|name| mentions_tool(line, name))
        })
        .collect()
}

/// Whether `line` names the tool `name` as a whole word.
///
/// Built-in names share prefixes --- `Read`/`ReadImage` and `Bash`/`BashSession`
/// --- so a plain substring match would treat a reference to `ReadImage` as a
/// reference to `Read` and drop a line that is still accurate.
fn mentions_tool(line: &str, name: &str) -> bool {
    line.split(|c: char| !is_tool_name_char(c))
        .any(|word| word == name)
}

const fn is_tool_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn apply_enabled_tool_selection(registry: &mut ToolRegistry, enabled_tools: Option<&[String]>) {
    if let Some(enabled_tools) = enabled_tools {
        registry.retain_enabled_tools(enabled_tools);
    }
}

fn is_enabled_tool(name: &str, enabled_tools: Option<&[String]>) -> bool {
    enabled_tools.is_none_or(|names| names.iter().any(|enabled| enabled == name))
}

/// Why a built-in tool is absent from the effective registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinToolFilterReason {
    /// The read-only sandbox policy removes every tool that can mutate.
    ReadOnlyPolicy,
    /// The resolved model cannot accept image input.
    ModelLacksImageSupport,
    /// The exact-name selection does not include the tool.
    NotSelected,
    /// `--no-tools` selected no tools.
    NoTools,
}

impl BuiltinToolFilterReason {
    /// Human- and machine-readable label for the filter.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ReadOnlyPolicy => "read-only policy",
            Self::ModelLacksImageSupport => "model has no image support",
            Self::NotSelected => "not in --tools",
            Self::NoTools => "--no-tools",
        }
    }
}

/// One built-in tool's effective status for a read-only inventory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltinToolInventoryEntry {
    /// Built-in tool name.
    pub name: &'static str,
    /// Whether a run would expose the tool.
    pub enabled: bool,
    /// The first filter that removed the tool, when it is disabled.
    pub reason: Option<BuiltinToolFilterReason>,
}

/// Report every built-in tool after the same filters a run applies.
///
/// This keeps the filtered tools so a read-only diagnostic can explain why
/// each one is unavailable. The filters run in the same order as an agent:
/// model capability, read-only sandbox policy, then exact-name selection.
pub fn builtin_tool_inventory(
    sandbox_policy: SandboxPolicy,
    supports_images: bool,
    enabled_tools: Option<&[String]>,
) -> Vec<BuiltinToolInventoryEntry> {
    let mut registry = default_tool_registry();
    let mut filtered = Vec::new();

    let before = registry.names();
    registry.retain_model_supported_tools(supports_images);
    record_removed_builtins(
        &mut filtered,
        &before,
        &registry.names(),
        BuiltinToolFilterReason::ModelLacksImageSupport,
    );

    if sandbox_policy == SandboxPolicy::ReadOnly {
        let before = registry.names();
        registry.retain_read_safe_tools();
        record_removed_builtins(
            &mut filtered,
            &before,
            &registry.names(),
            BuiltinToolFilterReason::ReadOnlyPolicy,
        );
    }

    if let Some(enabled_tools) = enabled_tools {
        let before = registry.names();
        apply_enabled_tool_selection(&mut registry, Some(enabled_tools));
        let reason = if enabled_tools.is_empty() {
            BuiltinToolFilterReason::NoTools
        } else {
            BuiltinToolFilterReason::NotSelected
        };
        record_removed_builtins(&mut filtered, &before, &registry.names(), reason);
    }

    let enabled_names = registry.names();
    BUILTIN_TOOL_NAMES
        .iter()
        .map(|name| {
            let enabled = enabled_names
                .iter()
                .any(|enabled_name| enabled_name.as_str() == *name);
            let reason = if enabled {
                None
            } else {
                filtered.iter().find_map(|(filtered_name, reason)| {
                    (*filtered_name == *name).then_some(*reason)
                })
            };
            BuiltinToolInventoryEntry {
                name,
                enabled,
                reason,
            }
        })
        .collect()
}

/// Record the built-ins one registry filter removed, preserving the first
/// reason when a later filter would have removed the same tool again.
fn record_removed_builtins(
    filtered: &mut Vec<(&'static str, BuiltinToolFilterReason)>,
    before: &[String],
    after: &[String],
    reason: BuiltinToolFilterReason,
) {
    for name in BUILTIN_TOOL_NAMES {
        let was_present = before.iter().any(|present| present.as_str() == *name);
        let is_present = after.iter().any(|present| present.as_str() == *name);
        if was_present && !is_present {
            filtered.push((name, reason));
        }
    }
}

/// Returns the default tool registry.
///
/// Each entry declares its capabilities here; scheduling, sandbox filtering,
/// and hook payload shaping consume those declarations instead of matching on
/// tool names (#277).
pub(super) fn default_tool_registry() -> ToolRegistry {
    let entries = vec![
        ToolEntry::new(bash::bash_tool(), execute_bash_tool).tolerates_intent(),
        ToolEntry::new(bash_session::bash_session_tool(), execute_bash_session_tool)
            .tolerates_intent(),
        ToolEntry::new(edit::edit_tool(), execute_edit_tool)
            .repairs_arguments()
            .mutates_path(edit::mutating_target),
        ToolEntry::new(read::read_tool(), execute_read_tool)
            .tolerates_intent()
            .read_safe()
            .replay_safe()
            .observes_path(read::path_target),
        ToolEntry::new(read_image::read_image_tool(), execute_read_image_tool)
            .tolerates_intent()
            .read_safe()
            .replay_safe()
            .observes_path(read_image::path_target)
            .requires_images(),
        ToolEntry::new(write::write_tool(), execute_write_tool)
            .tolerates_intent()
            .repairs_arguments()
            .mutates_path(write::mutating_target),
    ];
    let mut registry = ToolRegistry {
        entries,
        definitions: Vec::new(),
        revision: 0,
    };
    registry.refresh_definitions();
    registry
}
