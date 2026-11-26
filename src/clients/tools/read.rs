use serde::Deserialize;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use crate::clients::tools::{
    ToolContext, contains_null_byte, decode_utf8, file_access::validate_regular_file,
    read_tracker::ReadObservation, resolve_path_for_read_scheduling, validate_path_in_cwd,
};
use crate::config::settings::ToolLimits;
use crate::session_telemetry::{CompensationEventTelemetry, CompensationKind};

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
                    "description": "Last line to read (1-indexed, inclusive). Defaults to the configured window from `start_line` (500 lines by default)."
                }
            },
            "required": ["path"]
        }),
    }
}

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

    let path = validate_path_in_cwd(context, &args.path)?;

    validate_regular_file(&path)?;

    let outcome = read_file(&path, args.start_line, args.end_line, &context.limits)
        .map_err(|error| append_image_hint(error, &path))?;
    context.record_read(&path, outcome.observation);
    Ok(outcome.result)
}

/// A successful Read result plus the observation Edit uses for diagnostics.
struct ReadOutcome {
    result: super::ToolResult,
    observation: ReadObservation,
}

impl ReadOutcome {
    const fn new(result: super::ToolResult, observation: ReadObservation) -> Self {
        Self {
            result,
            observation,
        }
    }
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
        (None, Some(window)) if start_line.is_some() => start.saturating_add(window - 1),
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

/// Read and format a file with range metadata.
fn read_file(
    path: &Path,
    start_line: Option<usize>,
    end_line: Option<usize>,
    limits: &ToolLimits,
) -> Result<ReadOutcome, String> {
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

    let start = start_value - 1;
    let end_requested =
        end_requested_line(start, start_line, end_line, limits.read_default_end_line);
    read_from_reader(&mut reader, start, end_requested, path, limits)
}

/// Read a line range from an open reader and format it with range metadata.
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
) -> Result<ReadOutcome, String> {
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
            break;
        };

        reject_binary_line(&line, path)?;

        scanned_bytes = scanned_bytes.saturating_add(line.bytes_consumed);
        let i = total_lines;
        total_lines += 1;

        if i < start {
            continue;
        }

        if let Some(attempted_len) = builder.push(i, end_requested, &line) {
            first_line_overflow = Some(attempted_len);
            break;
        }

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
            total_lines,
            false,
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
        truncated_lines: builder.truncated_lines,
        max_bytes,
        cap_stopped: builder.cap_stopped,
    }))
}

/// No line at `start` exists. An empty file is a normal result; a non-empty
/// file means the requested start line is past EOF.
fn no_line_result(path: &Path, start: usize, total_lines: usize) -> Result<ReadOutcome, String> {
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
    /// 0-indexed delivered lines cut at the per-line byte cap.
    truncated_lines: Vec<usize>,
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
            truncated_lines: Vec::new(),
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
        append_line(&mut self.body, line, self.line_cap);
        if self.body.len() <= self.body_budget {
            if line.truncated {
                self.truncated_lines.push(i);
            }
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

/// Append one unnumbered line to the body, marking a line truncated at the
/// per-line byte cap.
fn append_line(body: &mut String, line: &BoundedLine<'_>, cap: usize) {
    if !body.is_empty() {
        body.push('\n');
    }
    body.push_str(line.text);
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
    let (terminated, contains_null_byte, bytes_consumed) =
        scan_line(reader, buf, cap.saturating_add(2))?;
    if !terminated && buf.is_empty() {
        return Ok(None);
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
            return Ok((false, has_null_byte, bytes_consumed));
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
    /// 0-indexed delivered lines cut at the per-line byte cap.
    truncated_lines: Vec<usize>,
    max_bytes: Option<usize>,
    cap_stopped: bool,
}

/// Build the final result from the delivered body, dropping whole lines until
/// the header, body, and footer fit the configured output budget.
fn build_read_result(input: ReadResultInput<'_>) -> ReadOutcome {
    let ReadResultInput {
        path,
        start,
        mut end,
        total_lines,
        exact,
        mut body,
        line_ends,
        mut truncated_lines,
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
            truncated_lines.retain(|line| *line <= end);
            return ReadOutcome::new(
                super::ToolResult {
                    output,
                    images: Vec::new(),
                    compensation_events,
                    permission_denials: Vec::new(),
                },
                ReadObservation {
                    start_line: start + 1,
                    end_line: Some(end + 1),
                    total_lines,
                    total_exact: exact,
                    output_truncated: cap_stopped,
                    truncated_lines: truncated_lines.into_iter().map(|line| line + 1).collect(),
                },
            );
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
                total_lines,
                exact,
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
    total_lines: usize,
    total_exact: bool,
) -> ReadOutcome {
    let needed = needed.map_or(String::new(), |needed| {
        format!("; the first line plus the Read header and footer needs {needed} bytes")
    });
    ReadOutcome::new(
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
        },
        ReadObservation {
            start_line: line,
            end_line: None,
            total_lines,
            total_exact,
            output_truncated: true,
            truncated_lines: Vec::new(),
        },
    )
}

/// An empty file has no range to report and no truncation.
fn empty_file_result(path: &Path) -> ReadOutcome {
    ReadOutcome::new(
        super::ToolResult {
            output: format!("File: {}\n0 lines total\n(file is empty)", path.display()),
            images: Vec::new(),
            compensation_events: Vec::new(),
            permission_denials: Vec::new(),
        },
        ReadObservation {
            start_line: 1,
            end_line: None,
            total_lines: 0,
            total_exact: true,
            output_truncated: false,
            truncated_lines: Vec::new(),
        },
    )
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
