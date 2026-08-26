use serde::Deserialize;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read as _};
use std::path::Path;

use crate::clients::tools::{ToolContext, validate_path_in_cwd};
use crate::config::settings::ToolLimits;
use crate::session_telemetry::{CompensationEventTelemetry, CompensationKind};

#[cfg(test)]
use crate::config::settings::DEFAULT_READ_MAX_OUTPUT_BYTES;

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
                    "description": "Last line to read (1-indexed, inclusive). Default: the configured read_default_end_line window (200 out of the box; start_line+window-1 when start_line given)"
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
    read_file(&path, args.start_line, args.end_line, &context.limits)
}

/// Check the first 8KB of a file for null bytes (binary detection)
fn is_binary(path: &Path) -> Result<bool, String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("Failed to open file '{}': {e}", path.display()))?;
    probe_binary(file).map_err(|e| format!("Failed to read file '{}': {e}", path.display()))
}

/// Probe the first 8 KiB of a reader for null bytes.
///
/// Uses `read_to_end` on a `Take` to handle readers that return fewer bytes
/// than requested on the first call (short reads from FIFOs, network mounts,
/// or FUSE filesystems).
fn probe_binary(reader: impl std::io::Read) -> Result<bool, std::io::Error> {
    let mut buf = Vec::with_capacity(8192);
    let n = reader.take(8192).read_to_end(&mut buf)?;
    Ok(buf[..n].contains(&0))
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

/// Read and format a file with line numbers
fn read_file(
    path: &Path,
    start_line: Option<usize>,
    end_line: Option<usize>,
    limits: &ToolLimits,
) -> Result<super::ToolResult, String> {
    // Check for binary files (null bytes in first 8KB)
    if is_binary(path)? {
        return Err(format!(
            "Cannot read binary file: {} (detected null bytes)",
            path.display()
        ));
    }

    // Default line range (1-indexed from caller, convert to 0-indexed).
    let start = start_line.unwrap_or(1).saturating_sub(1);
    let end_requested =
        end_requested_line(start, start_line, end_line, limits.read_default_end_line);

    let file = std::fs::File::open(path)
        .map_err(|e| format!("Failed to read file '{}': {e}", path.display()))?;
    let mut reader = BufReader::new(file);
    read_from_reader(&mut reader, start, end_requested, path, limits)
}

/// Read a line range from an open reader and format it with line numbers.
///
/// The work is bounded rather than proportional to the file:
/// - In-window lines accumulate into a single `String` only while it stays
///   under `read_max_output_bytes`; the byte cap trims the exact return.
/// - After the last emitted line, at most [`EXTRA_LINES_TO_COUNT`] more lines
///   are read to decide the footer. EOF within that budget yields the exact
///   total; otherwise the header and footer use "at least N" phrasing, which
///   never understates the file length.
fn read_from_reader<R: BufRead>(
    reader: &mut R,
    start: usize,
    end_requested: usize,
    path: &Path,
    limits: &ToolLimits,
) -> Result<super::ToolResult, String> {
    // The window is empty by request: nothing can be shown, so count to EOF
    // for the exact total in the message.
    if start > end_requested {
        let total_lines = count_lines_to_eof(reader, path)?;
        return Ok(no_content_result(path, total_lines));
    }

    // An unlimited budget accumulates the whole file by design.
    let soft_cap = limits.read_max_output_bytes.unwrap_or(usize::MAX);

    let mut body = String::new();
    let mut total_lines: usize = 0;
    let mut last_emitted: Option<usize> = None;
    let mut exact = false;
    let mut line_buf = String::new();

    loop {
        line_buf.clear();
        let n = reader
            .read_line(&mut line_buf)
            .map_err(|e| format!("Failed to read file '{}': {e}", path.display()))?;
        if n == 0 {
            exact = true;
            break; // EOF
        }

        let i = total_lines;
        total_lines += 1;

        if i < start {
            continue; // line_buf reused, no allocation
        }
        if emit_line(i, end_requested, &body, soft_cap) {
            append_line(&mut body, &line_buf, i);
            last_emitted = Some(i);
        }

        // Stop once the window is satisfied and a bounded number of lines
        // past the last emitted line has been counted for the footer.
        if past_counting_budget(last_emitted, i) {
            break;
        }
    }

    let Some(end) = last_emitted else {
        // `start` lay beyond EOF; only EOF stops the loop without an
        // emitted line, so the count is exact.
        return Ok(no_content_result(path, total_lines));
    };

    let mut output = format!(
        "File: {}\nLines {}-{}/{}\n{}",
        path.display(),
        start + 1,
        end + 1,
        total_label(exact, total_lines),
        body
    );

    let mut compensation_events = Vec::new();
    if let Some(event) = apply_output_cap(&mut output, limits.read_max_output_bytes) {
        compensation_events.push(event);
    }

    // Note remaining lines if applicable. The count is a lower bound when
    // the read stopped at the counting budget, so say "at least".
    append_more_lines(&mut output, end, total_lines, exact);

    Ok(super::ToolResult {
        output,
        compensation_events,
    })
}

/// Whether line `i` belongs in the output. The first in-window line is always
/// emitted; later lines stop once the running body reaches the byte budget.
const fn emit_line(i: usize, end_requested: usize, body: &str, soft_cap: usize) -> bool {
    i <= end_requested && (body.is_empty() || body.len() < soft_cap)
}

/// Append one numbered line to the body.
fn append_line(body: &mut String, line_buf: &str, i: usize) {
    if !body.is_empty() {
        body.push('\n');
    }
    let trimmed = line_buf.trim_end_matches('\n').trim_end_matches('\r');
    _ = write!(body, "{:>6}: {trimmed}", i + 1);
}

/// Whether the read should stop: the window is satisfied and
/// [`EXTRA_LINES_TO_COUNT`] lines have been counted past the last emitted
/// line.
fn past_counting_budget(last_emitted: Option<usize>, i: usize) -> bool {
    last_emitted.is_some_and(|end| i >= end.saturating_add(EXTRA_LINES_TO_COUNT))
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

/// Truncate `output` to the byte cap at a UTF-8 boundary, returning the
/// telemetry event when truncation happened. An unlimited budget
/// (`read_max_output_bytes = "unlimited"`) never truncates.
fn apply_output_cap(
    output: &mut String,
    max_bytes: Option<usize>,
) -> Option<CompensationEventTelemetry> {
    if let Some(max_bytes) = max_bytes
        && output.len() > max_bytes
    {
        let reserve = 100; // bytes for the truncation marker
        let byte_end = output.floor_char_boundary(max_bytes.saturating_sub(reserve));
        let mut truncated = {
            #[expect(
                clippy::string_slice,
                reason = "floor_char_boundary guarantees a char boundary"
            )]
            output[..byte_end].to_string()
        };
        _ = write!(
            truncated,
            "\n[... output truncated at {max_bytes} bytes ...]"
        );
        *output = truncated;
        Some(CompensationEventTelemetry::new(
            CompensationKind::OutputTruncation,
            Some("Read".to_string()),
        ))
    } else {
        None
    }
}

