use serde::Deserialize;
use std::fmt::Write as _;
use std::path::Path;

use crate::clients::tools::{ToolContext, validate_path_for_write};

// =============================================================================
// Constants
// =============================================================================

/// Maximum number of edits allowed in a single call
const MAX_EDITS_PER_CALL: usize = 10;
/// Maximum per-edit status lines included in a failed multi-edit preflight.
const MAX_PREFLIGHT_STATUS_LINES: usize = 6;

// =============================================================================
// Edit Tool Definition
// =============================================================================

/// Returns the Edit tool definition
pub(super) fn edit_tool() -> super::Tool {
    super::Tool {
        type_: "function".to_string(),
        name: "Edit".to_string(),
        description: "Edit text in files using literal search-and-replace. The set of edits is atomic: all edits in a single call succeed together, or none are applied.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The path of the file to edit."
                },
                "edits": {
                    "type": "array",
                    "description": "The edits to make to the file.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_text": {
                                "type": "string",
                                "description": "Text to search for - must match exactly. The old_text must uniquely identify the location - include enough surrounding context (e.g., 3+ lines or function/class names) to ensure only ONE match exists in the file. Special characters require JSON escaping: backticks (`\\``...\\``), quotes, backslashes. For multi-line content, include exact newlines and indentation."
                            },
                            "new_text": {
                                "type": "string",
                                "description": "Text to replace with"
                            }
                        },
                        "required": ["old_text", "new_text"]
                    }
                }
            },
            "required": ["path", "edits"]
        }),
    }
}

// =============================================================================
// Types
// =============================================================================

/// A single edit operation
#[derive(Debug, Clone, Deserialize)]
struct Edit {
    old_text: String,
    new_text: String,
}

/// Arguments for the Edit tool
#[derive(Debug, Deserialize)]
struct EditArgs {
    path: String,
    edits: Vec<Edit>,
}

/// Path-only arguments for edit summaries.
#[derive(Debug, Deserialize)]
struct EditSummaryArgs {
    path: String,
}

/// Summarize edit arguments for display
pub fn summarize_args(arguments: &str) -> String {
    serde_json::from_str::<EditSummaryArgs>(arguments)
        .map(|args| args.path)
        .unwrap_or_default()
}

/// A matched edit with position information
#[derive(Debug, Clone)]
struct MatchedEdit {
    /// The replacement text
    new_text: String,
    /// Position in original content where the match starts (byte offset)
    index: usize,
    /// Length of the matched original text
    match_length: usize,
    /// Original index in the edits array (1-based for error messages)
    edit_index: usize,
}

/// Normalized content with byte offsets back to the original string.
#[derive(Debug)]
struct NormalizedContent {
    /// Content with CRLF line endings represented as LF.
    content: String,
    /// Original byte offset for each normalized byte offset.
    original_offsets: Vec<usize>,
    /// Original content length in bytes.
    original_len: usize,
}

/// Line ending type for preservation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineEnding {
    Lf,   // \n (Unix)
    Crlf, // \r\n (Windows)
}

// =============================================================================
// Edit Execution
// =============================================================================

