//! Tool definitions and execution for the AI agent.
//!
//! This module provides the tool interface that allows the AI agent to interact
//! with the host system through controlled operations. All tools are sandboxed
//! to restrict file access to the working directory and allowed paths.
//!
//! # Available Tools
//!
//! - `Bash` - Execute shell commands with timeout and output capture
//! - `Read` - Read file contents with line range support
//! - `Edit` - Make targeted edits to files using literal search-replace
//! - `Write` - Create or overwrite files with content
//!
//! User-defined toolbox tools (`tb__*`, see `config::toolbox` and the
//! `toolbox` submodule) can be registered alongside the built-ins.
//!
//! # Security
//!
//! All tools validate paths against the current working directory and
//! directories added via `--add-dir` flag. Write operations are only allowed
//! in the working directory and temp directories. Toolbox tools are
//! user-provided trusted executables that run outside the OS sandbox and are
//! never registered under the read-only sandbox policy.

use serde::Serialize;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use crate::clients::judge::JudgeContext;
use crate::session_telemetry::{CompensationEventTelemetry, CompensationKind};

mod sandbox;

pub use sandbox::{SandboxPolicy, resolve_linked_worktree_dirs, resolve_sandbox_policy};

fn compute_temp_directories() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // Include symlink path first, then canonical path.
    // On macOS, /tmp -> /private/tmp and /var/folders -> /private/var/folders.
    // Both forms are needed so that ancestor literals and subpath rules
    // cover the paths regardless of which form a process uses.
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
    pub temp_dirs: Vec<PathBuf>,
    pub additional_dirs: Vec<PathBuf>,
    pub skill_dirs: Vec<PathBuf>,
    pub settings_dirs: Vec<PathBuf>,
    /// Resolved sandbox policy applied to model-generated shell commands.
    pub sandbox_policy: SandboxPolicy,
    /// LLM-judge command-safety context for the Bash preflight. `None` when
    /// the run has no judge configured; the Bash tool fails closed on an
    /// absent context rather than running a command ungated.
    pub judge: Option<Arc<JudgeContext>>,
}

impl ToolContext {
    /// Build a tool context using the same temp directory discovery as the
    /// existing process-global cache.
    pub fn new(
        cwd: PathBuf,
        additional_dirs: Vec<PathBuf>,
        skill_dirs: Vec<PathBuf>,
        settings_dirs: Vec<PathBuf>,
        sandbox_policy: SandboxPolicy,
    ) -> Self {
        let mut context = Self::with_temp_dirs(
            cwd,
            compute_temp_directories(),
            additional_dirs,
            skill_dirs,
            settings_dirs,
        );
        context.sandbox_policy = sandbox_policy;
        context
    }

    /// Attach the LLM-judge command-safety context used by the Bash preflight.
    pub fn with_judge(mut self, judge: Option<Arc<JudgeContext>>) -> Self {
        self.judge = judge;
        self
    }

    /// Build a tool context with explicitly supplied temp directories.
    ///
    /// This keeps construction testable without depending on process-global
    /// cache state. The sandbox policy defaults to `WorkspaceWrite` so the
    /// many existing test call sites need not pass it explicitly.
    pub const fn with_temp_dirs(
        cwd: PathBuf,
        temp_dirs: Vec<PathBuf>,
        additional_dirs: Vec<PathBuf>,
        skill_dirs: Vec<PathBuf>,
        settings_dirs: Vec<PathBuf>,
    ) -> Self {
        Self {
            cwd,
            temp_dirs,
            additional_dirs,
            skill_dirs,
            settings_dirs,
            sandbox_policy: SandboxPolicy::WorkspaceWrite,
            judge: None,
        }
    }

    /// Build a context from the current process environment with no configured
    /// extra directories.
    pub(crate) fn from_current_process() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            cwd,
            temp_dirs: compute_temp_directories(),
            additional_dirs: Vec::new(),
            skill_dirs: Vec::new(),
            settings_dirs: Vec::new(),
            sandbox_policy: SandboxPolicy::WorkspaceWrite,
            judge: None,
        }
    }
}

/// Get the additional directories from the current tool context.
pub fn get_additional_dirs(context: &ToolContext) -> &[PathBuf] {
    &context.additional_dirs
}

/// Get the skill directories from the current tool context.
pub fn get_skill_dirs(context: &ToolContext) -> &[PathBuf] {
    &context.skill_dirs
}

/// Get the settings directories from the current tool context.
pub fn get_settings_dirs(context: &ToolContext) -> &[PathBuf] {
    &context.settings_dirs
}

// =============================================================================
// Module Declarations
// =============================================================================

mod bash;
mod edit;
mod json_repair;
mod read;
mod scheduling;
mod secure_temp_dir;
mod toolbox;
mod write;

pub(super) use json_repair::repair_json_args;
pub use read::extract_path as read_extract_path;
pub(super) use scheduling::{ScheduledToolCall, schedule_tool_calls};
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
) -> Vec<CompensationEventTelemetry> {
    let mut events = Vec::new();
    if registered && uses_repair_pass(name) {
        let repaired = repair_json_args(arguments);
        if repaired != arguments && serde_json::from_str::<serde_json::Value>(&repaired).is_ok() {
            events.push(CompensationEventTelemetry::new(
                CompensationKind::JsonRepair,
                Some(name.to_string()),
            ));
        }
    }
    if was_error && registered && name == "Edit" && edit::arguments_are_invalid(arguments) {
        events.push(CompensationEventTelemetry::new(
            CompensationKind::EditInvalidArguments,
            None,
        ));
    }
    events
}

