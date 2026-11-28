//! Shared UTF-8 boundary and bounded-text helpers.

/// Longest prefix no longer than `max_bytes` that ends on a character boundary.
pub fn utf8_head(text: &str, max_bytes: usize) -> &str {
    let end = text.floor_char_boundary(max_bytes.min(text.len()));
    text.get(..end).unwrap_or_default()
}

/// Longest suffix no longer than `max_bytes` that starts on a character boundary.
pub fn utf8_tail(text: &str, max_bytes: usize) -> &str {
    let start = text.ceil_char_boundary(text.len().saturating_sub(max_bytes));
    text.get(start..).unwrap_or_default()
}

/// Smallest character boundary at or after `index`, clamped to the text length.
pub fn utf8_index_ceil(text: &str, index: usize) -> usize {
    text.ceil_char_boundary(index.min(text.len()))
}

/// Largest character boundary at or before `index`, not below `start`.
pub fn utf8_index_floor(text: &str, start: usize, index: usize) -> usize {
    let mut index = index.min(text.len());
    while index > start && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Collapse whitespace in a description and bound it by character count.
///
/// With `first_sentence`, text after the first period is dropped before
/// trimming. An empty result returns `None`.
pub fn bounded_description(
    description: &str,
    max_chars: usize,
    first_sentence: bool,
) -> Option<String> {
    let one_line = description.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = if first_sentence {
        one_line.split('.').next().unwrap_or(&one_line).trim()
    } else {
        &one_line
    };
    if text.is_empty() {
        return None;
    }
    let mut bounded: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        bounded.push_str("...");
    }
    Some(bounded)
}

/// Append as much of `line` as fits in `max_chars`; return whether it fit.
///
/// A cut line is shortened by three characters and ends with a plain ellipsis,
/// while budgets of three characters or fewer receive no ellipsis.
pub fn append_bounded(output: &mut String, line: &str, max_chars: usize) -> bool {
    let remaining = max_chars.saturating_sub(output.chars().count());
    if remaining == 0 {
        return false;
    }
    let line_characters = line.chars().count();
    if line_characters <= remaining {
        output.push_str(line);
        return true;
    }
    if remaining > 3 {
        output.extend(line.chars().take(remaining - 3));
        output.push_str("...");
    } else {
        output.extend(line.chars().take(remaining));
    }
    false
}
