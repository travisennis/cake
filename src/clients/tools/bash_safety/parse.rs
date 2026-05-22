// Shell-parsing utilities for the destructive command guard.
//
// These functions handle splitting commands, extracting inline scripts,
// stripping shell data contexts, and other parsing concerns needed by
// the safety check rules.

// =============================================================================
// Whitespace Normalization
// =============================================================================

/// Collapse all whitespace runs to a single space.
pub(super) fn normalize_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// =============================================================================
// Segment Splitting
// =============================================================================

/// Split a command string on common separators (`;`, `&&`, `||`, `|`, `\n`).
/// Tracks single and double quotes so that separators inside quoted strings
/// are not treated as split points. This is scoped preflight matching, not
/// complete shell tokenization.
pub(super) fn split_segments(command: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let bytes = command.as_bytes();
    let len = bytes.len();
    let mut start = 0;
    let mut i = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while i < len {
        // Track quote state (skip escaped quotes in double-quote context)
        if bytes[i] == b'\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            i += 1;
            continue;
        }
        if bytes[i] == b'"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            i += 1;
            continue;
        }
        if bytes[i] == b'\\' && in_double_quote && i + 1 < len {
            i += 2; // skip escaped char
            continue;
        }

        // Only split when outside quotes
        if !in_single_quote && !in_double_quote {
            if bytes[i] == b'\n' || bytes[i] == b';' {
                if let Some(segment) = command.get(start..i) {
                    segments.push(segment);
                }
                start = i + 1;
                i += 1;
                continue;
            }
            if i + 1 < len && bytes[i] == b'&' && bytes[i + 1] == b'&' {
                if let Some(segment) = command.get(start..i) {
                    segments.push(segment);
                }
                start = i + 2;
                i += 2;
                continue;
            }
            if i + 1 < len && bytes[i] == b'|' && bytes[i + 1] == b'|' {
                if let Some(segment) = command.get(start..i) {
                    segments.push(segment);
                }
                start = i + 2;
                i += 2;
                continue;
            }
            // Single pipe — also a command boundary
            if bytes[i] == b'|' {
                if let Some(segment) = command.get(start..i) {
                    segments.push(segment);
                }
                start = i + 1;
                i += 1;
                continue;
            }
        }

        i += 1;
    }

    if start < len
        && let Some(segment) = command.get(start..)
    {
        segments.push(segment);
    }

    segments
}

// =============================================================================
// Inline Script Extraction
// =============================================================================

/// Extract the inner script from documented `bash -c "..."` or `sh -c "..."`
/// wrappers.
///
/// Other shells and invocation forms are intentionally outside this guard's
/// scope; the OS sandbox remains the filesystem enforcement boundary.
/// Returns `None` if the command is not a shell -c invocation.
/// Handles flexible whitespace between the shell name and `-c`.
pub(super) fn extract_inline_script(command: &str) -> Option<String> {
    let normalized = normalize_whitespace(command);
    let lower = normalized.to_lowercase();

    // Match: bash -c or sh -c as command/path tokens, not substrings of other
    // command names such as zsh.
    let (idx, shell_len) = find_inline_shell_invocation(&lower)?;

    // Map back to original command: count the corresponding characters
    // by finding the same position accounting for whitespace normalization
    let after_flag = skip_to_after_flag(command, idx, shell_len)?;

    let trimmed = after_flag.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Strip surrounding quotes if present
    let first = trimmed.as_bytes()[0];
    if (first == b'"' || first == b'\'') && trimmed.len() > 1 {
        let quote = first;
        // Find matching closing quote (not escaped)
        let inner = trimmed.get(1..)?;
        let mut end = None;
        let bytes = inner.as_bytes();
        let mut j = 0;
        while j < bytes.len() {
            if bytes[j] == b'\\' {
                j += 2;
                continue;
            }
            if bytes[j] == quote {
                end = Some(j);
                break;
            }
            j += 1;
        }
        if let Some(e) = end {
            return inner.get(..e).map(str::to_string);
        }
    }

    // Unquoted — take everything
    Some(trimmed.to_string())
}

pub(super) fn find_inline_shell_invocation(normalized_lower: &str) -> Option<(usize, usize)> {
    find_shell_token(normalized_lower, "bash -c ")
        .map(|idx| (idx, 8))
        .or_else(|| find_shell_token(normalized_lower, "sh -c ").map(|idx| (idx, 6)))
}

pub(super) fn find_shell_token(normalized_lower: &str, needle: &str) -> Option<usize> {
    let mut offset = 0;

    while let Some(relative_idx) = normalized_lower.get(offset..)?.find(needle) {
        let idx = offset + relative_idx;
        if is_shell_token_boundary(normalized_lower.as_bytes(), idx) {
            return Some(idx);
        }
        offset = idx + 1;
    }

    None
}

