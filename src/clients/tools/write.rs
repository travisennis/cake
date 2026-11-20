use crate::clients::tools::ToolContext;
use crate::clients::tools::file_replace::replace_file;
use serde::Deserialize;

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
    super::resolve_path_for_write_scheduling(context, &args.path)
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
    let path = super::resolve_path_for_write_scheduling(context, &args.path)?;

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

    // Publish the complete replacement through a staged sibling so a failed
    // preparation leaves an existing file untouched.
    replace_file(&path, args.content.as_bytes())
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

    Ok(super::ToolResult {
        output: result,
        images: Vec::new(),
        compensation_events: Vec::new(),
        permission_denials: Vec::new(),
    })
}
