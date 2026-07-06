use crate::clients::tools::ToolContext;
use serde::Deserialize;
use std::path::Path;

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

    // Check if file exists to determine if it's a create or overwrite
    let file_existed = Path::new(&args.path).exists();

    // Validate path is within working directory
    // For new files, we need to handle the case where the file doesn't exist yet
    let path = validate_path_for_write(context, &args.path)?;

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

    // For new files, find the deepest existing parent directory
    let parent = path
        .parent()
        .ok_or_else(|| format!("Invalid file path (no parent): {path_str}"))?;

    // Walk up the tree to find an existing directory we can canonicalize
    let mut current_parent = parent;
    let mut components_to_create = Vec::new();

    while !current_parent.exists() {
        if let Some(file_name) = current_parent.file_name() {
            components_to_create.push(file_name.to_os_string());
        }
        current_parent = current_parent
            .parent()
            .ok_or_else(|| format!("Cannot find existing parent directory for path: {path_str}"))?;
    }

    // Canonicalize the deepest existing parent
    let canonical_parent = current_parent.canonicalize().map_err(|e| {
        format!(
            "Parent directory not found '{}': {e}",
            current_parent.display()
        )
    })?;

    // Check if the existing parent is within allowed directories
    let is_in_cwd = canonical_parent.starts_with(&context.cwd);
    let is_in_temp = super::get_temp_directories(context)
        .iter()
        .any(|temp_dir| canonical_parent.starts_with(temp_dir));
    let is_in_settings = super::get_settings_dirs(context)
        .iter()
        .any(|settings_dir| canonical_parent.starts_with(settings_dir));

    // Check if parent is in a read-only additional directory
    let is_in_read_only = super::get_additional_dirs(context)
        .iter()
        .any(|add_dir| canonical_parent.starts_with(add_dir));

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

    // Reconstruct the full path with the canonicalized parent
    let mut final_path = canonical_parent;
    for component in components_to_create.iter().rev() {
        final_path = final_path.join(component);
    }

    // Add the filename
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("Invalid file path (no file name): {path_str}"))?;
    Ok(final_path.join(file_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

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
}
