use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde::Deserialize;

use crate::clients::tool_output::{READ_TOOL_OUTPUT_NAME, ToolOutputEntry};
use crate::clients::tools::{Tool, ToolContext, ToolError, ToolResult, repair_json_args};
use crate::config::blob_store::TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES;

/// Maximum retrieval body bytes returned to the model.
const MAX_RETRIEVAL_BYTES: usize = TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES;

/// Maximum retrieval handle characters accepted from the model.
const MAX_HANDLE_CHARS: usize = 128;

/// Maximum literal search characters accepted from the model.
const MAX_SEARCH_CHARS: usize = 512;

/// Maximum context lines requested around a literal search match.
const MAX_CONTEXT_LINES: usize = 5;

/// Maximum explicitly requested line range.
const MAX_REQUESTED_LINES: usize = 10_000;

/// Maximum number of search matches rendered in one result.
const MAX_SEARCH_MATCHES: usize = 20;

/// Returns the `ReadToolOutput` definition.
pub(super) fn read_tool_output_tool() -> Tool {
    Tool {
        type_: "function".to_string(),
        name: READ_TOOL_OUTPUT_NAME.to_string(),
        description: include_str!("read-tool-output-description.txt").to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "handle": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_HANDLE_CHARS,
                    "description": "The retrieval handle shown in an externalized tool-result summary."
                },
                "start_line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "First line to return (1-indexed, inclusive). Defaults to 1."
                },
                "end_line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Last line to return (1-indexed, inclusive). Defaults to the end of the result, bounded by the retrieval cap."
                },
                "search": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_SEARCH_CHARS,
                    "description": "Literal, case-sensitive text to find within the result. Matching lines include limited context."
                },
                "context_lines": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": MAX_CONTEXT_LINES,
                    "description": "Lines of context around each search match. Defaults to 2."
                }
            },
            "required": ["handle"]
        }),
    }
}

#[derive(Deserialize)]
struct ReadArguments {
    handle: String,
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    context_lines: Option<usize>,
}

/// Execute one session-scoped output retrieval.
pub(super) fn execute_read_tool_output(
    context: &ToolContext,
    arguments: &str,
) -> Result<ToolResult, ToolError> {
    let args: ReadArguments = serde_json::from_str(arguments)
        .or_else(|_| serde_json::from_str(&repair_json_args(arguments)))
        .map_err(|error| {
            ToolError::new(format!(
                "Invalid {READ_TOOL_OUTPUT_NAME} arguments: {error}"
            ))
        })?;
    let handle = validate_handle(&args.handle)?;
    let entry = context
        .tool_outputs()
        .get(handle)
        .ok_or_else(|| unknown_handle_error(handle))?;
    let output = match args.search.as_deref() {
        Some(query) => search_output(&entry, &args, query)?,
        None => range_output(&entry, &args)?,
    };
    Ok(ToolResult {
        output,
        images: Vec::new(),
        compensation_events: Vec::new(),
        permission_denials: Vec::new(),
    })
}

fn validate_handle(handle: &str) -> Result<&str, ToolError> {
    let handle = handle.trim();
    if handle.is_empty() {
        return Err(ToolError::new(format!(
            "Invalid {READ_TOOL_OUTPUT_NAME} arguments: handle must not be empty"
        )));
    }
    if handle.chars().count() > MAX_HANDLE_CHARS {
        return Err(ToolError::new(format!(
            "Invalid {READ_TOOL_OUTPUT_NAME} arguments: handle exceeds {MAX_HANDLE_CHARS} characters"
        )));
    }
    if looks_path_shaped(handle) {
        return Err(ToolError::new(format!(
            "Invalid {READ_TOOL_OUTPUT_NAME} arguments: handle must be a retrieval handle, not a path"
        )));
    }
    if handle.chars().any(char::is_control) {
        return Err(ToolError::new(format!(
            "Invalid {READ_TOOL_OUTPUT_NAME} arguments: handle contains control characters"
        )));
    }
    Ok(handle)
}

fn looks_path_shaped(handle: &str) -> bool {
    handle.contains('/')
        || handle.contains('\\')
        || handle.contains("..")
        || handle.starts_with('~')
        || handle == "."
        || handle == ".."
}

