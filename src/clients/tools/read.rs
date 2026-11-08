use serde::Deserialize;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use crate::clients::tools::{
    ToolContext, contains_null_byte, decode_utf8, file_access::validate_regular_file,
    resolve_path_for_read_scheduling, validate_path_in_cwd,
};
use crate::config::settings::ToolLimits;
use crate::session_telemetry::{CompensationEventTelemetry, CompensationKind};

#[cfg(test)]
use crate::config::settings::{DEFAULT_READ_MAX_LINE_BYTES, DEFAULT_READ_MAX_OUTPUT_BYTES};

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
                    "minimum": 1,
                    "description": "First line to read (1-indexed, inclusive). Default: 1"
                },
                "end_line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Last line to read (1-indexed, inclusive). Default: the configured read_default_end_line window (500 out of the box; start_line+window-1 when start_line given)"
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

/// Return the validated canonical path this Read call would observe.
///
/// Scheduling uses this to serialize the read with same-path mutations in the
/// same turn. Unresolvable arguments or paths return `Err`, leaving the call
/// unscheduled so it can surface its own error.
pub(super) fn path_target(
    context: &ToolContext,
    arguments: &str,
) -> Result<std::path::PathBuf, String> {
    let args: ReadArgs =
        serde_json::from_str(arguments).map_err(|e| format!("Invalid read arguments: {e}"))?;
    resolve_path_for_read_scheduling(context, &args.path)
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

    // Reject directories, pipes, sockets, and other objects before the
    // blocking open; a named pipe with no writer never returns from it.
    validate_regular_file(&path)?;

    // Handle file
    read_file(&path, args.start_line, args.end_line, &context.limits)
        .map_err(|error| append_image_hint(error, &path))
}

/// Resolve the 0-indexed end line from the model's `start_line`/`end_line`
/// arguments and the configured default window.
///
/// When `start_line` is provided without `end_line`, the window expands from
/// `start_line` instead of keeping the absolute default. An unlimited window
/// (`read_default_end_line = "unlimited"`) reads to the end of the file,
/// bounded by the configured `read_max_output_bytes` output budget.
const fn end_requested_line(
    start: usize,
    start_line: Option<usize>,
    end_line: Option<usize>,
    window: Option<usize>,
) -> usize {
    match (end_line, window) {
        (Some(end), _) => end.saturating_sub(1),
        (None, Some(window)) if start_line.is_some() => {
            // Window of `window` lines starting from start_line
            start.saturating_add(window - 1)
        },
        (None, Some(window)) => window.saturating_sub(1),
        (None, None) => usize::MAX,
    }
}

/// Lines counted past the requested window before stopping the read.
///
/// The exact total is reported only when EOF arrives within this budget;
/// otherwise the header and footer use "at least N" phrasing so the reported
/// count never understates the file length. Counting further would trade I/O
/// proportional to the file size for an exact total.
const EXTRA_LINES_TO_COUNT: usize = 1000;

/// Minimum prefix scanned for null bytes when the requested window is small.
///
/// This preserves the Read tool's previous binary-detection coverage without
/// adding a second file pass. The line scanner may consume more to finish a
/// line or to count the footer.
const BINARY_DETECTION_BYTES: usize = 8192;

// Maximum bytes of one line delivered to the model.
//
// A longer line is held only up to this cap plus room for its trailing
// newline; the rest is consumed and discarded, so memory grows with the cap,
// not with the longest line (a newline-free 50 MB line no longer allocates
// the whole line). The displayed prefix ends with a
// `... (line truncated to N bytes)` marker. The cap is configurable via the
// `read_max_line_bytes` `[limits]` key (compiled default 10,000); `"unlimited"`
// re-enables the pre-#362 whole-line-in-memory behavior. `read-description.txt`
// documents the config and must change with it.

/// Read and format a file with line numbers.
fn read_file(
    path: &Path,
    start_line: Option<usize>,
    end_line: Option<usize>,
    limits: &ToolLimits,
) -> Result<super::ToolResult, String> {
    let start_value = start_line.unwrap_or(1);
    let line_cap = limits.read_max_line_bytes.unwrap_or(usize::MAX);

    let file = std::fs::File::open(path)
        .map_err(|e| format!("Failed to read file '{}': {e}", path.display()))?;
    let mut reader = BufReader::new(file);

    if start_value == 0 {
        let total_lines = count_lines_to_eof(&mut reader, path, line_cap)?;
        return Err(invalid_range_error(
            path,
            "start_line must be 1 or greater",
            total_lines,
        ));
    }
    if end_line == Some(0) {
        let total_lines = count_lines_to_eof(&mut reader, path, line_cap)?;
        return Err(invalid_range_error(
            path,
            "end_line must be 1 or greater",
            total_lines,
        ));
    }

    // Default line range (1-indexed from caller, convert to 0-indexed).
    let start = start_value - 1;
    let end_requested =
        end_requested_line(start, start_line, end_line, limits.read_default_end_line);
    read_from_reader(&mut reader, start, end_requested, path, limits)
}

