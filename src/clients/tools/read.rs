use serde::Deserialize;
use std::io::{BufRead, BufReader, Read as _};
use std::path::Path;

use crate::clients::tools::{ToolContext, validate_path_in_cwd};

const DEFAULT_END_LINE: usize = 200;
const MAX_OUTPUT_BYTES: usize = 100_000;

// =============================================================================
// Read Tool Definition
// =============================================================================

/// Returns the Read tool definition
pub(super) fn read_tool() -> super::Tool {
    super::Tool {
        type_: "function".to_string(),
        name: "Read".to_string(),
        description: include_str!("read-description.txt").to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the file to read"
                },
                "start_line": {
                    "type": "integer",
                    "description": "First line to read (1-indexed, inclusive). Default: 1"
                },
                "end_line": {
                    "type": "integer",
                    "description": "Last line to read (1-indexed, inclusive). Default: 200 (or start_line+199 when start_line given)"
                }
            },
            "required": ["path"]
        }),
    }
}

// =============================================================================
// Read Execution
// =============================================================================

/// Arguments for the Read tool
#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
}

/// Extract the path from Read tool arguments without full validation.
///
/// Returns `None` if the arguments cannot be parsed.
pub fn extract_path(arguments: &str) -> Option<String> {
    serde_json::from_str::<ReadArgs>(arguments)
        .map(|args| args.path)
        .ok()
}

/// Execute a read command
pub(super) fn execute_read(
    context: &ToolContext,
    arguments: &str,
) -> Result<super::ToolResult, String> {
    let args: ReadArgs =
        serde_json::from_str(arguments).map_err(|e| format!("Invalid read arguments: {e}"))?;

    // Validate and canonicalize the path
    let path = validate_path_in_cwd(context, &args.path)?;

    // Check if path exists
    if !path.exists() {
        return Err(format!("Path not found: {}", path.display()));
    }

    // Reject directories
    if path.is_dir() {
        return Err(format!(
            "Path is a directory, not a file: {}",
            path.display()
        ));
    }

    // Handle file
    read_file(&path, args.start_line, args.end_line)
}

/// Check the first 8KB of a file for null bytes (binary detection)
fn is_binary(path: &Path) -> Result<bool, String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("Failed to open file '{}': {e}", path.display()))?;
    let mut buf = [0u8; 8192];
    let n = (&file)
        .take(8192)
        .read(&mut buf)
        .map_err(|e| format!("Failed to read file '{}': {e}", path.display()))?;
    Ok(buf[..n].contains(&0))
}