fn unknown_handle_error(handle: &str) -> ToolError {
    ToolError::new(format!(
        "Unknown {READ_TOOL_OUTPUT_NAME} handle '{}'. Use a handle from the current session's \
         externalized tool-result summary.",
        bounded_label(handle)
    ))
}

fn bounded_label(value: &str) -> String {
    let mut label: String = value.chars().take(80).collect();
    if value.chars().count() > 80 {
        label.push_str("...");
    }
    label
}

fn range_output(entry: &ToolOutputEntry, args: &ReadArguments) -> Result<String, ToolError> {
    let (start, end) = range_bounds(entry, args)?;
    let mut output = header(entry, &format!("Lines {start}-{end}:"));
    let starts = line_starts(&entry.output);
    let mut rendered_any = false;
    let mut next_line = None;
    let mut partial_line = None;
    for line_number in start..=end {
        let line = line_slice(&entry.output, &starts, line_number);
        if output.len().saturating_add(line.len()) <= MAX_RETRIEVAL_BYTES {
            output.push_str(line);
            rendered_any = true;
            continue;
        }
        if !rendered_any {
            let prefix = utf8_head(line, MAX_RETRIEVAL_BYTES.saturating_sub(output.len()));
            output.push_str(prefix);
            partial_line = Some(line_number);
        }
        next_line = Some(line_number);
        break;
    }
    if let Some(line_number) = next_line {
        if partial_line == Some(line_number) {
            _ = writeln!(
                output,
                "\n[line {line_number} is longer than the {MAX_RETRIEVAL_BYTES}-byte retrieval cap \
                 and is truncated]"
            );
        } else {
            _ = writeln!(
                output,
                "\n[retrieval truncated at {MAX_RETRIEVAL_BYTES} bytes; continue with \
                 start_line={line_number}]"
            );
        }
    }
    Ok(output)
}

fn search_output(
    entry: &ToolOutputEntry,
    args: &ReadArguments,
    query: &str,
) -> Result<String, ToolError> {
    validate_search(query)?;
    let context = search_context(args)?;
    let (start, end) = range_bounds(entry, args)?;
    let starts = line_starts(&entry.output);
    let matches = search_matches(entry, &starts, start, end, query);
    let mut output = header(
        entry,
        &format!(
            "Literal search \"{query}\" in lines {start}-{end}: {} matches",
            matches.len()
        ),
    );
    if matches.is_empty() {
        output.push_str("No matches.\n");
        return Ok(output);
    }
    let shown = matches.len().min(MAX_SEARCH_MATCHES);
    _ = writeln!(
        output,
        "Showing matches{} with {context} context line(s):",
        search_scope_label(shown, matches.len())
    );
    render_search_matches(
        entry,
        &starts,
        &matches,
        shown,
        context,
        (start, end),
        &mut output,
    );
    Ok(output)
}

fn search_context(args: &ReadArguments) -> Result<usize, ToolError> {
    let context = args.context_lines.unwrap_or(2);
    if context > MAX_CONTEXT_LINES {
        return Err(ToolError::new(format!(
            "Invalid {READ_TOOL_OUTPUT_NAME} arguments: context_lines must be at most \
             {MAX_CONTEXT_LINES}"
        )));
    }
    Ok(context)
}

fn search_matches(
    entry: &ToolOutputEntry,
    starts: &[usize],
    start: usize,
    end: usize,
    query: &str,
) -> Vec<usize> {
    let mut matches = Vec::new();
    for line_number in start..=end {
        if line_text(line_slice(&entry.output, starts, line_number)).contains(query) {
            matches.push(line_number);
        }
    }
    matches
}

fn search_scope_label(shown: usize, total: usize) -> String {
    if shown < total {
        format!(" 1-{shown} of {total}")
    } else {
        String::new()
    }
}

