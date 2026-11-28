use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde::Deserialize;

use crate::clients::tool_output::{READ_TOOL_OUTPUT_NAME, ToolOutputEntry};
use crate::clients::tools::{Tool, ToolContext, ToolError, ToolResult, repair_json_args};
use crate::config::blob_store::TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES;
use crate::text::{utf8_head, utf8_index_ceil, utf8_index_floor};

/// Maximum retrieval body bytes returned to the model.
const MAX_RETRIEVAL_BYTES: usize = TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES;

/// Bytes held back from the body so a truncation footer stays inside the cap.
const FOOTER_RESERVE_BYTES: usize = 256;

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

/// Maximum bytes rendered from one long line in a search result.
const MAX_SEARCH_EXCERPT_BYTES: usize = 1024;

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
                "start_byte": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "0-based byte offset to continue a result whose single line exceeded the cap. Use alone or with end_byte."
                },
                "end_byte": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "0-based exclusive byte offset that bounds a start_byte window."
                },
                "search": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_SEARCH_CHARS,
                    "description": "Literal, case-sensitive text to find within the result. Matching lines include limited context and excerpt a long line around the match."
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
    start_byte: Option<usize>,
    #[serde(default)]
    end_byte: Option<usize>,
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
    if args.start_byte.is_some() {
        return byte_output(entry, args);
    }
    if args.end_byte.is_some() {
        return Err(ToolError::new(format!(
            "Invalid {READ_TOOL_OUTPUT_NAME} arguments: end_byte requires start_byte"
        )));
    }
    let (start, end) = range_bounds(entry, args)?;
    let mut output = header(entry, &format!("Lines {start}-{end}:"));
    let body_limit = MAX_RETRIEVAL_BYTES.saturating_sub(FOOTER_RESERVE_BYTES);
    let starts = line_starts(&entry.output);
    let mut rendered_any = false;
    let mut next_line = None;
    for line_number in start..=end {
        if output.len() >= body_limit {
            next_line = Some((line_number, None));
            break;
        }
        let line = line_slice(&entry.output, &starts, line_number);
        let available = body_limit.saturating_sub(output.len());
        if line.len() <= available {
            output.push_str(line);
            rendered_any = true;
            continue;
        }
        if rendered_any {
            next_line = Some((line_number, None));
        } else {
            let prefix = utf8_head(line, available);
            let line_start = starts
                .get(line_number.saturating_sub(1))
                .copied()
                .unwrap_or(0);
            let next_byte = line_start.saturating_add(prefix.len());
            output.push_str(prefix);
            next_line = Some((line_number, Some(next_byte)));
        }
        break;
    }
    if let Some((line_number, next_byte)) = next_line {
        if let Some(next_byte) = next_byte.filter(|offset| *offset < entry.output.len()) {
            _ = writeln!(
                output,
                "\n[line {line_number} is longer than the {MAX_RETRIEVAL_BYTES}-byte retrieval cap \
                 and was truncated; continue with start_byte={next_byte}]"
            );
        } else {
            _ = writeln!(
                output,
                "\n[retrieval truncated at the {MAX_RETRIEVAL_BYTES}-byte cap; continue with \
                 start_line={line_number}]"
            );
        }
    }
    Ok(finish(output))
}

fn byte_output(entry: &ToolOutputEntry, args: &ReadArguments) -> Result<String, ToolError> {
    if args.start_line.is_some() || args.end_line.is_some() {
        return Err(ToolError::new(format!(
            "Invalid {READ_TOOL_OUTPUT_NAME} arguments: start_byte cannot be combined with \
             start_line or end_line"
        )));
    }
    let Some(requested_start) = args.start_byte else {
        return Err(ToolError::new(format!(
            "Invalid {READ_TOOL_OUTPUT_NAME} arguments: start_byte is required"
        )));
    };
    let total = entry.output.len();
    if requested_start >= total {
        return Err(ToolError::new(format!(
            "Invalid {READ_TOOL_OUTPUT_NAME} arguments: start_byte {requested_start} is past the \
             {total} available bytes"
        )));
    }
    let requested_end = args.end_byte.unwrap_or(total);
    if requested_end <= requested_start {
        return Err(ToolError::new(format!(
            "Invalid {READ_TOOL_OUTPUT_NAME} arguments: end_byte must be after start_byte"
        )));
    }
    let start = utf8_index_ceil(&entry.output, requested_start);
    let mut output = header(
        entry,
        &format!("Bytes from {start} of {total} (0-based, end exclusive):"),
    );
    let body_limit = MAX_RETRIEVAL_BYTES.saturating_sub(FOOTER_RESERVE_BYTES);
    let available = body_limit.saturating_sub(output.len());
    let end = utf8_index_floor(
        &entry.output,
        start,
        requested_end
            .min(total)
            .min(start.saturating_add(available)),
    )
    .max(start);
    output.push_str(entry.output.get(start..end).unwrap_or_default());
    if end < total {
        _ = writeln!(
            output,
            "\n[returned bytes {start}-{end} of {total}; continue with start_byte={end}]"
        );
    }
    Ok(finish(output))
}

