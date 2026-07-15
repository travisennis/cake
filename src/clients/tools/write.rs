use crate::clients::tools::ToolContext;
use serde::Deserialize;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

// =============================================================================
// Write Tool Definition
// =============================================================================

/// Returns the Write tool definition
pub(super) fn write_tool() -> super::Tool {
    super::Tool {
        type_: "function".to_string(),
        name: "Write".to_string(),
        description: include_str!("write-description.txt").to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the file to create or overwrite"
                },
                "content": {
                    "type": "string",
                    "description": "The full content to write to the file"
                }
            },
            "required": ["path", "content"]
        }),
    }
}

// =============================================================================
// Write Execution
// =============================================================================

/// Arguments for the Write tool
#[derive(Debug, Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

/// Expected JSON shape for the Write tool arguments.
const fn expected_write_arguments_shape() -> &'static str {
    r#"{"path":"file.txt","content":"..."}"#
}

/// Return the validated canonical path this Write call would mutate.
pub(super) fn mutating_target(
    context: &ToolContext,
    arguments: &str,
) -> Result<std::path::PathBuf, String> {
    let repaired = super::repair_json_args(arguments);
    let args: WriteArgs = serde_json::from_str(&repaired).map_err(|e| {
        super::format_json_parse_error(&repaired, &e, "write", expected_write_arguments_shape())
    })?;
    validate_path_for_write(context, &args.path)
}

/// Execute a write command
pub(super) fn execute_write(
    context: &ToolContext,
    arguments: &str,
) -> Result<super::ToolResult, String> {
    let repaired = super::repair_json_args(arguments);
    let args: WriteArgs = serde_json::from_str(&repaired).map_err(|e| {
        super::format_json_parse_error(&repaired, &e, "write", expected_write_arguments_shape())
    })?;

    // Validate path is within working directory
    let path = validate_path_for_write(context, &args.path)?;

    // Check if file exists to determine if it's a create or overwrite.
    // This must use the validated (normalised) path so that writes to
    // files that exist only after normalisation are correctly reported
    // as overwrites.
    let file_existed = path.exists();

    // Create parent directories if they don't exist
    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create directories '{}': {e}", parent.display()))?;
    }

    // Write content to file
    std::fs::write(&path, &args.content)
        .map_err(|e| format!("Failed to write file '{}': {e}", path.display()))?;

    let bytes_written = args.content.len();

    let action = if file_existed {
        "Overwritten"
    } else {
        "Created"
    };

    let warning = if file_existed {
        " (Note: Consider using Edit tool for targeted changes to existing files)"
    } else {
        ""
    };

    let result = format!(
        "{action}: {}{}\nBytes written: {}",
        path.display(),
        warning,
        bytes_written
    );

    Ok(super::ToolResult { output: result })
}