fn render_search_matches(
    entry: &ToolOutputEntry,
    starts: &[usize],
    matches: &[usize],
    shown: usize,
    context: usize,
    bounds: (usize, usize),
    output: &mut String,
) {
    let mut selected = BTreeSet::new();
    for line_number in matches.iter().take(shown) {
        let first = line_number.saturating_sub(context).max(bounds.0);
        let last = line_number.saturating_add(context).min(bounds.1);
        for line in first..=last {
            selected.insert(line);
        }
    }
    for line_number in selected {
        let line = format!(
            "{line_number}: {}\n",
            line_text(line_slice(&entry.output, starts, line_number))
        );
        if !append_bounded(output, &line) {
            output.push_str(
                "\n[retrieval truncated at the byte cap; refine the search or use a \
                             line range]\n",
            );
            break;
        }
    }
}

fn validate_search(query: &str) -> Result<(), ToolError> {
    if query.trim().is_empty() {
        return Err(ToolError::new(format!(
            "Invalid {READ_TOOL_OUTPUT_NAME} arguments: search must not be empty"
        )));
    }
    if query.chars().count() > MAX_SEARCH_CHARS {
        return Err(ToolError::new(format!(
            "Invalid {READ_TOOL_OUTPUT_NAME} arguments: search exceeds {MAX_SEARCH_CHARS} characters"
        )));
    }
    if query.contains('\n') || query.contains('\r') {
        return Err(ToolError::new(format!(
            "Invalid {READ_TOOL_OUTPUT_NAME} arguments: search must be one line"
        )));
    }
    Ok(())
}

fn range_bounds(
    entry: &ToolOutputEntry,
    args: &ReadArguments,
) -> Result<(usize, usize), ToolError> {
    let start = args.start_line.unwrap_or(1);
    if start == 0 {
        return Err(ToolError::new(format!(
            "Invalid {READ_TOOL_OUTPUT_NAME} arguments: start_line must be 1 or greater"
        )));
    }
    if start > entry.line_count {
        return Err(ToolError::new(format!(
            "Invalid {READ_TOOL_OUTPUT_NAME} arguments: start_line {start} is past the {} \
             available lines",
            entry.line_count
        )));
    }
    if args.end_line == Some(0) {
        return Err(ToolError::new(format!(
            "Invalid {READ_TOOL_OUTPUT_NAME} arguments: end_line must be 1 or greater"
        )));
    }
    let requested_end = args.end_line.unwrap_or(entry.line_count);
    if requested_end < start {
        return Err(ToolError::new(format!(
            "Invalid {READ_TOOL_OUTPUT_NAME} arguments: end_line {requested_end} is before \
             start_line {start}"
        )));
    }
    if args.end_line.is_some()
        && requested_end.saturating_sub(start).saturating_add(1) > MAX_REQUESTED_LINES
    {
        return Err(ToolError::new(format!(
            "Invalid {READ_TOOL_OUTPUT_NAME} arguments: line range exceeds {MAX_REQUESTED_LINES} \
             lines; request a smaller range"
        )));
    }
    Ok((start, requested_end.min(entry.line_count)))
}

fn header(entry: &ToolOutputEntry, detail: &str) -> String {
    format!(
        "{READ_TOOL_OUTPUT_NAME} {tool}; handle={handle}; call_id={call_id}\n\
         Original: {bytes} bytes, {lines} lines\n{detail}\n",
        tool = entry.tool_name,
        handle = entry.handle,
        call_id = entry.call_id,
        bytes = entry.output.len(),
        lines = entry.line_count,
    )
}

fn line_starts(text: &str) -> Vec<usize> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut starts = vec![0];
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' && index + 1 < text.len() {
            starts.push(index + 1);
        }
    }
    starts
}

fn line_slice<'a>(text: &'a str, starts: &[usize], line_number: usize) -> &'a str {
    let Some(start) = starts.get(line_number.saturating_sub(1)).copied() else {
        return "";
    };
    let end = starts.get(line_number).copied().unwrap_or(text.len());
    text.get(start..end).unwrap_or_default()
}

fn line_text(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

fn append_bounded(output: &mut String, text: &str) -> bool {
    let remaining = MAX_RETRIEVAL_BYTES.saturating_sub(output.len());
    if text.len() <= remaining {
        output.push_str(text);
        true
    } else {
        output.push_str(utf8_head(text, remaining));
        false
    }
}

fn utf8_head(text: &str, max_bytes: usize) -> &str {
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or_default()
}
