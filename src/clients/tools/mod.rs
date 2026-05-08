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
//! # Security
//!
//! All tools validate paths against the current working directory and
//! directories added via `--add-dir` flag. Write operations are only allowed
//! in the working directory and temp directories.

use serde::Serialize;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

mod sandbox;

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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolContext {
    pub cwd: PathBuf,
    pub temp_dirs: Vec<PathBuf>,
    pub additional_dirs: Vec<PathBuf>,
    pub skill_dirs: Vec<PathBuf>,
    pub settings_dirs: Vec<PathBuf>,
}

impl ToolContext {
    /// Build a tool context using the same temp directory discovery as the
    /// existing process-global cache.
    pub fn new(
        cwd: PathBuf,
        additional_dirs: Vec<PathBuf>,
        skill_dirs: Vec<PathBuf>,
        settings_dirs: Vec<PathBuf>,
    ) -> Self {
        Self::with_temp_dirs(
            cwd,
            compute_temp_directories(),
            additional_dirs,
            skill_dirs,
            settings_dirs,
        )
    }

    /// Build a tool context with explicitly supplied temp directories.
    ///
    /// This keeps construction testable without depending on process-global
    /// cache state.
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
mod bash_safety;
mod edit;
pub mod read;
mod write;

// =============================================================================
// Tool Types
// =============================================================================

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
/// for Bash or file contents for Read operations.
#[derive(Debug)]
pub struct ToolResult {
    pub output: String,
}

type ToolFuture = Pin<Box<dyn Future<Output = Result<ToolResult, String>> + Send>>;
type ToolExecutor = fn(Arc<ToolContext>, String) -> ToolFuture;
type ToolSummarizer = fn(&str) -> String;

/// Registered behavior for a callable tool.
///
/// This keeps the model-facing definition, execution entry point, and display
/// summary together so adding a tool only requires one registry entry.
#[derive(Clone)]
pub(super) struct ToolEntry {
    definition: Tool,
    execute: ToolExecutor,
    summarize: ToolSummarizer,
}

impl ToolEntry {
    fn new(definition: Tool, execute: ToolExecutor, summarize: ToolSummarizer) -> Self {
        Self {
            definition,
            execute,
            summarize,
        }
    }
}

/// Registry of tools available to an agent.
#[derive(Clone)]
pub(super) struct ToolRegistry {
    entries: Vec<ToolEntry>,
}

impl ToolRegistry {
    /// Build a registry from explicit entries.
    #[cfg(test)]
    pub(super) const fn new(entries: Vec<ToolEntry>) -> Self {
        Self { entries }
    }

    /// Return an empty registry, useful for tests that do not expose tools.
    #[cfg(test)]
    pub(super) const fn empty() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Return the model-facing tool definitions.
    pub(super) fn definitions(&self) -> Vec<Tool> {
        self.entries
            .iter()
            .map(|entry| entry.definition.clone())
            .collect()
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
        arguments: &str,
    ) -> Result<ToolResult, String> {
        let Some(entry) = self.find(name) else {
            return Err(format!("Unknown tool: {name}"));
        };

        (entry.execute)(context, arguments.to_string()).await
    }

    /// Summarize registered tool arguments for display.
    pub(super) fn summarize(&self, name: &str, arguments: &str) -> String {
        self.find(name)
            .map_or_else(String::new, |entry| (entry.summarize)(arguments))
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

/// Get standard temporary directory paths (cached)
pub(super) fn get_temp_directories(context: &ToolContext) -> &[PathBuf] {
    &context.temp_dirs
}

// =============================================================================
// Tool Execution
// =============================================================================

fn execute_bash_tool(context: Arc<ToolContext>, arguments: String) -> ToolFuture {
    Box::pin(async move { bash::execute_bash(&context, &arguments).await })
}

fn execute_edit_tool(context: Arc<ToolContext>, arguments: String) -> ToolFuture {
    Box::pin(async move {
        tokio::task::spawn_blocking(move || edit::execute_edit(&context, &arguments))
            .await
            .map_err(|e| format!("Task join error: {e}"))?
    })
}

fn execute_read_tool(context: Arc<ToolContext>, arguments: String) -> ToolFuture {
    Box::pin(async move {
        tokio::task::spawn_blocking(move || read::execute_read(&context, &arguments))
            .await
            .map_err(|e| format!("Task join error: {e}"))?
    })
}

fn execute_write_tool(context: Arc<ToolContext>, arguments: String) -> ToolFuture {
    Box::pin(async move {
        tokio::task::spawn_blocking(move || write::execute_write(&context, &arguments))
            .await
            .map_err(|e| format!("Task join error: {e}"))?
    })
}

// =============================================================================
// Tool Argument Summarization
// =============================================================================

/// Summarize tool arguments for display.
/// This function uses the same typed argument structs as the tool execution,
/// ensuring that parameter names stay in sync.
pub fn summarize_tool_args(tool_name: &str, arguments: &str) -> String {
    let raw = default_tool_registry().summarize(tool_name, arguments);

    truncate_display(&raw, 120)
}

/// Truncate a string for display, appending "..." if needed.
fn truncate_display(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

// =============================================================================
// Tool Registry
// =============================================================================

/// Returns the default tool registry.
pub(super) fn default_tool_registry() -> ToolRegistry {
    ToolRegistry {
        entries: vec![
            ToolEntry::new(bash::bash_tool(), execute_bash_tool, bash::summarize_args),
            ToolEntry::new(edit::edit_tool(), execute_edit_tool, edit::summarize_args),
            ToolEntry::new(read::read_tool(), execute_read_tool, read::summarize_args),
            ToolEntry::new(
                write::write_tool(),
                execute_write_tool,
                write::summarize_args,
            ),
        ],
    }
}

/// Returns a registry containing only the Read tool.
#[cfg(test)]
pub(super) fn read_tool_registry() -> ToolRegistry {
    ToolRegistry::new(vec![ToolEntry::new(
        read::read_tool(),
        execute_read_tool,
        read::summarize_args,
    )])
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
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

        assert_eq!(first, second);
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
}