/// Execute an edit command
pub(super) fn execute_edit(
    tool_context: &ToolContext,
    arguments: &str,
) -> Result<super::ToolResult, String> {
    let args: EditArgs =
        serde_json::from_str(arguments).map_err(|e| format!("Invalid edit arguments: {e}"))?;

    // Validate number of edits
    if args.edits.is_empty() {
        return Err("No edits provided. At least one edit is required.".to_string());
    }

    if args.edits.len() > MAX_EDITS_PER_CALL {
        return Err(format!(
            "Too many edits ({}). Maximum {} edits per call. Please split your changes into multiple tool calls.",
            args.edits.len(),
            MAX_EDITS_PER_CALL
        ));
    }

    // Check for no-op edits
    for (i, edit) in args.edits.iter().enumerate() {
        if edit.old_text == edit.new_text {
            return Err(format!(
                "Edit {}: old_text and new_text are identical — no changes needed",
                i + 1
            ));
        }
    }

    // Validate and canonicalize the path (ensures it's not read-only)
    let path = validate_path_for_write(tool_context, &args.path)?;

    // Check if file exists and is a file
    let metadata = std::fs::metadata(&path)
        .map_err(|e| format!("Failed to access file '{}': {e}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("Path is not a file: {}", path.display()));
    }

    // Refuse binary files before treating the bytes as editable UTF-8 text.
    let file_bytes = std::fs::read(&path)
        .map_err(|e| format!("Failed to read file '{}': {e}", path.display()))?;
    if file_bytes.contains(&0) {
        return Err(format!(
            "Cannot edit binary file: {} (detected null bytes)",
            path.display()
        ));
    }

    // Read file content as string
    let content = String::from_utf8(file_bytes)
        .map_err(|_e| format!("File contains invalid UTF-8: {}", path.display()))?;

    // Detect and strip BOM
    let (bom, content) = strip_bom(&content);

    // Detect line ending style
    let line_ending = detect_line_ending(content);

    // Normalize CRLF to LF for matching while keeping original byte ranges.
    let normalized_content = normalize_crlf_line_endings(content);

    // Preflight validation: find all match positions
    let matched_edits = preflight_edits(&args.edits, &normalized_content, line_ending, &path)?;

    // Apply edits in reverse order (highest position first)
    let new_content = apply_edits_reverse_order(content, &matched_edits);

    // Restore BOM if it was present
    let new_content = restore_bom(new_content, bom);

    // Write the modified content back
    std::fs::write(&path, &new_content)
        .map_err(|e| format!("Failed to write file '{}': {e}", path.display()))?;

    // Generate diff output
    let diff = generate_unified_diff(&restore_bom(content.to_string(), bom), &new_content, &path);

    let result = format!(
        "Applied {} edit{} to: {}\n{}",
        matched_edits.len(),
        if matched_edits.len() == 1 { "" } else { "s" },
        path.display(),
        diff
    );

    Ok(super::ToolResult { output: result })
}

// =============================================================================
// Preflight Validation
// =============================================================================

/// Validate all edits and find their match positions
fn preflight_edits(
    edits: &[Edit],
    normalized_content: &NormalizedContent,
    line_ending: LineEnding,
    path: &Path,
) -> Result<Vec<MatchedEdit>, String> {
    let total = edits.len();
    let mut matched_edits = Vec::with_capacity(total);

    for (i, edit) in edits.iter().enumerate() {
        let edit_index = i + 1; // 1-based for error messages

        // Count occurrences
        let occurrences: Vec<usize> = normalized_content
            .content
            .match_indices(&edit.old_text)
            .map(|(idx, _)| idx)
            .collect();

        if occurrences.is_empty() {
            let preflight_summary = preflight_failure_summary(
                total,
                &matched_edits,
                edit_index,
                "failed: could not find the exact text to replace",
            );
            let mut message = format!(
                "Edit {} of {} failed in {}: could not find the exact text to replace. The old_text must match exactly, including all whitespace and newlines.",
                edit_index,
                total,
                path.display(),
            );
            if let Some(hint) = nearest_match_hint(&normalized_content.content, &edit.old_text) {
                _ = write!(message, "\nNearest matching context in file:\n{hint}");
            }
            _ = write!(
                message,
                "{}\n{}\nRetry by re-reading the target range and providing a narrower old_text, or split the request into smaller edits.",
                preflight_summary,
                atomic_rollback_notice(total),
            );
            return Err(message);
        }

        if occurrences.len() > 1 {
            let contexts = ambiguous_match_contexts(&normalized_content.content, &occurrences);
            let failure_status =
                format!("failed: old_text matched {} locations", occurrences.len());
            let preflight_summary =
                preflight_failure_summary(total, &matched_edits, edit_index, &failure_status);
            return Err(format!(
                "Edit {} of {} failed in {}: old_text matches {} locations but must match exactly 1.\nCandidate match contexts:\n{}{}\n{}\nProvide a more specific old_text that includes more surrounding context.",
                edit_index,
                total,
                path.display(),
                occurrences.len(),
                contexts,
                preflight_summary,
                atomic_rollback_notice(total),
            ));
        }

        let (index, match_length) =
            normalized_content.original_range(occurrences[0], edit.old_text.len())?;

        matched_edits.push(MatchedEdit {
            new_text: normalize_replacement_line_endings(&edit.new_text, line_ending),
            index,
            match_length,
            edit_index,
        });
    }

    // Sort by position for overlap detection
    matched_edits.sort_by_key(|e| e.index);

    // Check for overlapping edits
    for window in matched_edits.windows(2) {
        let first = &window[0];
        let second = &window[1];
        let first_end = first.index + first.match_length;

        if first_end > second.index {
            let failure_status = format!(
                "failed: overlaps with edit {} after both matched uniquely",
                first.edit_index
            );
            let preflight_summary = preflight_failure_summary(
                total,
                &matched_edits,
                second.edit_index,
                &failure_status,
            );
            return Err(format!(
                "Edits {} and {} (of {}) overlap in {}; each edit must target a distinct region.{}\n{}\nCombine overlapping edits into a single edit.",
                first.edit_index,
                second.edit_index,
                total,
                path.display(),
                preflight_summary,
                atomic_rollback_notice(total),
            ));
        }
    }

    Ok(matched_edits)
}

/// Render a compact per-edit status block for failed multi-edit preflights.
fn preflight_failure_summary(
    total: usize,
    matched_edits: &[MatchedEdit],
    failed_edit_index: usize,
    failed_status: &str,
) -> String {
    if total == 1 {
        return String::new();
    }

    let mut matched_indexes: Vec<usize> = matched_edits
        .iter()
        .map(|matched| matched.edit_index)
        .collect();
    matched_indexes.sort_unstable();

    let reserved_status_lines = 1 + usize::from(failed_edit_index < total);
    let available_matched_lines = MAX_PREFLIGHT_STATUS_LINES.saturating_sub(reserved_status_lines);
    let mut out = String::from("\nPreflight summary:");

    let shown_matched = if matched_indexes.len() > available_matched_lines {
        available_matched_lines.saturating_sub(1)
    } else {
        matched_indexes.len()
    };
    for edit_index in matched_indexes.iter().take(shown_matched) {
        _ = write!(
            out,
            "\n- Edit {edit_index} of {total}: matched exactly once."
        );
    }

    if matched_indexes.len() > shown_matched {
        _ = write!(
            out,
            "\n- ... {} matched edit{} omitted.",
            matched_indexes.len() - shown_matched,
            plural_suffix(matched_indexes.len() - shown_matched)
        );
    }

    _ = write!(
        out,
        "\n- Edit {failed_edit_index} of {total}: {failed_status}."
    );

    if failed_edit_index < total {
        let skipped = total - failed_edit_index;
        let skipped_status = format!(
            "\n- Edit{} {} of {total}: not evaluated after the failure.",
            if skipped == 1 { "" } else { "s" },
            if skipped == 1 {
                (failed_edit_index + 1).to_string()
            } else {
                format!("{}-{}", failed_edit_index + 1, total)
            }
        );
        out.push_str(&skipped_status);
    }

    out
}

const fn plural_suffix(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// Sentence describing the atomic, all-or-nothing semantics so callers know
/// whether earlier edits were applied or rolled back when a later edit fails.
const fn atomic_rollback_notice(total: usize) -> &'static str {
    if total > 1 {
        "No edits were applied; the file is unchanged. Edit is atomic: all edits in a single call succeed together, or none are applied."
    } else {
        "No edits were applied; the file is unchanged."
    }
}

/// Render compact, capped snippets for ambiguous matches so callers can make
/// the next `old_text` more specific without re-reading the whole file.
fn ambiguous_match_contexts(content: &str, occurrences: &[usize]) -> String {
    const CONTEXT_RADIUS: usize = 1;
    const MAX_CANDIDATES: usize = 5;
    const MAX_LINE_LEN: usize = 160;

    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return "  (no line context available)".to_string();
    }

    let mut out = String::new();
    for (candidate_index, occurrence) in occurrences.iter().take(MAX_CANDIDATES).enumerate() {
        let line_index = line_index_for_offset(content, *occurrence).min(lines.len() - 1);
        let start = line_index.saturating_sub(CONTEXT_RADIUS);
        let end = (line_index + CONTEXT_RADIUS + 1).min(lines.len());

        if candidate_index > 0 {
            out.push('\n');
        }
        _ = writeln!(
            out,
            "Match {} at line {}:",
            candidate_index + 1,
            line_index + 1
        );

        for (offset, line) in lines[start..end].iter().enumerate() {
            let current_index = start + offset;
            let marker = if current_index == line_index {
                ">"
            } else {
                " "
            };
            let truncated = truncate_to_chars(line, MAX_LINE_LEN);
            _ = writeln!(out, "{marker} {:>5} | {truncated}", current_index + 1);
        }
    }

    if occurrences.len() > MAX_CANDIDATES {
        _ = writeln!(
            out,
            "\nShowing first {MAX_CANDIDATES} of {} matches.",
            occurrences.len()
        );
    }

    out.trim_end_matches('\n').to_string()
}