/// Read a line range from an open reader and format it with line numbers.
///
/// The work is bounded rather than proportional to the file:
/// - Each line is read into a reused buffer capped at `read_max_line_bytes`
///   bytes; the rest of an over-long line is consumed and discarded, so a
///   newline-free giant line cannot grow memory (or per-line allocation).
/// - In-window lines accumulate into a single `String` only while the running
///   body fits `read_max_output_bytes`. The header and footer are then checked
///   against the same cap, and whole lines are dropped until the result fits,
///   so a byte cap never cuts a line or invalidates the reported range.
/// - After the last emitted line, at most [`EXTRA_LINES_TO_COUNT`] more lines
///   are read to decide the footer. EOF within that budget yields the exact
///   total; otherwise the header and footer use "at least N" phrasing, which
///   never understates the file length.
/// - Null bytes are detected in every byte consumed by this bounded scan,
///   including bytes discarded from over-long lines. The scan consumes at
///   least [`BINARY_DETECTION_BYTES`] bytes when available, preserving the
///   previous detection coverage; an unconsumed suffix is not inspected.
fn read_from_reader<R: BufRead>(
    reader: &mut R,
    start: usize,
    end_requested: usize,
    path: &Path,
    limits: &ToolLimits,
) -> Result<super::ToolResult, String> {
    let line_cap = limits.read_max_line_bytes.unwrap_or(usize::MAX);
    if start > end_requested {
        let total_lines = count_lines_to_eof(reader, path, line_cap)?;
        return Err(invalid_range_error(
            path,
            &format!(
                "start_line ({}) is greater than end_line ({})",
                start + 1,
                end_requested + 1
            ),
            total_lines,
        ));
    }

    // An unlimited budget accumulates the whole file by design.
    let max_bytes = limits.read_max_output_bytes;

    let mut builder = BodyBuilder::new(max_bytes, line_cap);
    let mut total_lines: usize = 0;
    let mut scanned_bytes: usize = 0;
    let mut exact = false;
    let mut line_buf = Vec::new();
    let mut first_line_overflow = None;

    loop {
        let Some(line) = read_line_bounded(reader, &mut line_buf, line_cap)
            .map_err(|e| format!("Failed to read file '{}': {e}", path.display()))?
        else {
            exact = true;
            break; // EOF
        };

        reject_binary_line(&line, path)?;

        scanned_bytes = scanned_bytes.saturating_add(line.bytes_consumed);
        let i = total_lines;
        total_lines += 1;

        if i < start {
            continue; // line_buf reused, no allocation
        }

        if let Some(attempted_len) = builder.push(i, end_requested, &line) {
            first_line_overflow = Some(attempted_len);
            break;
        }

        // Stop once the window is satisfied and a bounded number of lines
        // past the last emitted line has been counted for the footer.
        if past_counting_budget(builder.last_emitted, i, scanned_bytes) {
            break;
        }
    }

    if let Some(line_bytes) = first_line_overflow {
        return Ok(read_cap_too_small_result(
            path,
            start + 1,
            line_bytes,
            max_bytes.unwrap_or(usize::MAX),
            None,
        ));
    }

    let Some(end) = builder.last_emitted else {
        return no_line_result(path, start, total_lines);
    };

    Ok(build_read_result(ReadResultInput {
        path,
        start,
        end,
        total_lines,
        exact,
        body: builder.body,
        line_ends: builder.line_ends,
        max_bytes,
        cap_stopped: builder.cap_stopped,
    }))
}

/// No line at `start` exists. An empty file is a normal result; a non-empty
/// file means the requested start line is past EOF.
fn no_line_result(
    path: &Path,
    start: usize,
    total_lines: usize,
) -> Result<super::ToolResult, String> {
    if total_lines == 0 {
        Ok(empty_file_result(path))
    } else {
        Err(start_past_eof_error(path, start + 1, total_lines))
    }
}

/// Accumulates delivered lines until the requested window closes or the body
/// reaches the byte budget. Counted-but-undelivered lines stay out of `body`.
struct BodyBuilder {
    body: String,
    line_ends: Vec<usize>,
    last_emitted: Option<usize>,
    body_budget: usize,
    line_cap: usize,
    cap_stopped: bool,
    window_closed: bool,
}

impl BodyBuilder {
    fn new(max_bytes: Option<usize>, line_cap: usize) -> Self {
        Self {
            body: String::new(),
            line_ends: Vec::new(),
            last_emitted: None,
            body_budget: max_bytes.unwrap_or(usize::MAX),
            line_cap,
            cap_stopped: false,
            window_closed: false,
        }
    }

    /// Try to append one in-window line. Returns the attempted body length when
    /// the first line alone exceeds the byte budget; otherwise `None`.
    fn push(&mut self, i: usize, end_requested: usize, line: &BoundedLine<'_>) -> Option<usize> {
        if self.window_closed {
            return None;
        }
        if i > end_requested {
            self.window_closed = true;
            return None;
        }

        let checkpoint = self.body.len();
        append_line(&mut self.body, line, i, self.line_cap);
        if self.body.len() <= self.body_budget {
            self.line_ends.push(self.body.len());
            self.last_emitted = Some(i);
            return None;
        }

        let attempted_len = self.body.len();
        self.body.truncate(checkpoint);
        if self.last_emitted.is_none() {
            return Some(attempted_len);
        }
        self.cap_stopped = true;
        self.window_closed = true;
        None
    }
}

/// Append one numbered line to the body, marking a line truncated at the
/// per-line byte cap.
fn append_line(body: &mut String, line: &BoundedLine<'_>, i: usize, cap: usize) {
    if !body.is_empty() {
        body.push('\n');
    }
    _ = write!(body, "{:>6}: {}", i + 1, line.text);
    append_truncation_marker(body, line.truncated, cap);
}

/// Append the per-line truncation marker when the line was cut at the cap.
fn append_truncation_marker(body: &mut String, truncated: bool, cap: usize) {
    if truncated {
        _ = write!(body, " ... (line truncated to {cap} bytes)");
    }
}

/// One line read by [`read_line_bounded`]: the text without its trailing
/// newline, whether it was cut at the byte cap, and whether the consumed line
/// contained a null byte.
struct BoundedLine<'a> {
    /// The line content, truncated to the byte cap at a UTF-8 boundary when
    /// the line is longer than the cap.
    text: &'a str,
    /// Whether the line exceeded the cap and `text` is a truncated prefix.
    truncated: bool,
    /// Whether any byte in the consumed line was a null byte. This includes
    /// bytes beyond the returned text when an over-long line is discarded.
    contains_null_byte: bool,
    /// Number of bytes consumed from the reader for this line, including its
    /// newline and bytes discarded after the per-line cap.
    bytes_consumed: usize,
}

/// Read one line into `buf`, keeping at most `cap` bytes of content.
///
/// `buf` is cleared and refilled each call so lines do not allocate. Once the
/// cap plus room for the trailing newline is reached, the rest of an over-long
/// line is consumed and discarded, so a newline-free giant line cannot grow
/// memory. The line still counts as one line. Invalid UTF-8 in a delivered
/// line is an error, matching `BufRead::read_line`; bytes beyond the cap are
/// still scanned for null bytes before they are discarded.
fn read_line_bounded<'a, R: BufRead>(
    reader: &mut R,
    buf: &'a mut Vec<u8>,
    cap: usize,
) -> Result<Option<BoundedLine<'a>>, std::io::Error> {
    buf.clear();
    // Room for the content plus the trailing newline, so a line of exactly
    // `cap` bytes (with or without a CRLF ending) is delivered without a
    // marker. `scan_line` reports EOF without distinguishing a final
    // unterminated line, so only an empty buffer means truly nothing read.
    let (terminated, contains_null_byte, bytes_consumed) =
        scan_line(reader, buf, cap.saturating_add(2))?;
    if !terminated && buf.is_empty() {
        return Ok(None); // EOF before any byte
    }

    let content = strip_line_ending(buf.as_slice());
    let truncated = content.len() > cap;
    let text = if truncated {
        truncate_to_valid_utf8(content, cap)
    } else {
        decode_utf8(content).map_err(|_err| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "stream did not contain valid UTF-8",
            )
        })?
    };
    Ok(Some(BoundedLine {
        text,
        truncated,
        contains_null_byte,
        bytes_consumed,
    }))
}