/// Validate a path for writing - allows non-existent paths as long as parent is in cwd or temp directories.
/// Rejects paths in read-only additional directories (--add-dir).
fn validate_path_for_write(
    context: &ToolContext,
    path_str: &str,
) -> Result<std::path::PathBuf, String> {
    let path = Path::new(path_str);

    // If the file exists, use the shared validation which checks read-only status
    if path.exists() {
        return super::validate_path_for_write(context, path_str);
    }

    // Resolve the path root-to-leaf with symlink-aware semantics for existing
    // components. Non-existing suffixes are normalised lexically so that
    // parent-directory components across nonexistent ancestors are handled
    // correctly (<cwd>/missing/../target → <cwd>/target).
    let (resolved_base, pending) = resolve_write_path(path);

    // Validate the resolved base is within allowed directories
    let canonical_base = resolved_base.canonicalize().map_err(|e| {
        format!(
            "Parent directory not found '{}': {e}",
            resolved_base.display()
        )
    })?;

    let is_in_cwd = canonical_base.starts_with(&context.cwd);
    let is_in_temp = super::get_temp_directories(context)
        .iter()
        .any(|temp_dir| canonical_base.starts_with(temp_dir));
    let is_in_settings = super::get_settings_dirs(context)
        .iter()
        .any(|settings_dir| canonical_base.starts_with(settings_dir));
    let is_in_read_only = super::get_additional_dirs(context)
        .iter()
        .any(|add_dir| canonical_base.starts_with(add_dir));

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
fn resolve_write_path(path: &Path) -> (PathBuf, Vec<OsString>) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Lexically normalise a path by resolving `.` and `..` components without
    /// touching the filesystem.
    fn normalize_path(path: &Path) -> PathBuf {
        let mut components: Vec<OsString> = Vec::new();
        let mut root: Option<OsString> = None;

        for component in path.components() {
            match component {
                std::path::Component::RootDir => {
                    components.clear();
                    root = Some(OsStr::new("/").to_os_string());
                },
                std::path::Component::Prefix(p) => {
                    components.clear();
                    root = Some(p.as_os_str().to_os_string());
                },
                std::path::Component::CurDir => {
                    // skip `.`
                },
                std::path::Component::ParentDir => {
                    // Pop the last component if it is a Normal component
                    // (not a preserved `..` from an earlier ParentDir).
                    match components.last() {
                        Some(c) if c.as_os_str() != OsStr::new("..") => {
                            components.pop();
                        },
                        _ if root.is_none() => {
                            // Relative path above cwd — preserve `..`
                            components.push(OsStr::new("..").to_os_string());
                        },
                        _ => {},
                    }
                },
                std::path::Component::Normal(name) => {
                    components.push(name.to_os_string());
                },
            }
        }

        let mut result = PathBuf::new();
        if let Some(r) = root {
            result.push(r);
        }
        for c in &components {
            result.push(c);
        }
        result
    }

    #[test]
    fn create_new_file() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("new_file.txt");

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "content": "Hello, world!"
        })
        .to_string();

        let result = execute_write(&ToolContext::from_current_process(), &args).unwrap();
        assert!(result.output.contains("Created:"));
        assert!(result.output.contains("Bytes written: 13"));

        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "Hello, world!");
    }

    #[test]
    fn overwrite_existing_file() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("existing.txt");
        fs::write(&file_path, "old content").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "content": "new content"
        })
        .to_string();

        let result = execute_write(&ToolContext::from_current_process(), &args).unwrap();
        assert!(result.output.contains("Overwritten:"));
        assert!(result.output.contains("Note: Consider using Edit tool"));

        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "new content");
    }

    #[test]
    fn auto_create_parent_directories() {
        let temp_dir = TempDir::new().unwrap();
        let nested_path = temp_dir.path().join("a/b/c/deep_file.txt");

        let args = serde_json::json!({
            "path": nested_path.to_str().unwrap(),
            "content": "Deep content"
        })
        .to_string();

        let result = execute_write(&ToolContext::from_current_process(), &args).unwrap();
        assert!(result.output.contains("Created:"));

        let content = fs::read_to_string(&nested_path).unwrap();
        assert_eq!(content, "Deep content");
    }

    #[test]
    fn parent_components_across_nonexistent_ancestor() {
        let temp_dir = TempDir::new().unwrap();
        let base = temp_dir.path();

        // Path with `..` traversing a directory that does not exist.
        // e.g. <base>/missing/../target/file.txt  ->  <base>/target/file.txt
        let raw_path = base.join("missing/../target/file.txt");
        let normalized_path = base.join("target/file.txt");

        let args = serde_json::json!({
            "path": raw_path.to_str().unwrap(),
            "content": "parent-component test",
        })
        .to_string();

        let result = execute_write(&ToolContext::from_current_process(), &args).unwrap();
        assert!(result.output.contains("Created:"));

        // The file must exist at the *normalized* location.
        assert!(
            normalized_path.exists(),
            "file should be at the normalized path: {}",
            normalized_path.display()
        );
        let content = fs::read_to_string(&normalized_path).unwrap();
        assert_eq!(content, "parent-component test");

        // The `missing` directory should never have been created.
        assert!(
            !base.join("missing").exists(),
            "intermediate `missing` directory must not be created"
        );

        // mutating_target must agree with the resolved destination.
        // Canonicalise the expected path because the validator uses
        // `canonicalize` on the deepest existing parent (macOS may expand
        // /var → /private/var).
        let target = mutating_target(&ToolContext::from_current_process(), &args).unwrap();
        let expected =
            std::fs::canonicalize(&normalized_path).unwrap_or_else(|_| normalized_path.clone());
        assert_eq!(
            target, expected,
            "mutating_target must return the normalized path"
        );
    }

    #[test]
    fn parent_components_above_root_are_rejected() {
        // An absolute path that lexically normalises above root should be
        // caught as outside the working directory.
        let args = serde_json::json!({
            "path": "/../etc/passwd",
            "content": "escape",
        })
        .to_string();

        let result = execute_write(&ToolContext::from_current_process(), &args);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("outside the working directory"),
            "expected 'outside the working directory' error, got: {err}"
        );
    }

    #[test]
    fn error_on_path_outside_working_directory() {
        let args = serde_json::json!({
            "path": "/etc/passwd",
            "content": "test"
        })
        .to_string();

        let result = execute_write(&ToolContext::from_current_process(), &args);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("outside the working directory")
        );
    }

    #[test]
    fn empty_content_creates_file() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("empty.txt");

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "content": ""
        })
        .to_string();

        let result = execute_write(&ToolContext::from_current_process(), &args).unwrap();
        assert!(result.output.contains("Created:"));
        assert!(result.output.contains("Bytes written: 0"));

        assert!(file_path.exists());
        let content = fs::read_to_string(&file_path).unwrap();
        assert!(content.is_empty());
    }

    // ── JSON parse error formatting ──

    #[test]
    fn write_invalid_json_missing_path() {
        let payload = r#"{"content":"hello"}"#;
        let err = execute_write(&ToolContext::from_current_process(), payload).unwrap_err();

        assert!(
            err.contains("Invalid write arguments"),
            "Error should identify invalid write arguments: {err}"
        );
        assert!(
            err.contains("missing field `path`"),
            "Error should identify missing path: {err}"
        );
        assert!(
            err.contains("Context:"),
            "Error should include context window: {err}"
        );
        assert!(err.contains("Hint:"), "Error should include a hint: {err}");
        assert!(
            err.contains("`path` field is required"),
            "Hint should mention path field: {err}"
        );
        assert!(
            err.contains("Expected shape"),
            "Error should include expected shape: {err}"
        );
    }

    #[test]
    fn write_invalid_json_trailing_chars() {
        // Payload that fails even after repair: trailing data.
        // Use serde_json::from_str::<WriteArgs> on a payload with extra data.
        let payload = r#"{"path":"x","content":"hello"}extra"#;
        // Repair strips trailing data, so this would actually succeed.
        // Test via `mutating_target` or directly with format_json_parse_error.
        let err = serde_json::from_str::<WriteArgs>(payload).unwrap_err();
        let msg = crate::clients::tools::format_json_parse_error(
            payload,
            &err,
            "write",
            r#"{"path":"file.txt","content":"..."}"#,
        );

        assert!(
            msg.contains("trailing characters") || msg.contains("trailing data"),
            "Error should mention trailing content: {msg}"
        );
        assert!(msg.contains("Hint:"), "Error should include a hint: {msg}");
        assert!(
            msg.contains("Context:"),
            "Error should include context: {msg}"
        );
    }

    #[test]
    fn write_invalid_json_control_char_in_string() {
        let corrupted = b"{\"path\":\"x\",\"content\":\"hello\x01world\"}";
        let payload = std::str::from_utf8(corrupted).unwrap();
        let err = serde_json::from_str::<WriteArgs>(payload).unwrap_err();
        let msg = crate::clients::tools::format_json_parse_error(
            payload,
            &err,
            "write",
            r#"{"path":"file.txt","content":"..."}"#,
        );

        assert!(
            msg.contains("control character"),
            "Error should mention control character: {msg}"
        );
        assert!(
            msg.contains("raw control character"),
            "Hint should mention raw control chars: {msg}"
        );
    }

    #[test]
    fn normalize_path_removes_dot_components() {
        assert_eq!(normalize_path(Path::new("/a/b/./c")), Path::new("/a/b/c"));
    }

    #[test]
    fn normalize_path_resolves_parent_components() {
        assert_eq!(normalize_path(Path::new("/a/b/../c")), Path::new("/a/c"));
    }

    #[test]
    fn normalize_path_multiple_parent() {
        assert_eq!(
            normalize_path(Path::new("/a/b/c/../../d")),
            Path::new("/a/d")
        );
    }

    #[test]
    fn normalize_path_parent_above_root_is_noop() {
        assert_eq!(normalize_path(Path::new("/../c")), Path::new("/c"));
    }

    #[test]
    fn normalize_path_relative_with_dotdot() {
        assert_eq!(normalize_path(Path::new("a/../../c")), Path::new("../c"));
    }

    #[test]
    fn normalize_path_relative_preserves_leading_dotdot() {
        assert_eq!(normalize_path(Path::new("../../c")), Path::new("../../c"));
    }

    #[test]
    fn normalize_path_empty_is_empty() {
        assert_eq!(normalize_path(Path::new("")), Path::new(""));
    }

    #[test]
    fn normalize_path_root_only() {
        assert_eq!(normalize_path(Path::new("/")), Path::new("/"));
    }
}