/// Append the remaining-lines footer, using "at least" when the count is a
/// lower bound.
fn append_more_lines(output: &mut String, end: usize, total_lines: usize, exact: bool) {
    if end < total_lines.saturating_sub(1) {
        let remaining = total_lines.saturating_sub(end + 1);
        if exact {
            _ = write!(output, "\n[... {remaining} more lines ...]");
        } else {
            _ = write!(output, "\n[... at least {remaining} more lines ...]");
        }
    }
}

/// Count every line of `reader` to EOF, reusing one buffer (no per-line
/// allocation). Used when the window is empty by request and the message
/// needs the exact total.
fn count_lines_to_eof<R: BufRead>(reader: &mut R, path: &Path) -> Result<usize, String> {
    let mut line_buf = String::new();
    let mut total_lines = 0usize;
    loop {
        line_buf.clear();
        let n = reader
            .read_line(&mut line_buf)
            .map_err(|e| format!("Failed to read file '{}': {e}", path.display()))?;
        if n == 0 {
            break;
        }
        total_lines += 1;
    }
    Ok(total_lines)
}

/// The "no content to show" result, reporting the exact total.
fn no_content_result(path: &Path, total_lines: usize) -> super::ToolResult {
    super::ToolResult {
        output: format!(
            "File: {}\n{total_lines} lines total\n(start_line > end_line, no content to show)",
            path.display()
        ),
        compensation_events: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn start_line_after_end_line_reports_exact_total() {
        // An empty requested window (start_line > end_line) counts to EOF so
        // the message can report the exact total without understating it.
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "a\nb\nc\nd\ne").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "start_line": 4,
            "end_line": 2
        })
        .to_string();

        let result = execute_read(&ToolContext::from_current_process(), &args).unwrap();
        assert!(result.output.contains("5 lines total"));
        assert!(result.output.contains("no content to show"));
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
        assert!(result.output.contains("[... at least 1000 more lines ...]"));
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
        // (100_000 bytes). Using '€' (3 bytes, U+20AC) repeated on one long
        // line. Header: ~60 bytes. Line prefix "     1: ": 8 bytes.
        // Each € = 3 bytes.  35_000 € → 105_000 bytes, comfortably over the cap.
        let emoji_count = 35_000;
        let mut line = String::with_capacity(emoji_count * 3);
        for _ in 0..emoji_count {
            line.push('\u{20AC}');
        }
        // File has one line with 35K € chars
        fs::write(&file_path, &line).unwrap();

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
    fn probe_binary_detects_null_byte_across_short_reads() {
        // A reader that returns data in 100-byte chunks with a null byte
        // at offset 512 (past the first few chunks).
        let mut data = vec![b'x'; 8192];
        data[512] = 0x00;
        let reader = ShortReader::new(&data, 100);
        assert!(
            probe_binary(reader).unwrap(),
            "should detect null byte at offset 512 across short-read boundary"
        );
    }

    #[test]
    fn probe_binary_short_read_no_null() {
        // A reader returning data in small chunks without null bytes.
        let data = vec![b'x'; 8192];
        let reader = ShortReader::new(&data, 100);
        assert!(
            !probe_binary(reader).unwrap(),
            "should not detect binary without null bytes"
        );
    }

    #[test]
    fn probe_binary_short_read_short_file_no_null() {
        // A file shorter than the probe window, returned in tiny chunks.
        let data = vec![b'a'; 42];
        let reader = ShortReader::new(&data, 7);
        assert!(
            !probe_binary(reader).unwrap(),
            "short file without null bytes should not be detected as binary"
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
        fs::write(&file_path, "x".repeat(5000)).unwrap();

        let mut context = ToolContext::from_current_process();
        let mut limits = crate::config::settings::ToolLimits::defaults();
        limits.read_max_output_bytes = Some(1000);
        context.limits = limits;

        let args = serde_json::json!({ "path": file_path.to_str().unwrap() }).to_string();
        let result = execute_read(&context, &args).unwrap();
        assert!(
            result
                .output
                .contains("[... output truncated at 1000 bytes ...]")
        );
        assert!(result.output.len() < 5000);
        assert!(
            result
                .compensation_events
                .iter()
                .any(|e| e.kind == crate::session_telemetry::CompensationKind::OutputTruncation),
            "truncation must record an output_truncation event"
        );
    }

    #[test]
    fn read_max_output_bytes_unlimited_never_truncates() {
        // 120,000 bytes exceeds the compiled 100,000-byte cap, but an
        // unlimited budget passes it through untouched.
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
        assert!(
            !result
                .compensation_events
                .iter()
                .any(|e| e.kind == crate::session_telemetry::CompensationKind::OutputTruncation),
            "unlimited budget must not record a truncation event"
        );
    }
}