/// Scan `reader` to the end of one line, appending at most `budget` bytes to
/// `buf`. The rest of an over-long line is consumed and discarded, while
/// null-byte detection covers those discarded bytes too. Returns whether the
/// terminating newline was consumed and whether the line contained a null
/// byte; false from the first value means EOF arrived first — a final
/// unterminated line still returns its content. The third value is the number
/// of bytes consumed from the reader, including discarded bytes.
fn scan_line<R: BufRead>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    budget: usize,
) -> Result<(bool, bool, usize), std::io::Error> {
    let mut has_null_byte = false;
    let mut bytes_consumed = 0usize;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok((false, has_null_byte, bytes_consumed)); // EOF
        }
        let (take, done) = available
            .iter()
            .position(|&b| b == b'\n')
            .map_or_else(|| (available.len(), false), |pos| (pos + 1, true));
        has_null_byte |= contains_null_byte(&available[..take]);
        bytes_consumed = bytes_consumed.saturating_add(take);
        if buf.len() < budget {
            let room = budget - buf.len();
            buf.extend_from_slice(&available[..take.min(room)]);
        }
        reader.consume(take);
        if done {
            return Ok((true, has_null_byte, bytes_consumed));
        }
    }
}

/// The line content without its trailing newline and CR.
fn strip_line_ending(raw: &[u8]) -> &[u8] {
    let mut content = raw;
    if content.ends_with(b"\n") {
        content = &content[..content.len() - 1];
    }
    if content.ends_with(b"\r") {
        content = &content[..content.len() - 1];
    }
    content
}

/// The longest valid-UTF-8 prefix of `bytes` within the first `max` bytes.
///
/// Returns `max` itself when already valid; otherwise the prefix ends at a
/// char boundary before the first invalid byte. The suffix is discarded, so an
/// over-long line never needs to be decoded past the cap.
fn truncate_to_valid_utf8(bytes: &[u8], max: usize) -> &str {
    let max = max.min(bytes.len());
    match decode_utf8(&bytes[..max]) {
        Ok(s) => s,
        Err(e) => {
            let valid = e.valid_up_to();
            #[expect(
                clippy::expect_used,
                reason = "valid_up_to guarantees the prefix is valid UTF-8"
            )]
            decode_utf8(&bytes[..valid]).expect("valid_up_to always yields a valid UTF-8 prefix")
        },
    }
}

/// Whether the read should stop: the window is satisfied, the bounded footer
/// budget has been counted, and the minimum binary-detection prefix is covered.
fn past_counting_budget(last_emitted: Option<usize>, i: usize, scanned_bytes: usize) -> bool {
    last_emitted.is_some_and(|end| {
        i >= end.saturating_add(EXTRA_LINES_TO_COUNT) && scanned_bytes >= BINARY_DETECTION_BYTES
    })
}

/// The header's total: the exact count, or a lower bound with "at least" so
/// the model can never conclude the file is shorter than it is.
fn total_label(exact: bool, total_lines: usize) -> String {
    if exact {
        total_lines.to_string()
    } else {
        format!("at least {total_lines}")
    }
}

/// The delivered lines and the metadata needed to size and format the result.
struct ReadResultInput<'a> {
    path: &'a Path,
    start: usize,
    end: usize,
    total_lines: usize,
    exact: bool,
    body: String,
    line_ends: Vec<usize>,
    max_bytes: Option<usize>,
    cap_stopped: bool,
}

/// Build the final result from the delivered body, dropping whole lines until
/// the header, body, and footer fit the configured output budget.
fn build_read_result(input: ReadResultInput<'_>) -> super::ToolResult {
    let ReadResultInput {
        path,
        start,
        mut end,
        total_lines,
        exact,
        mut body,
        line_ends,
        max_bytes,
        mut cap_stopped,
    } = input;
    let first_line_bytes = line_ends.first().copied().unwrap_or(body.len());
    let mut delivered = line_ends.len();

    loop {
        let header = format_header(path, start, end, total_lines, exact);
        let footer = more_lines_footer(
            end,
            total_lines,
            exact,
            if cap_stopped { max_bytes } else { None },
        );
        let total_len = header.len() + body.len() + footer.as_ref().map_or(0, String::len);

        if max_bytes.is_none_or(|cap| total_len <= cap) {
            let mut output = header;
            output.push_str(&body);
            if let Some(footer) = footer {
                output.push_str(&footer);
            }
            let compensation_events = if cap_stopped {
                vec![CompensationEventTelemetry::new(
                    CompensationKind::OutputTruncation,
                    Some("Read".to_string()),
                )]
            } else {
                Vec::new()
            };
            return super::ToolResult {
                output,
                images: Vec::new(),
                compensation_events,
                permission_denials: Vec::new(),
            };
        }

        cap_stopped = true;
        let needed = total_len;
        delivered = delivered.saturating_sub(1);
        body.truncate(if delivered == 0 {
            0
        } else {
            line_ends[delivered - 1]
        });
        if delivered == 0 {
            return read_cap_too_small_result(
                path,
                start + 1,
                first_line_bytes,
                max_bytes.unwrap_or(needed),
                Some(needed),
            );
        }
        end = start + delivered - 1;
    }
}

/// Format the `File:` and `Lines X-Y/Z` header for the delivered range.
fn format_header(path: &Path, start: usize, end: usize, total_lines: usize, exact: bool) -> String {
    format!(
        "File: {}\nLines {}-{}/{}\n",
        path.display(),
        start + 1,
        end + 1,
        total_label(exact, total_lines)
    )
}

/// The single footer shape for a stopped read: how many lines remain and where
/// to resume. A byte-cap stop also names the cap. `None` at EOF.
fn more_lines_footer(
    end: usize,
    total_lines: usize,
    exact: bool,
    cap: Option<usize>,
) -> Option<String> {
    let remaining = total_lines.saturating_sub(end.saturating_add(1));
    if remaining == 0 {
        return None;
    }
    let resume = end.saturating_add(2);
    let count = if exact {
        format!("{remaining} more lines")
    } else {
        format!("at least {remaining} more lines")
    };
    let cap = cap.map_or(String::new(), |cap| {
        format!("output truncated at {cap} bytes; ")
    });
    Some(format!(
        "\n[... {cap}{count}; continue with start_line={resume} ...]"
    ))
}