fn line_index_for_offset(content: &str, offset: usize) -> usize {
    content
        .as_bytes()
        .iter()
        .take(offset)
        .filter(|byte| **byte == b'\n')
        .count()
}

/// Best-effort: pick the line in `content` whose token overlap with the first
/// non-empty line of `needle` is highest. Returns the surrounding lines as a
/// compact hint. Returns `None` if no useful match is found.
fn nearest_match_hint(content: &str, needle: &str) -> Option<String> {
    const HINT_RADIUS: usize = 1;
    const MAX_LINE_LEN: usize = 200;

    let needle_line = needle.lines().find(|line| !line.trim().is_empty())?;
    let needle_trimmed = needle_line.trim();
    if needle_trimmed.len() < 4 {
        return None;
    }

    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return None;
    }

    let mut best_idx: Option<usize> = None;
    let mut best_score: usize = 0;
    for (idx, line) in lines.iter().enumerate() {
        let score = common_prefix_len(line.trim_start(), needle_trimmed);
        if score > best_score {
            best_score = score;
            best_idx = Some(idx);
        }
    }

    // Require a meaningful overlap so we don't show noise.
    let min_score = needle_trimmed.len().min(8);
    if best_score < min_score {
        return None;
    }

    let center = best_idx?;
    let start = center.saturating_sub(HINT_RADIUS);
    let end = (center + HINT_RADIUS + 1).min(lines.len());

    let mut out = String::new();
    for (offset, line) in lines[start..end].iter().enumerate() {
        let line_no = start + offset + 1;
        let marker = if start + offset == center { ">" } else { " " };
        let truncated = truncate_to_chars(line, MAX_LINE_LEN);
        _ = writeln!(out, "{marker} {line_no:>5} | {truncated}");
    }
    Some(out.trim_end_matches('\n').to_string())
}