/// Read and format a file with line numbers
fn read_file(
    path: &Path,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<super::ToolResult, String> {
    // Check for binary files (null bytes in first 8KB)
    if is_binary(path)? {
        return Err(format!(
            "Cannot read binary file: {} (detected null bytes)",
            path.display()
        ));
    }

    // Default line range (1-indexed from caller, convert to 0-indexed)
    // When start_line is provided without end_line, expand the window from start_line
    // instead of keeping the absolute default of 500.
    let start = start_line.unwrap_or(1).saturating_sub(1);
    let end_requested = match end_line {
        Some(end) => end.saturating_sub(1),
        None if start_line.is_some() => {
            // Window of DEFAULT_END_LINE lines starting from start_line
            start.saturating_add(DEFAULT_END_LINE - 1)
        },
        None => DEFAULT_END_LINE.saturating_sub(1),
    };

    // Read only the lines we need using a buffered reader
    let file = std::fs::File::open(path)
        .map_err(|e| format!("Failed to read file '{}': {e}", path.display()))?;
    let reader = BufReader::new(file);

    let mut numbered_lines: Vec<String> = Vec::new();
    let mut total_lines: usize = 0;

    for (i, line_result) in reader.lines().enumerate() {
        let line =
            line_result.map_err(|e| format!("Failed to read file '{}': {e}", path.display()))?;
        total_lines = i + 1;

        if i < start {
            continue;
        }
        if i > end_requested {
            // Keep counting for total_lines but don't store content
            continue;
        }
        numbered_lines.push(format!("{:>6}: {line}", i + 1));
    }

    // Clamp end to actual file length
    let end = end_requested.min(total_lines.saturating_sub(1));

    if start > end || start >= total_lines {
        return Ok(super::ToolResult {
            output: format!(
                "File: {}\n{total_lines} lines total\n(start_line > end_line, no content to show)",
                path.display()
            ),
        });
    }

    let mut output = format!(
        "File: {}\nLines {}-{}/{}\n{}",
        path.display(),
        start + 1,
        end + 1,
        total_lines,
        numbered_lines.join("\n")
    );

    // Truncate if too large
    if output.len() > MAX_OUTPUT_BYTES {
        use std::fmt::Write;
        let truncate_at = MAX_OUTPUT_BYTES - 100; // Leave room for truncation message
        let mut truncated = output.chars().take(truncate_at).collect::<String>();
        _ = write!(
            truncated,
            "\n[... output truncated at {MAX_OUTPUT_BYTES} bytes ...]"
        );
        output = truncated;
    }

    // Note remaining lines if applicable
    if end < total_lines.saturating_sub(1) {
        use std::fmt::Write;
        let remaining = total_lines - end - 1;
        _ = write!(output, "\n[... {remaining} more lines ...]");
    }

    Ok(super::ToolResult { output })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn read_small_file_full_content() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Line 1\nLine 2\nLine 3").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap()
        })
        .to_string();

        let result = execute_read(&ToolContext::from_current_process(), &args).unwrap();
        assert!(result.output.contains("File:"));
        assert!(result.output.contains("     1: Line 1"));
        assert!(result.output.contains("     2: Line 2"));
        assert!(result.output.contains("     3: Line 3"));
    }

    #[test]
    fn read_with_line_range() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Line 1\nLine 2\nLine 3\nLine 4\nLine 5").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "start_line": 2,
            "end_line": 4
        })
        .to_string();

        let result = execute_read(&ToolContext::from_current_process(), &args).unwrap();
        assert!(result.output.contains("Lines 2-4/5"));
        assert!(result.output.contains("     2: Line 2"));
        assert!(result.output.contains("     3: Line 3"));
        assert!(result.output.contains("     4: Line 4"));
        assert!(!result.output.contains("Line 1"));
        assert!(!result.output.contains("Line 5"));
    }

    #[test]
    fn error_on_nonexistent_path() {
        let args = serde_json::json!({
            "path": "/nonexistent/path/xyz123"
        })
        .to_string();

        let result = execute_read(&ToolContext::from_current_process(), &args);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Path not found"));
    }

    #[test]
    fn default_read_still_1_to_200() {
        // Neither start_line nor end_line should still default to 1-200.
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let lines: Vec<String> = (1..=600).map(|i| format!("Line {i}")).collect();
        fs::write(&file_path, lines.join("\n")).unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap()
        })
        .to_string();

        let result = execute_read(&ToolContext::from_current_process(), &args).unwrap();
        assert!(result.output.contains("Lines 1-200/600"));
        assert!(result.output.contains("Line 200"));
        assert!(result.output.contains("[... 400 more lines ...]"));
    }

    #[test]
    fn truncation_note_when_exceeds_range() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Line 1\nLine 2\nLine 3").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "start_line": 1,
            "end_line": 2
        })
        .to_string();

        let result = execute_read(&ToolContext::from_current_process(), &args).unwrap();
        assert!(result.output.contains("Lines 1-2/3"));
        assert!(result.output.contains("[... 1 more lines ...]"));
    }

    #[test]
    fn error_on_directory_path() {
        let temp_dir = TempDir::new().unwrap();

        let args = serde_json::json!({
            "path": temp_dir.path().to_str().unwrap()
        })
        .to_string();

        let result = execute_read(&ToolContext::from_current_process(), &args);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("is a directory, not a file"));
    }

    #[test]
    fn start_line_without_end_line_returns_window() {
        // When start_line is provided without end_line, the window should be
        // start_line..start_line+199, not the absolute 1-200 default.
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let lines: Vec<String> = (1..=720).map(|i| format!("Line {i}")).collect();
        fs::write(&file_path, lines.join("\n")).unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "start_line": 200
        })
        .to_string();

        let result = execute_read(&ToolContext::from_current_process(), &args).unwrap();
        // Should read lines 200-399 (file ends before start_line+199)
        assert!(result.output.contains("Lines 200-399/720"));
        assert!(result.output.contains("   200: Line 200"));
        assert!(result.output.contains("   399: Line 399"));
        assert!(!result.output.contains("Line 199"));
    }

    #[test]
    fn start_line_without_end_line_window_in_middle() {
        // When the file is long enough, start_line+199 should be the end.
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let lines: Vec<String> = (1..=1000).map(|i| format!("Line {i}")).collect();
        fs::write(&file_path, lines.join("\n")).unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "start_line": 300
        })
        .to_string();

        let result = execute_read(&ToolContext::from_current_process(), &args).unwrap();
        assert!(result.output.contains("Lines 300-499/1000"));
        assert!(result.output.contains("   300: Line 300"));
        assert!(result.output.contains("   499: Line 499"));
        assert!(!result.output.contains("Line 299"));
        assert!(!result.output.contains("Line 800"));
        assert!(result.output.contains("[... 501 more lines ...]"));
    }

    #[test]
    fn start_line_at_end_of_file() {
        // start_line beyond the file should show the "no content" message.
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let lines: Vec<String> = (1..=10).map(|i| format!("Line {i}")).collect();
        fs::write(&file_path, lines.join("\n")).unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "start_line": 20
        })
        .to_string();

        let result = execute_read(&ToolContext::from_current_process(), &args).unwrap();
        assert!(result.output.contains("no content to show"));
    }

    #[test]
    fn start_line_one_without_end_line_matches_default() {
        // Explicit start_line=1 without end_line should behave same as default.
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let lines: Vec<String> = (1..=600).map(|i| format!("Line {i}")).collect();
        fs::write(&file_path, lines.join("\n")).unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "start_line": 1
        })
        .to_string();

        let result = execute_read(&ToolContext::from_current_process(), &args).unwrap();
        assert!(result.output.contains("Lines 1-200/600"));
        assert!(result.output.contains("[... 400 more lines ...]"));
    }
}