/// A clear result for a byte cap too small to hold the header, one line, and
/// the footer. It never cuts a line or reports a range it did not deliver.
fn read_cap_too_small_result(
    path: &Path,
    line: usize,
    line_bytes: usize,
    cap: usize,
    needed: Option<usize>,
) -> super::ToolResult {
    let needed = needed.map_or(String::new(), |needed| {
        format!("; the first line plus the Read header and footer needs {needed} bytes")
    });
    super::ToolResult {
        output: format!(
            "File: {}\n[... output truncated at {cap} bytes; line {line} needs {line_bytes} bytes{needed} ...]",
            path.display()
        ),
        images: Vec::new(),
        compensation_events: vec![CompensationEventTelemetry::new(
            CompensationKind::OutputTruncation,
            Some("Read".to_string()),
        )],
        permission_denials: Vec::new(),
    }
}

/// An empty file has no range to report and no truncation.
fn empty_file_result(path: &Path) -> super::ToolResult {
    super::ToolResult {
        output: format!("File: {}\n0 lines total\n(file is empty)", path.display()),
        images: Vec::new(),
        compensation_events: Vec::new(),
        permission_denials: Vec::new(),
    }
}

/// The model-visible error for a range that is invalid before any line is read.
fn invalid_range_error(path: &Path, detail: &str, total_lines: usize) -> String {
    format!(
        "Invalid Read range for '{}': {detail}; the file has {}.",
        path.display(),
        lines_label(total_lines)
    )
}

/// The model-visible error for a start line beyond the last line.
fn start_past_eof_error(path: &Path, start_line: usize, total_lines: usize) -> String {
    format!(
        "Invalid Read range for '{}': start_line ({start_line}) is past the end of the file ({}).",
        path.display(),
        lines_label(total_lines)
    )
}

fn lines_label(lines: usize) -> String {
    format!("{lines} line{}", if lines == 1 { "" } else { "s" })
}

/// Count every line of `reader` to EOF, reusing one buffer capped at
/// `read_max_line_bytes` per line (no per-line allocation). Used when an invalid
/// range needs the exact total in the model-visible error.
fn count_lines_to_eof<R: BufRead>(
    reader: &mut R,
    path: &Path,
    line_cap: usize,
) -> Result<usize, String> {
    let mut buf = Vec::new();
    let mut total_lines = 0usize;
    while let Some(line) = read_line_bounded(reader, &mut buf, line_cap)
        .map_err(|e| format!("Failed to read file '{}': {e}", path.display()))?
    {
        reject_binary_line(&line, path)?;
        total_lines += 1;
    }
    Ok(total_lines)
}

/// Return the model-visible error for a file containing null bytes.
fn binary_file_error(path: &Path) -> String {
    format!(
        "Cannot read binary file: {} (detected null bytes)",
        path.display()
    )
}

/// Append the `ReadImage` hint to a rejected file's read error when its
/// leading bytes look like an image, so the model learns which tool can
/// deliver it. Covers both binary-rejection paths: a null byte in decodable
/// text and a line that is not valid UTF-8 (where real PNG, JPEG, and WebP
/// files land).
fn append_image_hint(error: String, path: &Path) -> String {
    match probe_image_media_type(path) {
        Some(media_type) => format!("{error} (looks like {media_type}; use ReadImage)"),
        None => error,
    }
}

/// Leading bytes examined when probing a rejected binary file for an image
/// signature.
const IMAGE_HINT_PROBE_BYTES: usize = 8192;

/// Detect an image MIME type from the leading bytes of `path`.
///
/// The `infer` dependency is compiled without its `std` feature, so the file
/// is read here and the bytes are handed to the buffer-based detector.
fn probe_image_media_type(path: &Path) -> Option<&'static str> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut header = [0u8; IMAGE_HINT_PROBE_BYTES];
    let read = file.read(&mut header).ok()?;
    infer::get(&header[..read])
        .map(|kind| kind.mime_type())
        .filter(|media_type| media_type.starts_with("image/"))
}