fn search_output(
    entry: &ToolOutputEntry,
    args: &ReadArguments,
    query: &str,
) -> Result<String, ToolError> {
    if args.start_byte.is_some() || args.end_byte.is_some() {
        return Err(ToolError::new(format!(
            "Invalid {READ_TOOL_OUTPUT_NAME} arguments: search cannot be combined with byte offsets"
        )));
    }
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
        return Ok(finish(output));
    }
    let shown = matches.len().min(MAX_SEARCH_MATCHES);
    _ = writeln!(
        output,
        "Showing matches{} with {context} context line(s):",
        search_scope_label(shown, matches.len())
    );
    let body_limit = MAX_RETRIEVAL_BYTES.saturating_sub(FOOTER_RESERVE_BYTES);
    let truncated = render_search_matches(
        entry,
        &starts,
        &matches,
        shown,
        context,
        (start, end),
        query,
        body_limit,
        &mut output,
    );
    if truncated {
        output.push_str(
            "\n[search results truncated at the retrieval cap; refine the search or use \
             start_line or start_byte]\n",
        );
    }
    Ok(finish(output))
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

#[expect(
    clippy::too_many_arguments,
    reason = "rendering needs the entry, the match selection, the scope, and the shared budget"
)]
fn render_search_matches(
    entry: &ToolOutputEntry,
    starts: &[usize],
    matches: &[usize],
    shown: usize,
    context: usize,
    bounds: (usize, usize),
    query: &str,
    body_limit: usize,
    output: &mut String,
) -> bool {
    let mut selected = BTreeSet::new();
    for line_number in matches.iter().take(shown) {
        let first = line_number.saturating_sub(context).max(bounds.0);
        let last = line_number.saturating_add(context).min(bounds.1);
        for line in first..=last {
            selected.insert(line);
        }
    }
    for line_number in selected {
        if output.len() >= body_limit {
            return true;
        }
        let available = body_limit.saturating_sub(output.len());
        let line = line_text(line_slice(&entry.output, starts, line_number));
        let Some(rendered) = render_search_line(line_number, line, query, available) else {
            return true;
        };
        output.push_str(&rendered);
    }
    false
}

fn render_search_line(
    line_number: usize,
    line: &str,
    query: &str,
    available: usize,
) -> Option<String> {
    let prefix = format!("{line_number}: ");
    let overhead = prefix.len().saturating_add(1);
    if available <= overhead {
        return None;
    }
    let budget = available
        .saturating_sub(overhead)
        .min(MAX_SEARCH_EXCERPT_BYTES);
    let body = if line.len() <= budget {
        line.to_string()
    } else {
        excerpt_around(line, query, budget)
    };
    let mut rendered = String::with_capacity(overhead.saturating_add(body.len()));
    rendered.push_str(&prefix);
    rendered.push_str(&body);
    rendered.push('\n');
    Some(rendered)
}

fn excerpt_around(line: &str, query: &str, budget: usize) -> String {
    if line.len() <= budget {
        return line.to_string();
    }
    if budget <= 6 {
        return utf8_head(line, budget).to_string();
    }
    let content_budget = budget.saturating_sub(6);
    let Some(match_start) = line.find(query) else {
        let mut excerpt = utf8_head(line, budget.saturating_sub(3)).to_string();
        excerpt.push_str("...");
        return excerpt;
    };
    let match_end = match_start.saturating_add(query.len()).min(line.len());
    let (start, end) = if query.len() <= content_budget {
        let before = content_budget.saturating_sub(query.len()) / 2;
        let start = utf8_index_ceil(line, match_start.saturating_sub(before));
        let mut end = utf8_index_floor(line, start, start.saturating_add(content_budget));
        if end < match_end {
            end = utf8_index_floor(line, start, match_end);
        }
        let start = utf8_index_ceil(line, end.saturating_sub(content_budget));
        (start, end)
    } else {
        let start = utf8_index_ceil(line, match_start);
        (
            start,
            utf8_index_floor(line, start, start.saturating_add(content_budget)),
        )
    };
    let mut excerpt = String::with_capacity(budget);
    if start > 0 {
        excerpt.push_str("...");
    }
    excerpt.push_str(line.get(start..end).unwrap_or_default());
    if end < line.len() {
        excerpt.push_str("...");
    }
    excerpt
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

fn finish(mut output: String) -> String {
    if output.len() > MAX_RETRIEVAL_BYTES {
        let keep = utf8_head(&output, MAX_RETRIEVAL_BYTES).len();
        output.truncate(keep);
    }
    output
}