fn uses_repair_pass(name: &str) -> bool {
    name == "Edit" || name == "Write" || name.starts_with(crate::config::toolbox::TOOLBOX_PREFIX)
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

// =============================================================================
// JSON Parse Error Formatting
// =============================================================================

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

    // The error may be at or past the end of the last line (EOF).
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

    // Use safe string slicing via `.get()` which returns `None` (falling back to
    // empty) when the range doesn't fall on UTF-8 character boundaries.
    let before = payload.get(start..offset).unwrap_or_default();
    let after = payload.get(offset..end).unwrap_or_default();

    let escaped_before = escape_json_context(before);
    let escaped_after = escape_json_context(after);

    // Show a 4-space indent before the snippet
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
            // Remaining C0 control chars (U+0001-U+001F) not matched above:
            // vertical tab, form feed, shift-in through unit separator.
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
    // Extract just the message portion before " at line ... column ..."
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

#[cfg(test)]
mod json_parse_error_tests {
    use super::*;

    // ── json_error_hint branch coverage ──

    #[test]
    fn hint_control_char() {
        let msg =
            "control character (\\u0000-\\u001F) found while parsing a string at line 1 column 5";
        assert!(json_error_hint(msg).contains("raw control character"));
    }

    #[test]
    fn hint_trailing_characters() {
        let msg = "trailing characters at line 1 column 42";
        assert!(json_error_hint(msg).contains("Content continued"));
    }

    #[test]
    fn hint_trailing_data() {
        let msg = "trailing data at line 1 column 10";
        assert!(json_error_hint(msg).contains("Content continued"));
    }

    #[test]
    fn hint_missing_field_path() {
        let msg = "missing field `path` at line 1 column 10";
        assert!(json_error_hint(msg).contains("`path` field is required"));
    }

    #[test]
    fn hint_missing_field_other() {
        let msg = "missing field `old_text` at line 1 column 42";
        assert!(json_error_hint(msg).contains("A required field is missing"));
    }

    #[test]
    fn hint_unknown_field() {
        let msg = "unknown field `foo` at line 1 column 10";
        assert!(json_error_hint(msg).contains("unrecognized field name"));
    }

    #[test]
    fn hint_key_must_be_string() {
        let msg = "key must be a string at line 1 column 5";
        assert!(json_error_hint(msg).contains("double-quoted strings"));
    }

    #[test]
    fn hint_eof() {
        let msg = "EOF while parsing a string at line 1 column 42";
        assert!(json_error_hint(msg).contains("ended unexpectedly"));
    }

    #[test]
    fn hint_eof_end_of_input() {
        let msg = "end of input at line 1 column 42";
        assert!(json_error_hint(msg).contains("ended unexpectedly"));
    }

    #[test]
    fn hint_default() {
        let msg = "expected `,` or `}` at line 1 column 10";
        assert!(json_error_hint(msg).contains("Check the payload structure"));
    }

    #[test]
    fn hint_no_line_column() {
        // Error message without " at line ..." suffix
        let msg = "some random io error";
        assert!(json_error_hint(msg).contains("Check the payload structure"));
    }

    // ── line_col_to_offset coverage ──

    #[test]
    fn line_col_to_offset_first_line() {
        let s = "hello";
        assert_eq!(line_col_to_offset(s, 1, 1), Some(0));
        assert_eq!(line_col_to_offset(s, 1, 3), Some(2));
    }

    #[test]
    fn line_col_to_offset_second_line() {
        let s = "abc\ndef";
        assert_eq!(line_col_to_offset(s, 2, 1), Some(4));
        assert_eq!(line_col_to_offset(s, 2, 3), Some(6));
    }

    #[test]
    fn line_col_to_offset_at_eof() {
        // serde reports column at past-the-end for EOF errors
        let s = "abc";
        assert_eq!(line_col_to_offset(s, 1, 4), Some(3));
    }

    #[test]
    fn line_col_to_offset_past_eof_returns_none() {
        let s = "abc";
        assert_eq!(line_col_to_offset(s, 1, 999), None);
    }

    #[test]
    fn line_col_to_offset_line_not_found_returns_none() {
        let s = "abc";
        assert_eq!(line_col_to_offset(s, 99, 1), None);
    }

    // ── escape_json_context coverage ──

    #[test]
    fn escape_context_escapes_form_feed() {
        // U+000C (form feed) is ASCII whitespace per Rust; verify it is escaped.
        assert_eq!(escape_json_context("\x0C"), "\\u{000C}");
    }

    #[test]
    fn escape_context_passes_normal_chars() {
        assert_eq!(escape_json_context("hello world"), "hello world");
        assert_eq!(escape_json_context("{\"a\":1}"), "{\"a\":1}");
    }

    #[test]
    fn escape_context_escapes_control_chars() {
        assert_eq!(escape_json_context("\0"), "\\0");
        assert_eq!(escape_json_context("\t"), "\\t");
        assert_eq!(escape_json_context("\n"), "\\n");
        assert_eq!(escape_json_context("\r"), "\\r");
    }

    #[test]
    fn escape_context_escapes_soh() {
        // U+0001 is not whitespace, not newline/tab/cr
        assert_eq!(escape_json_context("\x01"), "\\u{0001}");
    }

    // ── extract_json_error_context coverage ──

    #[test]
    fn extract_context_empty_payload() {
        // Empty payload yields an empty context window
        let result = extract_json_error_context("", 1, 1);
        assert!(result.is_some());
        let (_snippet, caret) = result.unwrap();
        assert!(caret.contains('^'), "caret should appear in empty payload");
    }

    #[test]
    fn extract_context_short_payload() {
        let (snippet, caret) = extract_json_error_context("{\"a\":1}", 1, 5).unwrap();
        assert!(snippet.contains("{\"a\":1}"));
        assert!(caret.contains('^'));
    }

    #[test]
    fn extract_context_with_control_char() {
        // Trailing null byte
        let payload = "{\"a\":1}\x00";
        let (snippet, caret) = extract_json_error_context(payload, 1, 9).unwrap();
        assert!(snippet.contains("\\0"), "null should be escaped: {snippet}");
        assert!(caret.contains('^'));
    }

    // ── format_json_parse_error integration ──

    #[test]
    fn format_error_empty_payload() {
        // Empty payload: line_col_to_offset returns None, context is skipped
        let payload = "";
        let err = serde_json::from_str::<serde_json::Value>(payload).unwrap_err();
        let msg = format_json_parse_error(payload, &err, "test", "{\"a\":1}");
        assert!(msg.contains("Invalid test arguments"));
        assert!(msg.contains("Expected shape"));
        // No Context: section since context extraction returned None
    }

    #[test]
    fn format_error_with_invalid_syntax() {
        // Payload with a syntax error so serde fails
        let payload = "{\"a\": }";
        let err = serde_json::from_str::<serde_json::Value>(payload).unwrap_err();
        let msg = format_json_parse_error(payload, &err, "test", "{\"a\":1}");
        assert!(msg.contains("Invalid test arguments"));
        assert!(msg.contains("Hint:"));
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

/// Result of executing a tool.
///
/// Contains the output string from tool execution, which may be stdout/stderr
/// for Bash or file contents for Read operations, plus any model-compensation
/// events observed while running the tool (recorded to session telemetry by
/// the agent loop).
#[derive(Debug)]
pub struct ToolResult {
    pub output: String,
    pub compensation_events: Vec<CompensationEventTelemetry>,
}

/// Error from executing a tool.
///
/// Carries the model-visible message plus any model-compensation events
/// observed while the tool failed (for example a judge `block` verdict or a
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

impl From<String> for ToolError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

impl From<&str> for ToolError {
    fn from(message: &str) -> Self {
        Self::new(message.to_string())
    }
}

type ToolFuture = Pin<Box<dyn Future<Output = Result<ToolResult, ToolError>> + Send>>;
type ToolExecutor = Arc<dyn Fn(Arc<ToolContext>, String, String) -> ToolFuture + Send + Sync>;

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
}

impl ToolEntry {
    fn new(
        definition: Tool,
        execute: impl Fn(Arc<ToolContext>, String, String) -> ToolFuture + Send + Sync + 'static,
    ) -> Self {
        Self {
            definition,
            execute: Arc::new(execute),
        }
    }
}

/// Registry of tools available to an agent.
#[derive(Clone)]
pub(super) struct ToolRegistry {
    entries: Vec<ToolEntry>,
    /// Cached model-facing tool definitions, computed once at construction.
    definitions: Vec<Tool>,
}

impl ToolRegistry {
    /// Build a registry from explicit entries.
    #[cfg(test)]
    pub(super) fn new(entries: Vec<ToolEntry>) -> Self {
        let definitions = entries.iter().map(|e| e.definition.clone()).collect();
        Self {
            entries,
            definitions,
        }
    }

    /// Return an empty registry, useful for tests that do not expose tools.
    #[cfg(test)]
    pub(super) const fn empty() -> Self {
        Self {
            entries: Vec::new(),
            definitions: Vec::new(),
        }
    }

    /// Return the cached model-facing tool definitions.
    pub(super) fn definitions(&self) -> &[Tool] {
        &self.definitions
    }

    /// Remove the tools that mutate files in-process (Edit, Write) and all
    /// user-defined toolbox tools (`tb__*`).
    ///
    /// The read-only sandbox policy uses this so the model never sees tools
    /// it cannot use: Edit and Write bypass the OS sandbox (which only wraps
    /// Bash), and toolbox tools run as unsandboxed external processes, so
    /// omitting them is what makes the policy's no-mutation guarantee hold
    /// for the whole agent, not just shell commands.
    pub(super) fn retain_read_safe_tools(&mut self) {
        self.entries.retain(|entry| {
            !matches!(entry.definition.name.as_str(), "Edit" | "Write")
                && !entry
                    .definition
                    .name
                    .starts_with(crate::config::toolbox::TOOLBOX_PREFIX)
        });
        self.definitions = self.entries.iter().map(|e| e.definition.clone()).collect();
    }

    /// Append a tool entry and refresh the cached definitions.
    pub(super) fn push_entry(&mut self, entry: ToolEntry) {
        self.definitions.push(entry.definition.clone());
        self.entries.push(entry);
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

        (entry.execute)(context, call_id.to_string(), arguments.to_string()).await
    }

    /// Return whether a tool with `name` is registered.
    pub(super) fn has(&self, name: &str) -> bool {
        self.find(name).is_some()
    }

    /// Return the canonical path a mutating path-aware tool would write.
    pub(super) fn mutating_target(
        &self,
        context: &ToolContext,
        name: &str,
        arguments: &str,
    ) -> Option<Result<PathBuf, String>> {
        match self.find(name)?.definition.name.as_str() {
            "Edit" => Some(edit::mutating_target(context, arguments)),
            "Write" => Some(write::mutating_target(context, arguments)),
            _ => None,
        }
    }

    fn find(&self, name: &str) -> Option<&ToolEntry> {
        self.entries
            .iter()
            .find(|entry| entry.definition.name == name)
    }
}

// =============================================================================
// Path Validation
// =============================================================================

/// Access level for a validated path
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PathAccess {
    /// Path is in a read-write location (cwd, temp dirs)
    ReadWrite,
    /// Path is in a read-only location (--add-dir directories)
    ReadOnly,
}

/// Result of path validation containing the canonical path and access level
#[derive(Debug)]
pub(super) struct ValidatedPath {
    pub canonical: std::path::PathBuf,
    pub access: PathAccess,
}

/// Validate that a path exists and is within the current working directory, allowed temp directories,
/// or directories added via --add-dir flag (read-only access).
///
/// Returns the canonical path along with its access level.
pub(super) fn validate_path(
    context: &ToolContext,
    path_str: &str,
) -> Result<ValidatedPath, String> {
    validate_path_with_dirs(
        path_str,
        &context.cwd,
        &context.temp_dirs,
        get_settings_dirs(context),
        get_additional_dirs(context),
        get_skill_dirs(context),
    )
}

/// Core path validation logic, separated for testability.
fn validate_path_with_dirs(
    path_str: &str,
    cwd: &Path,
    temp_dirs: &[PathBuf],
    settings_dirs: &[PathBuf],
    additional_dirs: &[PathBuf],
    skill_dirs: &[PathBuf],
) -> Result<ValidatedPath, String> {
    let path = Path::new(path_str);

    // Canonicalize the path (resolve symlinks, relative paths, etc.)
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("Path not found or not accessible '{}': {e}", path.display()))?;

    // Check if path is within working directory (read-write)
    if path_starts_with(&canonical, &[cwd.to_path_buf()]) {
        return Ok(ValidatedPath {
            canonical,
            access: PathAccess::ReadWrite,
        });
    }

    // Allow paths in standard temp directories (read-write)
    if path_starts_with(&canonical, temp_dirs) {
        return Ok(ValidatedPath {
            canonical,
            access: PathAccess::ReadWrite,
        });
    }

    // Allow paths in settings directories from settings.toml (read-write)
    if path_starts_with(&canonical, settings_dirs) {
        return Ok(ValidatedPath {
            canonical,
            access: PathAccess::ReadWrite,
        });
    }

    // Allow paths in directories added via --add-dir flag (read-only)
    if path_starts_with(&canonical, additional_dirs) {
        return Ok(ValidatedPath {
            canonical,
            access: PathAccess::ReadOnly,
        });
    }

    // Allow paths in skill directories (read-only)
    if path_starts_with(&canonical, skill_dirs) {
        return Ok(ValidatedPath {
            canonical,
            access: PathAccess::ReadOnly,
        });
    }

    Err(format!(
        "Path '{}' is outside the working directory",
        canonical.display()
    ))
}

/// Check if a canonical path starts with any of the given directories.
/// Each directory is canonicalized before comparison to handle symlinks
/// (e.g., /tmp → /private/tmp on macOS).
fn path_starts_with(canonical: &Path, dirs: &[PathBuf]) -> bool {
    dirs.iter().any(|dir| {
        // Try canonical form first (fast path when no symlinks involved)
        if canonical.starts_with(dir) {
            return true;
        }
        // Also try the canonicalized form of the dir
        dir.canonicalize()
            .is_ok_and(|canon_dir| canonical.starts_with(&canon_dir))
    })
}

/// Validate that a path exists and is within the current working directory, allowed temp directories,
/// or directories added via --add-dir flag (read-only access).
///
/// This is a convenience function for read operations that don't need to check access level.
pub(super) fn validate_path_in_cwd(
    context: &ToolContext,
    path_str: &str,
) -> Result<std::path::PathBuf, String> {
    validate_path(context, path_str).map(|vp| vp.canonical)
}

/// Validate that a path is writable (not in a read-only additional directory).
/// Returns the canonical path if valid, or an error if the path is read-only.
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
    let mut pending: Vec<OsString> = Vec::new();
    let mut missing = false;

    for component in path.components() {
        match component {
            std::path::Component::RootDir => {
                resolved = PathBuf::from("/");
                pending.clear();
                missing = false;
            },
            std::path::Component::Prefix(p) => {
                resolved.push(p.as_os_str());
                pending.clear();
                missing = false;
            },
            std::path::Component::CurDir => {
                // skip `.`
            },
            std::path::Component::Normal(name) => {
                if missing {
                    pending.push(name.to_os_string());
                } else {
                    let candidate = resolved.join(name);
                    if candidate.exists() {
                        // Follow symlinks by canonicalizing
                        resolved = candidate.canonicalize().unwrap_or(candidate);
                    } else {
                        missing = true;
                        pending.push(name.to_os_string());
                    }
                }
            },
            std::path::Component::ParentDir => {
                if !missing {
                    let candidate = resolved.join("..");
                    if candidate.exists() {
                        resolved = candidate.canonicalize().unwrap_or(candidate);
                    } else {
                        missing = true;
                        pending.push(OsStr::new("..").to_os_string());
                    }
                } else if !pending.is_empty() {
                    // `..` in the non-existing segment cancels the last
                    // pending normal component. If pending is now empty,
                    // resume filesystem resolution so that subsequent
                    // existing symlinks are canonicalized correctly.
                    pending.pop();
                    if pending.is_empty() {
                        missing = false;
                    }
                } else if resolved.as_os_str().is_empty() || resolved == Path::new("/") {
                    // `..` at root or empty base is a no-op
                } else {
                    // `..` above a fully-resolved non-root base — go up
                    resolved.pop();
                }
            },
        }
    }

    (resolved, pending)
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

    // If the file exists, use the shared validation which canonicalizes and
    // checks read-only status.
    if path.exists() {
        return validate_path_for_write(context, path_str);
    }

    // Resolve the path root-to-leaf with symlink-aware semantics for existing
    // components. Non-existing suffixes are normalised lexically so that
    // parent-directory components across nonexistent ancestors are handled
    // correctly (<cwd>/missing/../target → <cwd>/target).
    let (resolved_base, pending) = resolve_write_path(path);

    // Validate the resolved base is within allowed directories. Compare via
    // `path_starts_with` (which canonicalizes each dir) so a symlinked cwd,
    // temp, or settings dir is accepted for new files exactly as the
    // existing-file path accepts it in `validate_path_with_dirs`.
    let canonical_base = resolved_base.canonicalize().map_err(|e| {
        format!(
            "Parent directory not found '{}': {e}",
            resolved_base.display()
        )
    })?;

    let is_in_cwd = path_starts_with(&canonical_base, std::slice::from_ref(&context.cwd));
    let is_in_temp = path_starts_with(&canonical_base, get_temp_directories(context));
    let is_in_settings = path_starts_with(&canonical_base, get_settings_dirs(context));
    let is_in_read_only = path_starts_with(&canonical_base, get_additional_dirs(context));

    if is_in_read_only {
        return Err(format!(
            "Path '{}' is in a read-only directory (added via --add-dir). Write operations are not allowed.",
            path.display()
        ));
    }

    if !is_in_cwd && !is_in_temp && !is_in_settings {
        return Err(format!(
            "Path '{}' is outside the working directory",
            path.display()
        ));
    }

    // Reconstruct the full path: resolved base + pending components
    let mut final_path = resolved_base;
    for component in &pending {
        final_path = final_path.join(component);
    }
    Ok(final_path)
}