/// Reject a consumed line when it contains a null byte.
fn reject_binary_line(line: &BoundedLine<'_>, path: &Path) -> Result<(), String> {
    if line.contains_null_byte {
        Err(binary_file_error(path))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::clients::tools::file_access::process_isolation;
    use std::fmt::Write;
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
    fn default_read_is_1_to_500() {
        // Neither start_line nor end_line defaults to the compiled 500-line
        // window.
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let lines: Vec<String> = (1..=501).map(|i| format!("Line {i}")).collect();
        fs::write(&file_path, lines.join("\n")).unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap()
        })
        .to_string();

        let result = execute_read(&ToolContext::from_current_process(), &args).unwrap();
        assert!(result.output.contains("Lines 1-500/501"));
        assert!(result.output.contains("   500: Line 500"));
        assert!(result.output.contains("     1: Line 1"));
        assert!(
            result
                .output
                .contains("[... 1 more lines; continue with start_line=501 ...]")
        );
        assert!(!result.output.contains("   501: "));
        assert!(result.compensation_events.is_empty());
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
        assert!(
            result
                .output
                .contains("[... 1 more lines; continue with start_line=3 ...]")
        );
        assert!(result.compensation_events.is_empty());
    }

    #[test]
    fn start_line_after_end_line_is_rejected_with_total() {
        // An invalid requested window counts to EOF so the error can report
        // the exact total without understating it.
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "a\nb\nc\nd\ne").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "start_line": 4,
            "end_line": 2
        })
        .to_string();

        let err = execute_read(&ToolContext::from_current_process(), &args).unwrap_err();
        assert!(
            err.contains("start_line (4) is greater than end_line (2)"),
            "{err}"
        );
        assert!(err.contains("5 lines"), "{err}");
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

    #[cfg(unix)]
    #[test]
    fn read_rejects_fifo_without_a_writer() {
        // A regression in the object-type precheck would make the blocking
        // open wait forever, so the body runs in a child process with an
        // external deadline.
        if !process_isolation::in_child() {
            process_isolation::run_isolated(
                "read_rejects_fifo_without_a_writer",
                std::time::Duration::from_secs(10),
            );
            return;
        }

        let temp_dir = TempDir::new().unwrap();
        let fifo = temp_dir.path().join("pipe.txt");
        create_fifo(&fifo);

        let args = serde_json::json!({ "path": fifo.to_str().unwrap() }).to_string();
        let err = execute_read(&ToolContext::from_current_process(), &args).unwrap_err();

        assert!(err.contains("is not a regular file"), "{err}");
        assert!(err.contains("pipe.txt"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn read_rejects_unix_socket() {
        let temp_dir = TempDir::new().unwrap();
        let socket = temp_dir.path().join("agent.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();

        let args = serde_json::json!({ "path": socket.to_str().unwrap() }).to_string();
        let err = execute_read(&ToolContext::from_current_process(), &args).unwrap_err();

        assert!(err.contains("is not a regular file"), "{err}");
        assert!(err.contains("agent.sock"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn read_follows_symlink_to_regular_file() {
        let temp_dir = TempDir::new().unwrap();
        let target = temp_dir.path().join("target.txt");
        fs::write(&target, "Line 1\nLine 2").unwrap();
        let link = temp_dir.path().join("link.txt");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let args = serde_json::json!({ "path": link.to_str().unwrap() }).to_string();
        let result = execute_read(&ToolContext::from_current_process(), &args).unwrap();

        assert!(
            result.output.contains("     1: Line 1"),
            "{}",
            result.output
        );
    }

    /// Create a named pipe with no writer, which blocks `File::open` until the
    /// object-type precheck rejects it.
    #[cfg(unix)]
    fn create_fifo(path: &std::path::Path) {
        let status = std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success(), "mkfifo failed for {}", path.display());
    }

    #[test]
    fn start_line_without_end_line_returns_window() {
        // When start_line is provided without end_line, the window should be
        // start_line..start_line+499, not the absolute 1-500 default.
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
        assert!(result.output.contains("Lines 200-699/720"));
        assert!(result.output.contains("   200: Line 200"));
        assert!(result.output.contains("   699: Line 699"));
        assert!(!result.output.contains("Line 199"));
        assert!(!result.output.contains("   700: "));
        assert!(
            result
                .output
                .contains("[... 21 more lines; continue with start_line=700 ...]")
        );
    }

    #[test]
    fn start_line_without_end_line_window_in_middle() {
        // When the file is long enough, start_line+499 should be the end.
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
        assert!(result.output.contains("Lines 300-799/1000"));
        assert!(result.output.contains("   300: Line 300"));
        assert!(result.output.contains("   799: Line 799"));
        assert!(!result.output.contains("Line 299"));
        assert!(!result.output.contains("   800: "));
        assert!(
            result
                .output
                .contains("[... 201 more lines; continue with start_line=800 ...]")
        );
    }

    #[test]
    fn start_line_at_end_of_file_is_rejected() {
        // start_line beyond the file is an invalid range, not an empty window.
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let lines: Vec<String> = (1..=10).map(|i| format!("Line {i}")).collect();
        fs::write(&file_path, lines.join("\n")).unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "start_line": 20
        })
        .to_string();

        let err = execute_read(&ToolContext::from_current_process(), &args).unwrap_err();
        assert!(
            err.contains("start_line (20) is past the end of the file"),
            "{err}"
        );
        assert!(err.contains("10 lines"), "{err}");
    }

    #[test]
    fn read_early_window_large_file() {
        // An early window of a large file must not read to EOF. The exact
        // total is unreachable at a bounded cost, so the header and footer
        // report a lower bound ("at least") that cannot understate the file.
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("large.txt");
        let mut content = String::with_capacity(6_000_000);
        for i in 1..=100_000 {
            let _result = writeln!(content, "Line number {i}");
        }
        fs::write(&file_path, &content).unwrap();
        drop(content);

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "start_line": 1,
            "end_line": 200
        })
        .to_string();

        let result = execute_read(&ToolContext::from_current_process(), &args).unwrap();
        assert!(result.output.contains("Lines 1-200/at least 1200"));
        assert!(result.output.contains("   200: Line number 200"));
        assert!(!result.output.contains("Line number 201"));
        assert!(
            result
                .output
                .contains("[... at least 1000 more lines; continue with start_line=201 ...]")
        );
    }

    /// A reader that counts bytes served to the caller, used to assert that
    /// the Read tool stops well short of EOF on large files.
    struct CountingReader<R> {
        inner: R,
        bytes: usize,
    }

    impl<R: std::io::Read> std::io::Read for CountingReader<R> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.inner.read(buf)?;
            self.bytes += n;
            Ok(n)
        }
    }

    #[test]
    fn huge_end_line_bounds_read_volume() {
        // A huge end_line on a large file must stop at the output byte
        // budget plus a bounded count, not read to EOF — so memory and I/O
        // scale with the budget, not with the requested range.
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("big.log");
        let lines: Vec<String> = (1..=200_000).map(|i| format!("log line {i}")).collect();
        let content = lines.join("\n");
        fs::write(&file_path, &content).unwrap();
        let file_size = content.len();

        let file = std::fs::File::open(&file_path).unwrap();
        let counter = CountingReader {
            inner: file,
            bytes: 0,
        };
        let mut reader = BufReader::new(counter);

        let limits = crate::config::settings::ToolLimits::defaults();
        let result = read_from_reader(&mut reader, 0, 199_999_999, &file_path, &limits).unwrap();

        let consumed = reader.get_ref().bytes;
        assert!(
            consumed < file_size / 4,
            "read {consumed} of {file_size} bytes; expected to stop well before EOF"
        );

        let output = &result.output;
        assert!(output.contains("output truncated"));
        assert!(output.contains("at least"));
        assert!(output.len() <= DEFAULT_READ_MAX_OUTPUT_BYTES as usize);
        assert_eq!(
            result.compensation_events.len(),
            1,
            "truncated read must record one compensation event"
        );
    }

    #[test]
    fn read_truncation_with_multibyte_utf8() {
        // Multibyte output must respect the documented byte cap without
        // splitting a code point.
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("emoji.txt");

        // Build content that when formatted exceeds the default read cap
        // (100_000 bytes). Using '€' (3 bytes, U+20AC): 200 lines of 600 €
        // each are 1800 bytes per line — below the per-line cap, so only the
        // output cap truncates. 200 × 1800 ≈ 360_000 bytes, over the cap.
        let lines: Vec<String> = (0..200).map(|_| "\u{20AC}".repeat(600)).collect();
        fs::write(&file_path, lines.join("\n")).unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap()
        })
        .to_string();

        let result = execute_read(&ToolContext::from_current_process(), &args).unwrap();
        let output = &result.output;

        // Output must not exceed the byte cap
        assert!(
            output.len() <= DEFAULT_READ_MAX_OUTPUT_BYTES as usize,
            "Output is {} bytes but cap is {}",
            output.len(),
            DEFAULT_READ_MAX_OUTPUT_BYTES
        );

        // Truncation marker must be present
        assert!(
            output.contains("output truncated"),
            "Expected truncation marker in output of {} bytes",
            output.len()
        );

        // No replacement character from a split code point
        assert!(
            !output.contains('\u{FFFD}'),
            "Output contains replacement character (split code point)"
        );

        // The truncation is recorded as a compensation event for telemetry.
        assert_eq!(
            result.compensation_events.len(),
            1,
            "truncated read must record one compensation event"
        );
        assert_eq!(
            result.compensation_events[0].kind,
            crate::session_telemetry::CompensationKind::OutputTruncation
        );
        assert_eq!(
            result.compensation_events[0].detail.as_deref(),
            Some("Read")
        );
    }

    /// A reader that returns data in small chunks to simulate short reads
    /// from FIFOs, network mounts, or FUSE filesystems.
    struct ShortReader<'a> {
        data: &'a [u8],
        chunk_size: usize,
        pos: usize,
    }

    impl<'a> ShortReader<'a> {
        fn new(data: &'a [u8], chunk_size: usize) -> Self {
            Self {
                data,
                chunk_size,
                pos: 0,
            }
        }
    }

    impl std::io::Read for ShortReader<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pos >= self.data.len() {
                return Ok(0);
            }
            let chunk = self
                .chunk_size
                .min(buf.len())
                .min(self.data.len() - self.pos);
            let chunk = chunk.max(1); // always return at least 1 byte if data remains
            buf[..chunk].copy_from_slice(&self.data[self.pos..self.pos + chunk]);
            self.pos += chunk;
            Ok(chunk)
        }
    }

    #[test]
    fn read_line_bounded_detects_null_byte_across_short_reads() {
        // A reader that returns data in 100-byte chunks with a null byte
        // beyond the first few chunks.
        let mut data = vec![b'x'; 8192];
        data[512] = 0x00;
        let short_reader = ShortReader::new(&data, 100);
        let mut reader = std::io::BufReader::new(short_reader);
        let mut buf = Vec::new();
        let line = read_line_bounded(&mut reader, &mut buf, DEFAULT_READ_MAX_LINE_BYTES as usize)
            .unwrap()
            .expect("data should produce a line");
        assert!(
            line.contains_null_byte,
            "should detect null byte across short-read boundaries"
        );
    }

    #[test]
    fn read_line_bounded_short_read_no_null() {
        // A reader returning data in small chunks without null bytes.
        let data = vec![b'x'; 8192];
        let short_reader = ShortReader::new(&data, 100);
        let mut reader = std::io::BufReader::new(short_reader);
        let mut buf = Vec::new();
        let line = read_line_bounded(&mut reader, &mut buf, DEFAULT_READ_MAX_LINE_BYTES as usize)
            .unwrap()
            .expect("data should produce a line");
        assert!(
            !line.contains_null_byte,
            "text without null bytes should not be detected as binary"
        );
    }

    #[test]
    fn read_line_bounded_short_read_short_file_no_null() {
        // A file shorter than the scan window, returned in tiny chunks.
        let data = vec![b'a'; 42];
        let short_reader = ShortReader::new(&data, 7);
        let mut reader = std::io::BufReader::new(short_reader);
        let mut buf = Vec::new();
        let line = read_line_bounded(&mut reader, &mut buf, DEFAULT_READ_MAX_LINE_BYTES as usize)
            .unwrap()
            .expect("data should produce a line");
        assert!(
            !line.contains_null_byte,
            "short text without null bytes should not be detected as binary"
        );
    }

    #[test]
    fn error_on_binary_file() {
        // Binary files (containing null bytes) must be rejected.
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("binary.bin");
        // A file with a null byte early on
        fs::write(
            &file_path,
            [
                b'h', b'e', b'l', b'l', b'o', 0x00, b'w', b'o', b'r', b'l', b'd',
            ],
        )
        .unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap()
        })
        .to_string();

        let result = execute_read(&ToolContext::from_current_process(), &args);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Cannot read binary file"), "{err}");
        assert!(err.contains("detected null bytes"), "{err}");
    }

    #[test]
    fn early_window_preserves_initial_binary_detection() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("early-window-late-null.txt");
        let mut content = b"a\n".repeat(5_000);
        content[5_000] = 0;
        fs::write(&file_path, content).unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap()
        })
        .to_string();

        let result = execute_read(&ToolContext::from_current_process(), &args);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Cannot read binary file"), "{err}");
        assert!(err.contains("detected null bytes"), "{err}");
    }

    #[test]
    fn error_on_null_byte_after_initial_8k() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("text-with-late-null.txt");
        let mut content = vec![b'a'; DEFAULT_READ_MAX_LINE_BYTES as usize + 100];
        content.push(0);
        content.extend_from_slice(b"tail");
        fs::write(&file_path, content).unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap()
        })
        .to_string();

        let result = execute_read(&ToolContext::from_current_process(), &args);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Cannot read binary file"), "{err}");
        assert!(err.contains("detected null bytes"), "{err}");
        assert!(err.ends_with("(detected null bytes)"), "{err}");
    }

    #[test]
    fn binary_image_file_hint_points_at_read_image() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("red.png");
        // The PNG signature, which is invalid UTF-8, so Read rejects the file
        // before it can report a null byte. `infer` detects PNG from the first
        // four bytes, which is exactly what the hint probes.
        fs::write(&file_path, b"\x89PNG\r\n\x1a\n").unwrap();

        let args = serde_json::json!({ "path": file_path.to_str().unwrap() }).to_string();
        let err = execute_read(&ToolContext::from_current_process(), &args).unwrap_err();

        assert!(err.contains("stream did not contain valid UTF-8"), "{err}");
        assert!(err.contains("looks like image/png"), "{err}");
        assert!(err.contains("use ReadImage"), "{err}");
    }

    #[test]
    fn null_byte_image_file_hint_points_at_read_image() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("loop.gif");
        // GIF's signature is valid UTF-8, so this file reaches the null-byte
        // rejection instead of the UTF-8 one.
        fs::write(&file_path, b"GIF89a\0").unwrap();

        let args = serde_json::json!({ "path": file_path.to_str().unwrap() }).to_string();
        let err = execute_read(&ToolContext::from_current_process(), &args).unwrap_err();

        assert!(err.contains("detected null bytes"), "{err}");
        assert!(err.contains("looks like image/gif"), "{err}");
        assert!(err.contains("use ReadImage"), "{err}");
    }

    #[test]
    fn binary_text_file_hint_omits_read_image() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("notes.bin");
        fs::write(&file_path, b"hello\0world").unwrap();

        let args = serde_json::json!({ "path": file_path.to_str().unwrap() }).to_string();
        let err = execute_read(&ToolContext::from_current_process(), &args).unwrap_err();

        assert!(err.contains("Cannot read binary file"), "{err}");
        assert!(!err.contains("ReadImage"), "{err}");
    }

    // --- [limits] overrides ---

    #[test]
    fn read_default_end_line_override_changes_window() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let lines: Vec<String> = (1..=50).map(|i| format!("Line {i}")).collect();
        fs::write(&file_path, lines.join("\n")).unwrap();

        let mut context = ToolContext::from_current_process();
        let mut limits = crate::config::settings::ToolLimits::defaults();
        limits.read_default_end_line = Some(10);
        context.limits = limits;

        let args = serde_json::json!({ "path": file_path.to_str().unwrap() }).to_string();
        let result = execute_read(&context, &args).unwrap();
        assert!(result.output.contains("Lines 1-10/50"));
        assert!(result.output.contains("    10: Line 10"));
        assert!(!result.output.contains("Line 11"));
    }

    #[test]
    fn read_default_end_line_unlimited_reads_to_eof() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let lines: Vec<String> = (1..=600).map(|i| format!("Line {i}")).collect();
        fs::write(&file_path, lines.join("\n")).unwrap();

        let mut context = ToolContext::from_current_process();
        let mut limits = crate::config::settings::ToolLimits::defaults();
        limits.read_default_end_line = None;
        context.limits = limits;

        let args = serde_json::json!({ "path": file_path.to_str().unwrap() }).to_string();
        let result = execute_read(&context, &args).unwrap();
        assert!(result.output.contains("Lines 1-600/600"));
        assert!(result.output.contains("   600: Line 600"));
    }

    #[test]
    fn read_max_output_bytes_override_truncates_at_custom_cap() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("big.txt");
        let lines: Vec<String> = (1..=100)
            .map(|i| format!("line {i:03} {}", "x".repeat(20)))
            .collect();
        fs::write(&file_path, lines.join("\n")).unwrap();

        let mut context = ToolContext::from_current_process();
        let mut limits = crate::config::settings::ToolLimits::defaults();
        limits.read_max_output_bytes = Some(1000);
        context.limits = limits;

        let args = serde_json::json!({ "path": file_path.to_str().unwrap() }).to_string();
        let result = execute_read(&context, &args).unwrap();
        assert!(result.output.contains("output truncated at 1000 bytes"));
        assert!(result.output.contains("continue with start_line="));
        assert!(
            result.output.len() <= 1000,
            "output is {} bytes but cap is 1000",
            result.output.len()
        );
        assert_eq!(
            result.compensation_events.len(),
            1,
            "truncation must record exactly one output_truncation event"
        );
    }

    #[test]
    fn byte_cap_truncation_ends_on_a_complete_line_and_resumes() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("aligned.txt");
        let lines: Vec<String> = (1..=80)
            .map(|i| format!("line {i:03} {}", "y".repeat(50)))
            .collect();
        fs::write(&file_path, lines.join("\n")).unwrap();

        let mut context = ToolContext::from_current_process();
        let mut limits = crate::config::settings::ToolLimits::defaults();
        limits.read_max_output_bytes = Some(800);
        context.limits = limits;

        let args = serde_json::json!({ "path": file_path.to_str().unwrap() }).to_string();
        let result = execute_read(&context, &args).unwrap();
        let output = &result.output;
        assert!(output.len() <= 800);

        let header = output.lines().nth(1).expect("header line");
        assert!(header.starts_with("Lines 1-"), "{header}");
        assert!(header.ends_with("/80"), "{header}");
        let delivered_end: usize = header
            .strip_prefix("Lines 1-")
            .unwrap()
            .split('/')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(delivered_end < 80);

        let last_numbered = output
            .lines()
            .filter_map(|line| line.split_once(": "))
            .filter_map(|(number, _)| number.trim().parse::<usize>().ok())
            .next_back()
            .expect("numbered body line");
        assert_eq!(last_numbered, delivered_end);
        assert!(output.contains("output truncated at 800 bytes"), "{output}");
        assert!(
            output.contains(&format!("continue with start_line={}", delivered_end + 1)),
            "{output}"
        );
        assert_eq!(result.compensation_events.len(), 1);

        // Resume at the reported line: the next read starts exactly there, so
        // consecutive ranges lose no line and repeat none.
        let resume = delivered_end + 1;
        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "start_line": resume
        })
        .to_string();
        let next = execute_read(&context, &args).unwrap();
        assert!(
            next.output
                .contains(&format!("{resume:>6}: line {resume:03} ")),
            "{}",
            next.output
        );
    }

    #[test]
    fn byte_cap_too_small_for_first_line_is_clear() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("giant-line.txt");
        fs::write(&file_path, "x".repeat(5000)).unwrap();

        let mut context = ToolContext::from_current_process();
        let mut limits = crate::config::settings::ToolLimits::defaults();
        limits.read_max_output_bytes = Some(1000);
        context.limits = limits;

        let args = serde_json::json!({ "path": file_path.to_str().unwrap() }).to_string();
        let result = execute_read(&context, &args).unwrap();
        assert!(
            result.output.contains("output truncated at 1000 bytes"),
            "{}",
            result.output
        );
        assert!(result.output.contains("line 1 needs"), "{}", result.output);
        assert!(!result.output.contains("xxxxx"), "{}", result.output);
        assert_eq!(result.compensation_events.len(), 1);
    }

    #[test]
    fn byte_cap_too_small_for_header_and_first_line_is_clear() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("one-line.txt");
        fs::write(&file_path, "z".repeat(200)).unwrap();

        let mut context = ToolContext::from_current_process();
        let mut limits = crate::config::settings::ToolLimits::defaults();
        limits.read_max_output_bytes = Some(220);
        context.limits = limits;

        let args = serde_json::json!({ "path": file_path.to_str().unwrap() }).to_string();
        let result = execute_read(&context, &args).unwrap();
        assert!(
            result
                .output
                .contains("plus the Read header and footer needs"),
            "{}",
            result.output
        );
        assert_eq!(result.compensation_events.len(), 1);
    }

    #[test]
    fn zero_line_numbers_are_rejected() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("three.txt");
        fs::write(&file_path, "a\nb\nc").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "start_line": 0
        })
        .to_string();
        let err = execute_read(&ToolContext::from_current_process(), &args).unwrap_err();
        assert!(err.contains("start_line must be 1 or greater"), "{err}");
        assert!(err.contains("3 lines"), "{err}");

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "end_line": 0
        })
        .to_string();
        let err = execute_read(&ToolContext::from_current_process(), &args).unwrap_err();
        assert!(err.contains("end_line must be 1 or greater"), "{err}");
        assert!(err.contains("3 lines"), "{err}");
    }

    #[test]
    fn empty_file_is_reported_without_a_range_error() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("empty.txt");
        fs::write(&file_path, "").unwrap();

        let args = serde_json::json!({ "path": file_path.to_str().unwrap() }).to_string();
        let result = execute_read(&ToolContext::from_current_process(), &args).unwrap();
        assert!(result.output.contains("0 lines total"), "{}", result.output);
        assert!(
            result.output.contains("(file is empty)"),
            "{}",
            result.output
        );
    }

    #[test]
    fn read_max_output_bytes_unlimited_never_truncates() {
        // 120,000 bytes exceeds the compiled 100,000-byte cap, but an
        // unlimited output budget passes it through without the output
        // truncation marker. The per-line cap still bounds the single long
        // line itself.
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("big.txt");
        fs::write(&file_path, "x".repeat(120_000)).unwrap();

        let mut context = ToolContext::from_current_process();
        let mut limits = crate::config::settings::ToolLimits::defaults();
        limits.read_max_output_bytes = None;
        context.limits = limits;

        let args = serde_json::json!({ "path": file_path.to_str().unwrap() }).to_string();
        let result = execute_read(&context, &args).unwrap();
        assert!(!result.output.contains("output truncated"));
        assert!(result.output.contains("line truncated to 10000 bytes"));
        assert!(
            !result
                .compensation_events
                .iter()
                .any(|e| e.kind == crate::session_telemetry::CompensationKind::OutputTruncation),
            "unlimited budget must not record a truncation event"
        );
    }

    #[test]
    fn read_max_line_bytes_override_changes_marker() {
        // A custom per-line cap both bounds the delivered line and is reported
        // by the truncation marker, overriding the compiled 10,000 default.
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("long.txt");
        fs::write(&file_path, "x".repeat(500)).unwrap();

        let mut context = ToolContext::from_current_process();
        let mut limits = crate::config::settings::ToolLimits::defaults();
        limits.read_max_line_bytes = Some(100);
        context.limits = limits;

        let args = serde_json::json!({ "path": file_path.to_str().unwrap() }).to_string();
        let result = execute_read(&context, &args).unwrap();
        assert!(
            result.output.contains("... (line truncated to 100 bytes)"),
            "marker must report the configured cap: {}",
            result.output
        );
    }

    #[test]
    fn read_max_line_bytes_unlimited_reads_whole_line() {
        // An unlimited per-line cap re-enables reading a line whole, so a
        // long line is delivered in full without a truncation marker.
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("long.txt");
        fs::write(&file_path, "y".repeat(50_000)).unwrap();

        let mut context = ToolContext::from_current_process();
        let mut limits = crate::config::settings::ToolLimits::defaults();
        limits.read_max_line_bytes = None;
        context.limits = limits;

        let args = serde_json::json!({ "path": file_path.to_str().unwrap() }).to_string();
        let result = execute_read(&context, &args).unwrap();
        assert!(!result.output.contains("line truncated"));
        assert!(result.output.contains(&"y".repeat(50_000)));
    }

    // --- per-line byte cap ---

    #[test]
    fn newline_free_giant_line_bounds_allocation() {
        // A single newline-free line much larger than the cap must not be
        // read into memory whole: the raw line buffer stays within a small
        // multiple of the cap and the delivered text is a truncated prefix.
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("minified.json");
        fs::write(&file_path, "x".repeat(50 * 1024 * 1024)).unwrap();

        let file = std::fs::File::open(&file_path).unwrap();
        let mut reader = BufReader::new(file);
        let mut buf = Vec::new();
        let cap = DEFAULT_READ_MAX_LINE_BYTES as usize;
        let line = read_line_bounded(&mut reader, &mut buf, cap)
            .unwrap()
            .expect("file has one line");
        assert!(line.truncated, "50 MB line must count as truncated");
        assert!(
            line.text.len() <= cap,
            "delivered {} bytes but cap is {cap}",
            line.text.len()
        );
        assert!(
            buf.capacity() < 4 * cap,
            "line buffer capacity {} exceeds a small multiple of the cap",
            buf.capacity()
        );

        // The line still counts as exactly one line: EOF follows immediately.
        assert!(
            read_line_bounded(&mut reader, &mut buf, cap)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn read_truncates_newline_free_giant_line() {
        // The delivered line is truncated with a marker, the numbering is
        // unchanged, and the file still totals exactly one line.
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("minified.json");
        fs::write(&file_path, "y".repeat(20_000)).unwrap();

        let args = serde_json::json!({ "path": file_path.to_str().unwrap() }).to_string();
        let result = execute_read(&ToolContext::from_current_process(), &args).unwrap();
        let output = &result.output;
        assert!(output.contains("Lines 1-1/1"));
        assert!(output.contains("     1: "));
        assert!(
            output.contains("... (line truncated to 10000 bytes)"),
            "marker missing from: {output}"
        );
    }

    #[test]
    fn line_of_exactly_cap_bytes_gets_no_marker() {
        // A line of exactly the cap is delivered in full without a marker.
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("exact.txt");
        fs::write(&file_path, "z".repeat(DEFAULT_READ_MAX_LINE_BYTES as usize)).unwrap();

        let args = serde_json::json!({ "path": file_path.to_str().unwrap() }).to_string();
        let result = execute_read(&ToolContext::from_current_process(), &args).unwrap();
        let output = &result.output;
        assert!(output.contains("Lines 1-1/1"));
        assert!(
            !output.contains("line truncated"),
            "exact-cap line must not be marked: {output}"
        );
        let full = "z".repeat(DEFAULT_READ_MAX_LINE_BYTES as usize);
        assert!(output.contains(full.as_str()));
    }

    #[test]
    fn read_truncates_multibyte_line_at_char_boundary() {
        // A line of '€' (3 bytes each) exceeding the per-line cap is
        // truncated at a char boundary with no replacement character.
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("emoji.txt");
        // 4_000 € is 12_000 bytes, over the 10_000-byte cap.
        fs::write(&file_path, "\u{20AC}".repeat(4_000)).unwrap();

        let args = serde_json::json!({ "path": file_path.to_str().unwrap() }).to_string();
        let result = execute_read(&ToolContext::from_current_process(), &args).unwrap();
        let output = &result.output;
        assert!(output.contains("Lines 1-1/1"));
        assert!(output.contains("... (line truncated to 10000 bytes)"));
        assert!(
            !output.contains('\u{FFFD}'),
            "Output contains replacement character (split code point)"
        );
    }
}