/// Return `line` truncated to at most `max_chars` characters, appending an
/// ellipsis if it was shortened. Avoids slicing bytes inside a UTF-8 sequence.
fn truncate_to_chars(line: &str, max_chars: usize) -> String {
    if line.chars().count() <= max_chars {
        return line.to_string();
    }
    let mut out: String = line.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// Length of the longest common byte prefix between `a` and `b`.
fn common_prefix_len(a: &str, b: &str) -> usize {
    a.as_bytes()
        .iter()
        .zip(b.as_bytes().iter())
        .take_while(|(x, y)| x == y)
        .count()
}

// =============================================================================
// Edit Application
// =============================================================================

/// Apply edits in reverse order (highest position first) to prevent position shifting
fn apply_edits_reverse_order(content: &str, matched_edits: &[MatchedEdit]) -> String {
    let mut result = content.to_string();

    // Process highest index first
    for edit in matched_edits.iter().rev() {
        result.replace_range(edit.index..edit.index + edit.match_length, &edit.new_text);
    }

    result
}

// =============================================================================
// Line Ending Handling
// =============================================================================

/// Detect the line ending style used in the content
fn detect_line_ending(content: &str) -> LineEnding {
    if content.contains("\r\n") {
        LineEnding::Crlf
    } else {
        LineEnding::Lf
    }
}

/// Normalize CRLF line endings to LF for consistent processing.
///
/// Bare CR characters are preserved, and every normalized byte offset can be
/// mapped back to the original string so unrelated bytes are not rewritten.
fn normalize_crlf_line_endings(content: &str) -> NormalizedContent {
    let mut normalized = String::with_capacity(content.len());
    let mut original_offsets = Vec::with_capacity(content.len());
    let mut chars = content.char_indices().peekable();

    while let Some((index, ch)) = chars.next() {
        if ch == '\r' && chars.peek().is_some_and(|(_, next)| *next == '\n') {
            normalized.push('\n');
            original_offsets.push(index);
            let _ = chars.next();
            continue;
        }

        let normalized_index = normalized.len();
        normalized.push(ch);
        original_offsets.resize(normalized.len(), index);

        debug_assert!(normalized.is_char_boundary(normalized_index));
    }

    NormalizedContent {
        content: normalized,
        original_offsets,
        original_len: content.len(),
    }
}

/// Convert replacement LF line endings to the file's dominant line ending.
fn normalize_replacement_line_endings(content: &str, ending: LineEnding) -> String {
    match ending {
        LineEnding::Crlf => content.replace('\n', "\r\n"),
        LineEnding::Lf => content.to_string(),
    }
}

impl NormalizedContent {
    /// Return the original byte range corresponding to a normalized match.
    fn original_range(&self, start: usize, len: usize) -> Result<(usize, usize), String> {
        let end = start + len;
        let original_start = self.original_offset(start)?;
        let original_end = if end == self.content.len() {
            self.original_len
        } else {
            self.original_offset(end)?
        };

        Ok((original_start, original_end - original_start))
    }

    /// Return the original byte offset for a normalized byte offset.
    fn original_offset(&self, index: usize) -> Result<usize, String> {
        self.original_offsets.get(index).copied().ok_or_else(|| {
            "Internal error: edit match did not map to original file content".to_string()
        })
    }
}

// =============================================================================
// BOM Handling
// =============================================================================

/// Strip BOM from content if present, returning (`optional_bom`, `content_without_bom`)
fn strip_bom(content: &str) -> (Option<&str>, &str) {
    // UTF-8 BOM is \u{FEFF} which is 3 bytes in UTF-8
    const BOM: &str = "\u{FEFF}";

    content
        .strip_prefix(BOM)
        .map_or((None, content), |stripped| (Some(BOM), stripped))
}

/// Restore BOM to content if it was present
fn restore_bom(content: String, bom: Option<&str>) -> String {
    match bom {
        Some(bom_str) => format!("{bom_str}{content}"),
        None => content,
    }
}

// =============================================================================
// Unified Diff Generation
// =============================================================================

/// Generate a unified diff showing the changes
fn generate_unified_diff(original: &str, modified: &str, path: &Path) -> String {
    use similar::TextDiff;

    let diff = TextDiff::from_lines(original, modified);
    diff.unified_diff()
        .context_radius(3)
        .header(
            &format!("--- {}", path.display()),
            &format!("+++ {}", path.display()),
        )
        .to_string()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // =========================================================================
    // Multiple Edits Tests
    // =========================================================================

    #[test]
    fn summarize_args_only_requires_path() {
        let args = serde_json::json!({
            "path": "src/main.rs",
            "edits": [
                { "old_text": 123, "new_text": ["not", "summary", "data"] }
            ]
        })
        .to_string();

        assert_eq!(summarize_args(&args), "src/main.rs");
    }

    #[test]
    fn multiple_edits_in_single_call() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Hello world\nGoodbye earth\n").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "Hello", "new_text": "Hi" },
                { "old_text": "Goodbye", "new_text": "Bye" }
            ]
        })
        .to_string();

        let result = execute_edit(&ToolContext::from_current_process(), &args).unwrap();
        assert!(result.output.contains("Applied 2 edits"));

        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "Hi world\nBye earth\n");
    }

    #[test]
    fn error_on_overlapping_edits() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Hello world").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "Hello", "new_text": "Hi" },
                { "old_text": "lo wor", "new_text": "xx" }
            ]
        })
        .to_string();

        let result = execute_edit(&ToolContext::from_current_process(), &args);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("overlap"),
            "Error should mention overlap: {err}"
        );
    }

    #[test]
    fn error_on_too_many_edits() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Hello world").unwrap();

        let edits: Vec<_> = (0..15)
            .map(|i| serde_json::json!({ "old_text": format!("text{i}"), "new_text": format!("new{i}") }))
            .collect();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": edits
        })
        .to_string();

        let result = execute_edit(&ToolContext::from_current_process(), &args);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("Too many edits"),
            "Error should mention too many edits: {err}"
        );
        assert!(
            err.contains("Maximum 10"),
            "Error should mention max: {err}"
        );
    }

    #[test]
    fn error_with_specific_edit_number() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Hello world").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "Hello", "new_text": "Hi" },
                { "old_text": "notfound", "new_text": "replacement" }
            ]
        })
        .to_string();

        let result = execute_edit(&ToolContext::from_current_process(), &args);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("Edit 2 of 2"),
            "Error should mention edit number and total: {err}"
        );
        assert!(
            err.contains("could not find"),
            "Error should mention not found: {err}"
        );
    }

    #[test]
    fn error_on_multiple_matches_in_single_edit() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Hello world\nGoodbye world").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "world", "new_text": "universe" }
            ]
        })
        .to_string();

        let result = execute_edit(&ToolContext::from_current_process(), &args);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("Edit 1 of 1"),
            "Error should mention edit number and total: {err}"
        );
        assert!(
            err.contains("matches 2 locations"),
            "Error should mention multiple matches: {err}"
        );
        assert!(
            err.contains("Candidate match contexts"),
            "Error should include candidate contexts: {err}"
        );
        assert!(
            err.contains("Match 1 at line 1") && err.contains("Match 2 at line 2"),
            "Error should identify candidate line numbers: {err}"
        );
        assert!(
            err.contains(">     1 | Hello world") && err.contains(">     2 | Goodbye world"),
            "Error should include line-numbered matching snippets: {err}"
        );
    }

    #[test]
    fn ambiguous_match_contexts_are_capped() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let original = "target 1\ntarget 2\ntarget 3\ntarget 4\ntarget 5\ntarget 6\n";
        fs::write(&file_path, original).unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "target", "new_text": "replacement" }
            ]
        })
        .to_string();

        let err = execute_edit(&ToolContext::from_current_process(), &args).unwrap_err();
        assert!(
            err.contains("matches 6 locations"),
            "Error should report the full match count: {err}"
        );
        assert!(
            err.contains("Showing first 5 of 6 matches."),
            "Error should explain the candidate cap: {err}"
        );
        assert!(
            err.contains("Match 5 at line 5") && !err.contains("Match 6 at line 6"),
            "Error should include only the capped candidate list: {err}"
        );

        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, original, "File should be unchanged after failure");
    }

    #[test]
    fn error_on_no_edits_provided() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Hello world").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": []
        })
        .to_string();

        let result = execute_edit(&ToolContext::from_current_process(), &args);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No edits provided"));
    }

    #[test]
    fn error_on_identical_old_and_new_text() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Hello world").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "world", "new_text": "world" }
            ]
        })
        .to_string();

        let result = execute_edit(&ToolContext::from_current_process(), &args);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("identical"),
            "Error should mention identical: {err}"
        );
    }

    // =========================================================================
    // Line Ending Tests
    // =========================================================================

    #[test]
    fn preserves_lf_line_endings() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Hello world\nGoodbye earth\n").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "world", "new_text": "universe" }
            ]
        })
        .to_string();

        let _ = execute_edit(&ToolContext::from_current_process(), &args).unwrap();

        let content = fs::read_to_string(&file_path).unwrap();
        assert!(content.contains("Hello universe\n"));
        assert!(!content.contains("\r\n"));
    }

    #[test]
    fn preserves_crlf_line_endings() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Hello world\r\nGoodbye earth\r\n").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "world", "new_text": "universe" }
            ]
        })
        .to_string();

        let _ = execute_edit(&ToolContext::from_current_process(), &args).unwrap();

        let content = String::from_utf8(fs::read(&file_path).unwrap()).unwrap();
        assert!(content.contains("Hello universe\r\n"));
        assert!(content.contains("\r\n"));
    }

    #[test]
    fn preserves_unrelated_bare_cr_bytes() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, b"header\runrelated\r\nHello world\r\n").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "Hello world\n", "new_text": "Hello universe\n" }
            ]
        })
        .to_string();

        let _ = execute_edit(&ToolContext::from_current_process(), &args).unwrap();

        let content = fs::read(&file_path).unwrap();
        assert_eq!(content, b"header\runrelated\r\nHello universe\r\n");
    }

    #[test]
    fn preserves_unedited_mixed_line_endings() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, b"lf only\ncrlf only\r\nreplace me\r\n").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "replace me\n", "new_text": "replaced\n" }
            ]
        })
        .to_string();

        let _ = execute_edit(&ToolContext::from_current_process(), &args).unwrap();

        let content = fs::read(&file_path).unwrap();
        assert_eq!(content, b"lf only\ncrlf only\r\nreplaced\r\n");
    }

    // =========================================================================
    // BOM Tests
    // =========================================================================

    #[test]
    fn handles_utf8_bom() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let bom: &[u8] = b"\xEF\xBB\xBF";
        let content_with_bom: Vec<u8> = [bom, b"Hello world"].concat();
        fs::write(&file_path, &content_with_bom).unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "world", "new_text": "universe" }
            ]
        })
        .to_string();

        let _ = execute_edit(&ToolContext::from_current_process(), &args).unwrap();

        let content = fs::read(&file_path).unwrap();
        assert!(
            content.starts_with(b"\xEF\xBB\xBF"),
            "BOM should be preserved"
        );
        let content_str = String::from_utf8(content).unwrap();
        assert!(content_str.contains("Hello universe"));
    }

    #[test]
    fn no_bom_stays_no_bom() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Hello world").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "world", "new_text": "universe" }
            ]
        })
        .to_string();

        let _ = execute_edit(&ToolContext::from_current_process(), &args).unwrap();

        let content = fs::read(&file_path).unwrap();
        assert!(
            !content.starts_with(b"\xEF\xBB\xBF"),
            "No BOM should be added"
        );
    }

    // =========================================================================
    // Legacy Tests (from original implementation)
    // =========================================================================

    #[test]
    fn edit_single_occurrence() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Hello world\nGoodbye earth").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "world", "new_text": "universe" }
            ]
        })
        .to_string();

        let result = execute_edit(&ToolContext::from_current_process(), &args).unwrap();
        assert!(result.output.contains("Applied 1 edit"));

        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "Hello universe\nGoodbye earth");
    }

    #[test]
    fn error_when_old_text_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Hello world").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "notfound", "new_text": "replacement" }
            ]
        })
        .to_string();

        let result = execute_edit(&ToolContext::from_current_process(), &args);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("could not find"));
    }

    #[test]
    fn delete_text_with_empty_new_text() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Hello world").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": " world", "new_text": "" }
            ]
        })
        .to_string();

        let result = execute_edit(&ToolContext::from_current_process(), &args).unwrap();
        assert!(result.output.contains("Applied 1 edit"));

        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "Hello");
    }

    #[test]
    fn error_on_binary_file() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.bin");
        fs::write(&file_path, [0u8, 1, 2, 3, 0, 4, 5]).unwrap(); // Contains null bytes

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "test", "new_text": "replacement" }
            ]
        })
        .to_string();

        let result = execute_edit(&ToolContext::from_current_process(), &args);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("binary file"));
    }

    #[test]
    fn error_on_null_byte_after_initial_8k() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let mut content = vec![b'a'; 8192];
        content.extend_from_slice(b"\0tail");
        fs::write(&file_path, content).unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "tail", "new_text": "replacement" }
            ]
        })
        .to_string();

        let result = execute_edit(&ToolContext::from_current_process(), &args);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("detected null bytes"),
            "Error should mention null bytes: {err}"
        );
    }

    #[test]
    fn error_on_invalid_utf8_file() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, b"valid text \xFF more text").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "valid", "new_text": "changed" }
            ]
        })
        .to_string();

        let result = execute_edit(&ToolContext::from_current_process(), &args);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("File contains invalid UTF-8"),
            "Error should mention invalid UTF-8: {err}"
        );
    }

    #[test]
    fn error_on_nonexistent_file() {
        let args = serde_json::json!({
            "path": "/etc/nonexistent_file_12345.txt",
            "edits": [
                { "old_text": "test", "new_text": "replacement" }
            ]
        })
        .to_string();

        let result = execute_edit(&ToolContext::from_current_process(), &args);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn error_on_directory() {
        let temp_dir = TempDir::new().unwrap();

        let args = serde_json::json!({
            "path": temp_dir.path().to_str().unwrap(),
            "edits": [
                { "old_text": "test", "new_text": "replacement" }
            ]
        })
        .to_string();

        let result = execute_edit(&ToolContext::from_current_process(), &args);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not a file"));
    }

    // =========================================================================
    // Path Validation Tests
    // =========================================================================

    #[test]
    fn error_on_path_outside_working_directory() {
        let args = serde_json::json!({
            "path": "/etc/passwd",
            "edits": [
                { "old_text": "test", "new_text": "replacement" }
            ]
        })
        .to_string();

        let result = execute_edit(&ToolContext::from_current_process(), &args);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("outside the working directory")
        );
    }

    // =========================================================================
    // Reverse Order Application Tests
    // =========================================================================

    #[test]
    fn edits_applied_in_correct_positions() {
        // This test verifies that edits are applied in reverse order
        // so that position shifts don't affect later edits
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "AAA BBB CCC").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "AAA", "new_text": "XXX" },
                { "old_text": "CCC", "new_text": "ZZZ" }
            ]
        })
        .to_string();

        let _ = execute_edit(&ToolContext::from_current_process(), &args).unwrap();

        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "XXX BBB ZZZ");
    }

    // =========================================================================
    // Multi-Edit Failure Recovery Tests
    // =========================================================================

    #[test]
    fn multi_edit_failure_reports_atomic_rollback() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let original = "alpha\nbeta\ngamma\n";
        fs::write(&file_path, original).unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "alpha", "new_text": "ALPHA" },
                { "old_text": "beta", "new_text": "BETA" },
                { "old_text": "delta", "new_text": "DELTA" }
            ]
        })
        .to_string();

        let err = execute_edit(&ToolContext::from_current_process(), &args).unwrap_err();

        // Failed edit number and total are visible
        assert!(
            err.contains("Edit 3 of 3"),
            "Error should identify failing edit number and total: {err}"
        );
        // Path is visible
        assert!(
            err.contains(file_path.to_str().unwrap()),
            "Error should include the file path: {err}"
        );
        // Atomicity is explicit
        assert!(
            err.contains("No edits were applied"),
            "Error should state that no edits were applied: {err}"
        );
        assert!(
            err.contains("atomic"),
            "Error should state that the operation is atomic: {err}"
        );
        // Recovery hint
        assert!(
            err.contains("Retry"),
            "Error should suggest a retry strategy: {err}"
        );

        // File is unchanged on disk (proving rollback)
        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, original, "File should be unchanged after failure");
    }

    #[test]
    fn multi_edit_failure_reports_per_edit_preflight_statuses() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let original = "alpha\nbeta\ngamma\n";
        fs::write(&file_path, original).unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "alpha", "new_text": "ALPHA" },
                { "old_text": "delta", "new_text": "DELTA" },
                { "old_text": "gamma", "new_text": "GAMMA" }
            ]
        })
        .to_string();

        let err = execute_edit(&ToolContext::from_current_process(), &args).unwrap_err();
        assert!(
            err.contains("Preflight summary:"),
            "Error should include a preflight summary: {err}"
        );
        assert!(
            err.contains("Edit 1 of 3: matched exactly once."),
            "Error should report the prior successful match: {err}"
        );
        assert!(
            err.contains("Edit 2 of 3: failed: could not find the exact text to replace."),
            "Error should report the failed edit status: {err}"
        );
        assert!(
            err.contains("Edit 3 of 3: not evaluated after the failure."),
            "Error should report that later edits were skipped: {err}"
        );
        assert!(
            err.contains("No edits were applied"),
            "Error should state that no edits were applied: {err}"
        );

        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, original, "File should be unchanged after failure");
    }

    #[test]
    fn multi_edit_preflight_summary_is_bounded() {
        let matched_edits: Vec<_> = (1..=9)
            .map(|edit_index| MatchedEdit {
                new_text: String::new(),
                index: edit_index * 10,
                match_length: 1,
                edit_index,
            })
            .collect();

        let summary = preflight_failure_summary(10, &matched_edits, 10, "failed: missing");
        assert!(
            summary.contains("Edit 1 of 10: matched exactly once."),
            "Summary should include early matched edits: {summary}"
        );
        assert!(
            summary.contains("5 matched edits omitted."),
            "Summary should cap matched status lines: {summary}"
        );
        assert!(
            summary.contains("Edit 10 of 10: failed: missing."),
            "Summary should always include the failed edit: {summary}"
        );
        assert!(
            !summary.contains("Edit 7 of 10: matched exactly once."),
            "Summary should omit excess matched status lines: {summary}"
        );
    }

    #[test]
    fn multi_edit_ambiguous_match_reports_atomic_rollback() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let original = "foo\nbar\nfoo\n";
        fs::write(&file_path, original).unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "bar", "new_text": "BAR" },
                { "old_text": "foo", "new_text": "FOO" }
            ]
        })
        .to_string();

        let err = execute_edit(&ToolContext::from_current_process(), &args).unwrap_err();
        assert!(
            err.contains("Edit 2 of 2"),
            "Error should identify failing edit number and total: {err}"
        );
        assert!(
            err.contains("matches 2 locations"),
            "Error should describe the ambiguity: {err}"
        );
        assert!(
            err.contains("No edits were applied"),
            "Error should state that no edits were applied: {err}"
        );
        assert!(
            err.contains("atomic"),
            "Error should state atomic semantics for multi-edit: {err}"
        );

        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, original, "File should be unchanged after failure");
    }

    #[test]
    fn multi_edit_overlap_reports_atomic_rollback_with_path() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let original = "Hello world";
        fs::write(&file_path, original).unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "Hello", "new_text": "Hi" },
                { "old_text": "lo wor", "new_text": "xx" }
            ]
        })
        .to_string();

        let err = execute_edit(&ToolContext::from_current_process(), &args).unwrap_err();
        assert!(
            err.contains("overlap"),
            "Error should mention overlap: {err}"
        );
        assert!(
            err.contains(file_path.to_str().unwrap()),
            "Error should include the file path: {err}"
        );
        assert!(
            err.contains("No edits were applied"),
            "Error should state that no edits were applied: {err}"
        );
        assert!(
            err.contains("atomic"),
            "Error should state atomic semantics for multi-edit: {err}"
        );

        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, original, "File should be unchanged after failure");
    }

    #[test]
    fn single_edit_failure_omits_atomic_phrasing() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Hello world").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "missing", "new_text": "x" }
            ]
        })
        .to_string();

        let err = execute_edit(&ToolContext::from_current_process(), &args).unwrap_err();
        assert!(
            err.contains("No edits were applied"),
            "Single-edit failure should still note no edits were applied: {err}"
        );
        assert!(
            !err.contains("atomic"),
            "Single-edit failure should not include the atomic-rollback phrasing: {err}"
        );
    }

    #[test]
    fn multi_edit_failure_includes_nearest_match_hint() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let original =
            "fn alpha(x: i32) -> i32 {\n    x + 1\n}\n\nfn beta(y: i32) -> i32 {\n    y * 2\n}\n";
        fs::write(&file_path, original).unwrap();

        // Search text close to a real line so the hint kicks in.
        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "fn alpha(x: u32) -> i32 {", "new_text": "fn alpha(x: i64) -> i64 {" }
            ]
        })
        .to_string();

        let err = execute_edit(&ToolContext::from_current_process(), &args).unwrap_err();
        assert!(
            err.contains("Nearest matching context"),
            "Error should include a nearest-match hint: {err}"
        );
        assert!(
            err.contains("fn alpha"),
            "Hint should reference the closest line: {err}"
        );
    }

    #[test]
    fn multiple_edits_with_different_lengths() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "short medium verylong").unwrap();

        let args = serde_json::json!({
            "path": file_path.to_str().unwrap(),
            "edits": [
                { "old_text": "short", "new_text": "s" },
                { "old_text": "medium", "new_text": "MEDIUM" },
                { "old_text": "verylong", "new_text": "vl" }
            ]
        })
        .to_string();

        let _ = execute_edit(&ToolContext::from_current_process(), &args).unwrap();

        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "s MEDIUM vl");
    }
}