/// Get standard temporary directory paths (cached)
pub(super) fn get_temp_directories(context: &ToolContext) -> &[PathBuf] {
    &context.temp_dirs
}

// =============================================================================
// Tool Execution
// =============================================================================

fn execute_bash_tool(context: Arc<ToolContext>, call_id: String, arguments: String) -> ToolFuture {
    Box::pin(async move { bash::execute_bash_for_call(&context, &arguments, Some(call_id)).await })
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

// =============================================================================
// Tool Registry
// =============================================================================

/// Generate the "Available tools" section for the built-in system prompt.
///
/// This is derived from the tool registry for the given sandbox policy so
/// that prompt text stays in sync with the actual set of registered tools
/// (under `ReadOnly`, Edit and Write are not registered, and toolbox tools
/// are excluded because they run unsandboxed). Each tool's one-line summary
/// is its first sentence (up to the first `.`).
pub fn format_tool_list_section(
    sandbox_policy: SandboxPolicy,
    toolbox_tools: &[crate::config::toolbox::ToolboxTool],
) -> String {
    let mut registry = default_tool_registry();
    if sandbox_policy == SandboxPolicy::ReadOnly {
        registry.retain_read_safe_tools();
    }
    let mut s = String::from("## Available tools\n\n");
    let toolbox_lines = if sandbox_policy == SandboxPolicy::ReadOnly {
        &[]
    } else {
        toolbox_tools
    };
    for (name, desc) in registry
        .definitions()
        .iter()
        .map(|def| (&def.name, &def.description))
        .chain(
            toolbox_lines
                .iter()
                .map(|tool| (&tool.registered_name, &tool.description)),
        )
    {
        let first_sentence = desc
            .split('.')
            .next()
            .unwrap_or(desc)
            .trim()
            .trim_end_matches('.');
        _ = writeln!(s, "- **{name}**: {first_sentence}.");
    }
    s.push_str("\nOnly these tools are available. There is no Glob, Grep, or LS tool.");
    s
}

/// Returns the default tool registry.
pub(super) fn default_tool_registry() -> ToolRegistry {
    let entries = vec![
        ToolEntry::new(bash::bash_tool(), execute_bash_tool),
        ToolEntry::new(edit::edit_tool(), execute_edit_tool),
        ToolEntry::new(read::read_tool(), execute_read_tool),
        ToolEntry::new(write::write_tool(), execute_write_tool),
    ];
    let definitions = entries.iter().map(|e| e.definition.clone()).collect();
    ToolRegistry {
        entries,
        definitions,
    }
}

/// Returns a registry containing only the Read tool.
#[cfg(test)]
pub(super) fn read_tool_registry() -> ToolRegistry {
    ToolRegistry::new(vec![ToolEntry::new(read::read_tool(), execute_read_tool)])
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn tool_context_with_temp_dirs_preserves_inputs() {
        let cwd = PathBuf::from("/workspace/project");
        let temp_dirs = vec![PathBuf::from("/tmp"), PathBuf::from("/private/tmp")];
        let additional_dirs = vec![PathBuf::from("/workspace/reference")];
        let skill_dirs = vec![PathBuf::from("/workspace/.agents/skills/example")];
        let settings_dirs = vec![PathBuf::from("/workspace/.cake")];

        let context = ToolContext::with_temp_dirs(
            cwd.clone(),
            temp_dirs.clone(),
            additional_dirs.clone(),
            skill_dirs.clone(),
            settings_dirs.clone(),
        );

        assert_eq!(context.cwd, cwd);
        assert_eq!(context.temp_dirs, temp_dirs);
        assert_eq!(context.additional_dirs, additional_dirs);
        assert_eq!(context.skill_dirs, skill_dirs);
        assert_eq!(context.settings_dirs, settings_dirs);
    }

    #[test]
    fn tool_context_construction_is_repeatable_with_explicit_temp_dirs() {
        let first = ToolContext::with_temp_dirs(
            PathBuf::from("/workspace/project"),
            vec![PathBuf::from("/tmp")],
            vec![PathBuf::from("/workspace/reference")],
            vec![PathBuf::from("/workspace/skills")],
            vec![PathBuf::from("/workspace/settings")],
        );
        let second = ToolContext::with_temp_dirs(
            PathBuf::from("/workspace/project"),
            vec![PathBuf::from("/tmp")],
            vec![PathBuf::from("/workspace/reference")],
            vec![PathBuf::from("/workspace/skills")],
            vec![PathBuf::from("/workspace/settings")],
        );

        assert_eq!(first.cwd, second.cwd);
        assert_eq!(first.temp_dirs, second.temp_dirs);
        assert_eq!(first.additional_dirs, second.additional_dirs);
        assert_eq!(first.skill_dirs, second.skill_dirs);
        assert_eq!(first.settings_dirs, second.settings_dirs);
        assert_eq!(first.sandbox_policy, second.sandbox_policy);
        assert!(first.judge.is_none(), "no judge context by default");
        assert!(second.judge.is_none(), "no judge context by default");
    }

    /// Verify that `validate_path_with_dirs` accepts paths within skill directories.
    #[test]
    fn skill_dir_path_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("fetching-x-content");
        fs::create_dir_all(&skill_dir).unwrap();

        // Create a SKILL.md and a script file in the skill directory
        let skill_file = skill_dir.join("SKILL.md");
        let script_file = skill_dir.join("scripts").join("x-fetch.js");
        fs::create_dir_all(script_file.parent().unwrap()).unwrap();
        fs::write(&skill_file, "# Skill content").unwrap();
        fs::write(&script_file, "// script content").unwrap();

        let cwd = tmp.path().join("project");
        fs::create_dir_all(&cwd).unwrap();

        let result = validate_path_with_dirs(
            skill_file.to_str().unwrap(),
            &cwd,
            &[],
            &[],
            &[],
            std::slice::from_ref(&skill_dir),
        );
        assert!(
            result.is_ok(),
            "Skill file should be readable: {:?}",
            result.err()
        );
        let validated = result.unwrap();
        assert_eq!(validated.access, PathAccess::ReadOnly);
    }

    /// Verify that files nested in skill subdirectories are also accepted.
    #[test]
    fn nested_path_in_skill_dir_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("fetching-x-content");
        let nested = skill_dir.join("scripts").join("x-fetch.js");
        fs::create_dir_all(nested.parent().unwrap()).unwrap();
        fs::write(&nested, "// script").unwrap();

        let cwd = tmp.path().join("project");
        fs::create_dir_all(&cwd).unwrap();

        let result = validate_path_with_dirs(
            nested.to_str().unwrap(),
            &cwd,
            &[],
            &[],
            &[],
            std::slice::from_ref(&skill_dir),
        );
        assert!(
            result.is_ok(),
            "Nested skill file should be readable: {:?}",
            result.err()
        );
        assert_eq!(result.unwrap().access, PathAccess::ReadOnly);
    }

    /// Verify that paths outside skill directories are still rejected.
    #[test]
    fn path_outside_skill_dir_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("fetching-x-content");
        fs::create_dir_all(&skill_dir).unwrap();
        let outside_file = tmp.path().join("outside.md");
        fs::write(&outside_file, "nope").unwrap();

        let cwd = tmp.path().join("project");
        fs::create_dir_all(&cwd).unwrap();

        let result = validate_path_with_dirs(
            outside_file.to_str().unwrap(),
            &cwd,
            &[],
            &[],
            &[],
            std::slice::from_ref(&skill_dir),
        );
        assert!(result.is_err(), "File outside skill dir should be rejected");
    }

    /// Verify that multiple skill directories are all recognized.
    #[test]
    fn multiple_skill_dirs_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_a = tmp.path().join("skill-a");
        let skill_b = tmp.path().join("skill-b");
        fs::create_dir_all(&skill_a).unwrap();
        fs::create_dir_all(&skill_b).unwrap();
        let file_a = skill_a.join("SKILL.md");
        let file_b = skill_b.join("SKILL.md");
        fs::write(&file_a, "a").unwrap();
        fs::write(&file_b, "b").unwrap();

        let cwd = tmp.path().join("project");
        fs::create_dir_all(&cwd).unwrap();

        let skill_dirs = [skill_a, skill_b];
        let result_a =
            validate_path_with_dirs(file_a.to_str().unwrap(), &cwd, &[], &[], &[], &skill_dirs);
        assert!(result_a.is_ok());

        let result_b =
            validate_path_with_dirs(file_b.to_str().unwrap(), &cwd, &[], &[], &[], &skill_dirs);
        assert!(result_b.is_ok());
    }

    #[test]
    fn definitions_returns_same_slice_on_repeated_calls() {
        let registry = default_tool_registry();
        let first = registry.definitions();
        let second = registry.definitions();

        // Same pointer confirms caching, not cloning on every call
        assert!(
            std::ptr::eq(first, second),
            "definitions() must return the same slice on repeated calls"
        );

        // Verify definitions contain the expected tools
        let names: Vec<&str> = first.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"Bash"), "should contain Bash");
        assert!(names.contains(&"Read"), "should contain Read");
        assert!(names.contains(&"Edit"), "should contain Edit");
        assert!(names.contains(&"Write"), "should contain Write");
    }

    #[test]
    fn empty_registry_returns_empty_slice() {
        let registry = ToolRegistry::empty();
        assert!(registry.definitions().is_empty());
    }

    #[test]
    fn read_tool_registry_definitions_match() {
        let registry = read_tool_registry();
        let defs = registry.definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "Read");
    }

    #[test]
    fn definitions_are_stable_across_clone() {
        let registry = default_tool_registry();
        let cloned = registry.clone();
        let orig_defs = registry.definitions();
        let cloned_defs = cloned.definitions();
        assert_eq!(orig_defs.len(), cloned_defs.len());
        for (a, b) in orig_defs.iter().zip(cloned_defs.iter()) {
            assert_eq!(a.name, b.name);
            assert_eq!(a.description, b.description);
            assert_eq!(a.parameters, b.parameters);
        }
    }

    #[test]
    fn concurrent_tool_contexts_validate_against_their_own_additional_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("project");
        let additional_a = tmp.path().join("reference-a");
        let additional_b = tmp.path().join("reference-b");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&additional_a).unwrap();
        fs::create_dir_all(&additional_b).unwrap();
        let file_a = additional_a.join("notes.txt");
        let file_b = additional_b.join("notes.txt");
        fs::write(&file_a, "a").unwrap();
        fs::write(&file_b, "b").unwrap();

        let context_a = ToolContext::with_temp_dirs(
            cwd.clone(),
            Vec::new(),
            vec![additional_a],
            Vec::new(),
            Vec::new(),
        );
        let context_b = ToolContext::with_temp_dirs(
            cwd,
            Vec::new(),
            vec![additional_b],
            Vec::new(),
            Vec::new(),
        );

        std::thread::scope(|scope| {
            let handle_a = scope.spawn(|| {
                let own = validate_path(&context_a, file_a.to_str().unwrap()).unwrap();
                let other = validate_path(&context_a, file_b.to_str().unwrap());
                (own.access, other.is_err())
            });
            let handle_b = scope.spawn(|| {
                let own = validate_path(&context_b, file_b.to_str().unwrap()).unwrap();
                let other = validate_path(&context_b, file_a.to_str().unwrap());
                (own.access, other.is_err())
            });

            assert_eq!(handle_a.join().unwrap(), (PathAccess::ReadOnly, true));
            assert_eq!(handle_b.join().unwrap(), (PathAccess::ReadOnly, true));
        });
    }

    /// A new file under a symlinked settings directory must be accepted for
    /// writes, matching the existing-file path (`validate_path_with_dirs`
    /// canonicalizes each directory before comparing).
    #[cfg(unix)]
    #[test]
    fn write_scheduling_accepts_new_file_under_symlinked_settings_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let real_dir = tmp.path().join("real-settings");
        let link_dir = tmp.path().join("link-settings");
        fs::create_dir_all(&real_dir).unwrap();
        std::os::unix::fs::symlink(&real_dir, &link_dir).unwrap();

        let cwd = tmp.path().join("project");
        fs::create_dir_all(&cwd).unwrap();

        let context = ToolContext::with_temp_dirs(
            cwd,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![link_dir.clone()],
        );

        let target = link_dir.join("new-file.txt");
        let resolved =
            resolve_path_for_write_scheduling(&context, target.to_str().unwrap()).unwrap();
        // The deepest existing ancestor is canonicalized (the symlink is
        // followed), so the resolved target points into the real directory.
        let expected = fs::canonicalize(&real_dir).unwrap().join("new-file.txt");
        assert_eq!(resolved, expected);
    }

    /// A new file under a read-only additional directory must be rejected,
    /// matching the existing-file path.
    #[test]
    fn write_scheduling_rejects_new_file_in_read_only_additional_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("project");
        let additional = tmp.path().join("reference");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&additional).unwrap();

        let context = ToolContext::with_temp_dirs(
            cwd,
            Vec::new(),
            vec![additional.clone()],
            Vec::new(),
            Vec::new(),
        );

        let target = additional.join("new-file.txt");
        let err =
            resolve_path_for_write_scheduling(&context, target.to_str().unwrap()).unwrap_err();
        assert!(
            err.contains("read-only"),
            "expected a read-only rejection, got: {err}"
        );
    }

    /// A new file outside the cwd, temp, settings, and additional directories
    /// must be rejected.
    #[test]
    fn write_scheduling_rejects_new_file_outside_allowed_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("project");
        let outside = tmp.path().join("elsewhere");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&outside).unwrap();

        let context =
            ToolContext::with_temp_dirs(cwd, Vec::new(), Vec::new(), Vec::new(), Vec::new());

        let target = outside.join("new-file.txt");
        let err =
            resolve_path_for_write_scheduling(&context, target.to_str().unwrap()).unwrap_err();
        assert!(
            err.contains("outside the working directory"),
            "expected an outside-working-directory rejection, got: {err}"
        );
    }

    /// An empty path has no resolvable base and must be rejected.
    #[test]
    fn write_scheduling_rejects_empty_path() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("project");
        fs::create_dir_all(&cwd).unwrap();

        let context =
            ToolContext::with_temp_dirs(cwd, Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let err = resolve_path_for_write_scheduling(&context, "").unwrap_err();
        assert!(
            err.contains("Parent directory not found"),
            "expected a parent-directory error, got: {err}"
        );
    }

    fn fixture_toolbox_tool() -> crate::config::toolbox::ToolboxTool {
        crate::config::toolbox::ToolboxTool {
            registered_name: "tb__run_tests".to_string(),
            original_name: "run_tests".to_string(),
            path: PathBuf::from("/tools/run_tests"),
            description: "Run the test suite. Extra detail.".to_string(),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
            format: crate::config::toolbox::ToolboxFormat::Json,
            timeout_secs: 60,
        }
    }

    #[test]
    fn format_tool_list_section_includes_all_tools() {
        let result = format_tool_list_section(SandboxPolicy::WorkspaceWrite, &[]);
        assert!(result.starts_with("## Available tools"));
        assert!(result.contains("- **Bash**:"));
        assert!(result.contains("- **Read**:"));
        assert!(result.contains("- **Edit**:"));
        assert!(result.contains("- **Write**:"));
        assert!(result.contains("Only these tools are available."));
        assert!(result.contains("no Glob, Grep, or LS tool"));
    }

    #[test]
    fn format_tool_list_section_includes_toolbox_tools() {
        let result =
            format_tool_list_section(SandboxPolicy::WorkspaceWrite, &[fixture_toolbox_tool()]);
        assert!(result.contains("- **tb__run_tests**: Run the test suite.\n"));
    }

    #[test]
    fn format_tool_list_section_read_only_excludes_mutating_tools() {
        let result = format_tool_list_section(SandboxPolicy::ReadOnly, &[fixture_toolbox_tool()]);
        assert!(result.contains("- **Bash**:"));
        assert!(result.contains("- **Read**:"));
        assert!(
            !result.contains("- **Edit**:"),
            "read-only prompt must not advertise the Edit tool"
        );
        assert!(
            !result.contains("- **Write**:"),
            "read-only prompt must not advertise the Write tool"
        );
        assert!(
            !result.contains("tb__run_tests"),
            "read-only prompt must not advertise toolbox tools"
        );
    }

    #[test]
    fn read_only_registry_drops_edit_write_and_toolbox_tools() {
        let mut registry = default_tool_registry();
        registry.push_entry(toolbox_tool_entry(
            fixture_toolbox_tool(),
            uuid::Uuid::nil(),
        ));
        assert_eq!(
            registry.names(),
            vec!["Bash", "Edit", "Read", "Write", "tb__run_tests"]
        );
        registry.retain_read_safe_tools();
        assert_eq!(registry.names(), vec!["Bash", "Read"]);
        assert_eq!(registry.definitions().len(), 2);
    }

    // ── argument compensation classification ──

    #[test]
    fn repair_event_fires_when_repair_modified_arguments() {
        // A raw newline inside a JSON string: repaired and parseable.
        let arguments = "{\"path\":\"f\",\"edits\":[{\"old_text\":\"a\n\",\"new_text\":\"b\"}]}";
        let events = argument_compensation_events("Edit", arguments, false, true);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, CompensationKind::JsonRepair);
        assert_eq!(events[0].detail.as_deref(), Some("Edit"));
    }

    #[test]
    fn repair_event_survives_a_failed_call() {
        // Repair applied but the call later failed: still counted.
        let arguments = "{\"path\":\"f\",\"edits\":[{\"old_text\":\"a\n\",\"new_text\":\"b\"}]}";
        let events = argument_compensation_events("Edit", arguments, true, true);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, CompensationKind::JsonRepair);
    }

    #[test]
    fn unregistered_tools_do_not_carry_argument_compensations() {
        // A hallucinated toolbox name with repairable arguments never reached
        // an argument parser, so no repair event fires.
        let arguments = "{\"a\":\"line1\nline2\"}";
        let events = argument_compensation_events("tb__nonexistent", arguments, true, false);
        assert!(events.is_empty());
    }

    #[test]
    fn invalid_edit_arguments_event_fires_only_on_failure() {
        let arguments = r#"{"path":"f","edits":[{"new_string":"x"}]}"#;
        let events = argument_compensation_events("Edit", arguments, true, true);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, CompensationKind::EditInvalidArguments);

        let events = argument_compensation_events("Edit", arguments, false, true);
        assert!(
            events.is_empty(),
            "successful call must not fire the counter"
        );
    }

    #[test]
    fn no_compensation_events_for_valid_arguments() {
        let arguments = r#"{"path":"f","edits":[{"old_text":"a","new_text":"b"}]}"#;
        let events = argument_compensation_events("Edit", arguments, true, true);
        assert!(
            events.is_empty(),
            "valid Edit args that failed later are not classified"
        );

        let events = argument_compensation_events("Bash", "echo hi", false, true);
        assert!(events.is_empty(), "Bash does not use the repair pass");
    }

    #[test]
    fn repair_event_requires_parseable_result() {
        // A raw tab inside an unterminated string: the repair pass escapes
        // the control character but the payload still is not valid JSON, so
        // the repair did not apply and no event fires.
        let arguments = "{\"a\":\"\t";
        let events = argument_compensation_events("Write", arguments, true, true);
        assert!(events.is_empty());
    }
}