pub(super) fn is_shell_token_boundary(bytes: &[u8], idx: usize) -> bool {
    idx == 0 || matches!(bytes[idx - 1], b' ' | b'\t' | b'\n' | b'/')
}

/// Map a position in the whitespace-normalized string back to the original,
/// then skip past `shell_len` normalized tokens to find where the script starts.
pub(super) fn skip_to_after_flag(
    original: &str,
    norm_pos: usize,
    shell_len: usize,
) -> Option<&str> {
    // Count how many non-whitespace characters precede norm_pos in the normalized string
    let normalized = normalize_whitespace(original);
    let prefix = normalized.get(..norm_pos + shell_len)?;
    let token_count = prefix.split_whitespace().count();

    // Walk the original string, skipping that many whitespace-separated tokens
    let mut seen = 0;
    let mut i = 0;
    let bytes = original.as_bytes();
    while i < bytes.len() && seen < token_count {
        // Skip whitespace
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        // Skip token
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        seen += 1;
    }

    original.get(i..)
}

// =============================================================================
// Command Substitution Extraction
// =============================================================================

/// Extract executable command substitutions from a shell segment.
///
/// Single-quoted text is shell data. Double-quoted text can still execute
/// substitutions, so `$()` and backticks are inspected there too.
pub(super) fn extract_command_substitutions(command: &str) -> Vec<String> {
    let bytes = command.as_bytes();
    let mut substitutions = Vec::new();
    let mut i = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while i < bytes.len() {
        match bytes[i] {
            b'\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
                i += 1;
            },
            b'"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
                i += 1;
            },
            b'\\' => {
                i += 2;
            },
            b'$' if !in_single_quote && i + 1 < bytes.len() && bytes[i + 1] == b'(' => {
                if let Some((inner, end)) = extract_dollar_paren(command, i + 2) {
                    substitutions.push(inner);
                    i = end;
                } else {
                    i += 2;
                }
            },
            b'`' if !in_single_quote => {
                if let Some((inner, end)) = extract_backticks(command, i + 1) {
                    substitutions.push(inner);
                    i = end;
                } else {
                    i += 1;
                }
            },
            _ => i += 1,
        }
    }

    substitutions
}

/// Strip shell data contexts before looking for destructive command text.
/// Command substitutions are replaced with spaces because they are recursively
/// validated by `extract_command_substitutions`.
pub(super) fn strip_shell_data(command: &str) -> String {
    let bytes = command.as_bytes();
    let mut stripped = String::with_capacity(command.len());
    let mut i = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while i < bytes.len() {
        match bytes[i] {
            b'\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
                stripped.push(' ');
                i += 1;
            },
            b'"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
                stripped.push(' ');
                i += 1;
            },
            b'\\' if in_double_quote => {
                stripped.push(' ');
                if i + 1 < bytes.len() {
                    stripped.push(' ');
                    i += 2;
                } else {
                    i += 1;
                }
            },
            b'$' if !in_single_quote && i + 1 < bytes.len() && bytes[i + 1] == b'(' => {
                if let Some((_inner, end)) = extract_dollar_paren(command, i + 2) {
                    stripped.push(' ');
                    i = end;
                } else {
                    stripped.push(bytes[i] as char);
                    i += 1;
                }
            },
            b'`' if !in_single_quote => {
                if let Some((_inner, end)) = extract_backticks(command, i + 1) {
                    stripped.push(' ');
                    i = end;
                } else {
                    stripped.push(bytes[i] as char);
                    i += 1;
                }
            },
            _ if in_single_quote || in_double_quote => {
                stripped.push(' ');
                i += 1;
            },
            _ => {
                stripped.push(bytes[i] as char);
                i += 1;
            },
        }
    }

    stripped
}

pub(super) fn extract_dollar_paren(command: &str, start: usize) -> Option<(String, usize)> {
    let bytes = command.as_bytes();
    let mut i = start;
    let mut depth = 1;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while i < bytes.len() {
        match bytes[i] {
            b'\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
                i += 1;
            },
            b'"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
                i += 1;
            },
            b'\\' => {
                i += 2;
            },
            b'$' if !in_single_quote && i + 1 < bytes.len() && bytes[i + 1] == b'(' => {
                depth += 1;
                i += 2;
            },
            b')' if !in_single_quote => {
                depth -= 1;
                if depth == 0 {
                    return command
                        .get(start..i)
                        .map(|inner| (inner.to_string(), i + 1));
                }
                i += 1;
            },
            _ => i += 1,
        }
    }

    None
}

pub(super) fn extract_backticks(command: &str, start: usize) -> Option<(String, usize)> {
    let bytes = command.as_bytes();
    let mut i = start;

    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'`' => {
                return command
                    .get(start..i)
                    .map(|inner| (inner.to_string(), i + 1));
            },
            _ => i += 1,
        }
    }

    None
}
