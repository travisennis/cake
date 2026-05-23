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

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // normalize_whitespace
    // =========================================================================

    #[test]
    fn normalize_whitespace_empty() {
        assert_eq!(normalize_whitespace(""), "");
    }

    #[test]
    fn normalize_whitespace_noop() {
        assert_eq!(normalize_whitespace("hello world"), "hello world");
        assert_eq!(normalize_whitespace("git"), "git");
    }

    #[test]
    fn normalize_whitespace_collapses_multiple_spaces() {
        assert_eq!(normalize_whitespace("hello   world"), "hello world");
        assert_eq!(
            normalize_whitespace("git    reset   --hard"),
            "git reset --hard"
        );
    }

    #[test]
    fn normalize_whitespace_tabs_and_newlines() {
        assert_eq!(normalize_whitespace("hello\tworld"), "hello world");
        assert_eq!(normalize_whitespace("hello\nworld"), "hello world");
        assert_eq!(normalize_whitespace("hello\r\nworld"), "hello world");
    }

    #[test]
    fn normalize_whitespace_leading_and_trailing() {
        assert_eq!(normalize_whitespace("  hello world  "), "hello world");
        assert_eq!(normalize_whitespace("\n\nhello\n\n"), "hello");
    }

    #[test]
    fn normalize_whitespace_mixed() {
        assert_eq!(
            normalize_whitespace("  bash   -c  \"echo\thi\"  "),
            "bash -c \"echo hi\""
        );
    }

    // =========================================================================
    // split_segments
    // =========================================================================

    #[test]
    fn split_segments_no_separators() {
        let result = split_segments("git status");
        assert_eq!(result, vec!["git status"]);
    }

    #[test]
    fn split_segments_empty_input() {
        let result = split_segments("");
        assert!(result.is_empty());
    }

    #[test]
    fn split_segments_semicolon() {
        let result = split_segments("echo a; echo b");
        assert_eq!(result, vec!["echo a", " echo b"]);
    }

    #[test]
    fn split_segments_double_ampersand() {
        let result = split_segments("cargo build && cargo test");
        assert_eq!(result, vec!["cargo build ", " cargo test"]);
    }

    #[test]
    fn split_segments_double_pipe() {
        let result = split_segments("false || echo ok");
        assert_eq!(result, vec!["false ", " echo ok"]);
    }

    #[test]
    fn split_segments_single_pipe() {
        let result = split_segments("git status | head -5");
        assert_eq!(result, vec!["git status ", " head -5"]);
    }

    #[test]
    fn split_segments_newline() {
        let result = split_segments("git add .\ngit commit");
        assert_eq!(result, vec!["git add .", "git commit"]);
    }

    #[test]
    fn split_segments_trailing_separator() {
        let result = split_segments("git stash;");
        assert_eq!(result, vec!["git stash"]);
    }

    #[test]
    fn split_segments_multiple_separators() {
        let result = split_segments("a;b&&c||d|e\nf");
        assert_eq!(result, vec!["a", "b", "c", "d", "e", "f"]);
    }

    #[test]
    fn split_segments_quoted_semicolon_not_split() {
        // Semicolons inside single quotes should not split
        let result = split_segments("git commit -m 'foo; bar'");
        assert_eq!(result, vec!["git commit -m 'foo; bar'"]);
    }

    #[test]
    fn split_segments_quoted_double_ampersand_not_split() {
        // && inside double quotes should not split
        let result = split_segments("echo \"foo && bar\"");
        assert_eq!(result, vec!["echo \"foo && bar\""]);
    }

    #[test]
    fn split_segments_quoted_pipe_not_split() {
        let result = split_segments("alias x='|' && echo ok");
        assert_eq!(result, vec!["alias x='|' ", " echo ok"]);
    }

    #[test]
    fn split_segments_escaped_quote_in_double_quotes() {
        // Escaped quote inside double-quoted string should be consumed
        let result = split_segments("echo \"foo\\\"bar; echo after\"");
        assert_eq!(result, vec!["echo \"foo\\\"bar; echo after\""]);
    }

    #[test]
    fn split_segments_only_separators() {
        let result = split_segments("||");
        assert_eq!(result, vec![""]);
    }

    // =========================================================================
    // extract_inline_script
    // =========================================================================

    #[test]
    fn extract_inline_script_bash_c_double_quoted() {
        let result = extract_inline_script("bash -c \"echo hello\"");
        assert_eq!(result, Some("echo hello".to_string()));
    }

    #[test]
    fn extract_inline_script_sh_c_single_quoted() {
        let result = extract_inline_script("sh -c 'git status'");
        assert_eq!(result, Some("git status".to_string()));
    }

    #[test]
    fn extract_inline_script_bin_bash_c() {
        let result = extract_inline_script("/bin/bash -c \"echo hi\"");
        assert_eq!(result, Some("echo hi".to_string()));
    }

    #[test]
    fn extract_inline_script_extra_whitespace() {
        let result = extract_inline_script("bash  -c \"script\"");
        assert_eq!(result, Some("script".to_string()));
        let result = extract_inline_script("bash\t-c \"script\"");
        assert_eq!(result, Some("script".to_string()));
    }

    #[test]
    fn extract_inline_script_not_shell_c() {
        let result = extract_inline_script("zsh -c 'git reset --hard'");
        assert_eq!(result, None);
    }

    #[test]
    fn extract_inline_script_no_c_flag() {
        let result = extract_inline_script("bash script.sh");
        assert_eq!(result, None);
    }

    #[test]
    fn extract_inline_script_empty_script() {
        let result = extract_inline_script("bash -c \"\"");
        assert_eq!(result, Some(String::new()));
    }

    #[test]
    fn extract_inline_script_just_flag() {
        let result = extract_inline_script("bash -c");
        assert_eq!(result, None);
    }

    #[test]
    fn extract_inline_script_unclosed_quote() {
        // Unclosed double quote — the opening quote is part of the returned script
        let result = extract_inline_script("bash -c \"echo");
        assert_eq!(result, Some("\"echo".to_string()));
    }

    #[test]
    fn extract_inline_script_nested_quotes() {
        let result = extract_inline_script("bash -c \"echo 'nested'\"");
        assert_eq!(result, Some("echo 'nested'".to_string()));
    }

    #[test]
    fn extract_inline_script_unquoted() {
        let result = extract_inline_script("bash -c echo hello");
        assert_eq!(result, Some("echo hello".to_string()));
    }

    // =========================================================================
    // find_inline_shell_invocation
    // =========================================================================

    #[test]
    fn find_inline_shell_invocation_finds_bash() {
        assert_eq!(find_inline_shell_invocation("bash -c echo"), Some((0, 8)));
    }

    #[test]
    fn find_inline_shell_invocation_finds_sh() {
        assert_eq!(find_inline_shell_invocation("sh -c echo"), Some((0, 6)));
    }

    #[test]
    fn find_inline_shell_invocation_finds_bin_bash() {
        assert_eq!(
            find_inline_shell_invocation("/bin/bash -c echo"),
            Some((5, 8))
        );
    }

    #[test]
    fn find_inline_shell_invocation_not_found() {
        assert_eq!(find_inline_shell_invocation("zsh -c echo"), None);
        assert_eq!(find_inline_shell_invocation("echo hello"), None);
    }

    #[test]
    fn find_inline_shell_invocation_case_insensitive() {
        // find_inline_shell_invocation operates on already-lowered input;
        // case sensitivity is handled by extract_inline_script which lowercases first.
        assert_eq!(find_inline_shell_invocation("bash -c echo"), Some((0, 8)));
        assert_eq!(find_inline_shell_invocation("sh -c echo"), Some((0, 6)));
    }

    #[test]
    fn extract_inline_script_is_case_insensitive() {
        assert_eq!(
            extract_inline_script("BASH -c \"echo hi\""),
            Some("echo hi".to_string())
        );
    }

    #[test]
    fn find_inline_shell_invocation_after_leading_token() {
        let result = find_inline_shell_invocation("echo safe ; bash -c \"dangerous\"");
        // Position past the leading text
        assert!(result.is_some());
        let (idx, _) = result.unwrap();
        assert!(idx > 0, "should find bash after the leading text");
    }

    // =========================================================================
    // find_shell_token
    // =========================================================================

    #[test]
    fn find_shell_token_at_start() {
        assert_eq!(find_shell_token("bash -c echo", "bash -c "), Some(0));
    }

    #[test]
    fn find_shell_token_after_boundary() {
        assert_eq!(
            find_shell_token("echo safe ; bash -c echo", "bash -c "),
            Some(12)
        );
    }

    #[test]
    fn find_shell_token_not_mid_word() {
        // "bash" inside "zsh -c" should not match because "zsh -c"
        // doesn't start with "bash" at a token boundary
        assert_eq!(find_shell_token("zsh -c echo", "bash -c "), None);
    }

    #[test]
    fn find_shell_token_not_found() {
        assert_eq!(find_shell_token("echo hello", "bash -c "), None);
        assert_eq!(find_shell_token("git status", "sh -c "), None);
    }

    #[test]
    fn find_shell_token_slash_boundary() {
        assert_eq!(find_shell_token("/bin/bash -c echo", "bash -c "), Some(5));
    }

    // =========================================================================
    // is_shell_token_boundary
    // =========================================================================

    #[test]
    fn boundary_at_start_of_string() {
        assert!(is_shell_token_boundary(b"bash -c", 0));
    }

    #[test]
    fn boundary_after_space() {
        assert!(is_shell_token_boundary(b"use bash -c", 4));
    }

    #[test]
    fn boundary_after_tab() {
        assert!(is_shell_token_boundary(b"use\tbash -c", 4));
    }

    #[test]
    fn boundary_after_newline() {
        assert!(is_shell_token_boundary(b"use\nbash -c", 4));
    }

    #[test]
    fn boundary_after_slash() {
        assert!(is_shell_token_boundary(b"/bin/bash -c", 5));
    }

    #[test]
    fn boundary_not_mid_word() {
        assert!(!is_shell_token_boundary(b"rebash -c", 2));
        assert!(!is_shell_token_boundary(b"foobash", 3));
    }

    // =========================================================================
    // skip_to_after_flag
    // =========================================================================

    #[test]
    fn skip_to_after_flag_simple() {
        // "bash -c " (len 8) at position 0 → skip 2 tokens → after "bash -c "
        let result = skip_to_after_flag("bash -c \"echo\"", 0, 8);
        assert_eq!(result, Some(" \"echo\""));
    }

    #[test]
    fn skip_to_after_flag_extra_whitespace() {
        let result = skip_to_after_flag("bash  -c  \"echo\"", 0, 8);
        assert_eq!(result, Some("  \"echo\""));
    }

    #[test]
    fn skip_to_after_flag_bin_bash() {
        // "/bin/bash -c " with norm_pos=5 and shell_len=8 → skip 2 tokens from start
        let result = skip_to_after_flag("/bin/bash -c \"script\"", 5, 8);
        assert_eq!(result, Some(" \"script\""));
    }

    #[test]
    fn skip_to_after_flag_trailing_normalized_whitespace() {
        let result = skip_to_after_flag("bash -c \"hi\"  ", 0, 8);
        assert_eq!(result, Some(" \"hi\"  "));
    }

    #[test]
    fn skip_to_after_flag_sh_no_script() {
        let result = skip_to_after_flag("sh -c", 0, 6);
        assert_eq!(result, None);
    }

    // =========================================================================
    // extract_dollar_paren
    // =========================================================================

    #[test]
    fn extract_dollar_paren_simple() {
        let result = extract_dollar_paren("$(echo hi)", 2);
        assert_eq!(result, Some(("echo hi".to_string(), 10)));
    }

    #[test]
    fn extract_dollar_paren_nested() {
        let result = extract_dollar_paren("$(echo $(whoami))", 2);
        assert_eq!(result, Some(("echo $(whoami)".to_string(), 17)));
    }

    #[test]
    fn extract_dollar_paren_empty() {
        let result = extract_dollar_paren("$()", 2);
        assert_eq!(result, Some((String::new(), 3)));
    }

    #[test]
    fn extract_dollar_paren_unclosed() {
        let result = extract_dollar_paren("$(echo hi", 2);
        assert_eq!(result, None);
    }

    #[test]
    fn extract_dollar_paren_with_quotes() {
        let result = extract_dollar_paren("$(echo \"hi\")", 2);
        assert_eq!(result, Some(("echo \"hi\"".to_string(), 12)));
    }

    #[test]
    fn extract_dollar_paren_parenthesis_in_quotes() {
        // Closing paren inside quotes should not reduce depth
        let result = extract_dollar_paren("$(echo ')')", 2);
        assert_eq!(result, Some(("echo ')'".to_string(), 11)));
    }

    #[test]
    fn extract_dollar_paren_tracks_double_quotes() {
        // Closing paren inside double quotes should not reduce depth
        let result = extract_dollar_paren("$(echo \"\")", 2);
        assert_eq!(result, Some(("echo \"\"".to_string(), 10)));
    }

    #[test]
    fn extract_dollar_paren_escaped_char() {
        let result = extract_dollar_paren("$(echo \\))", 2);
        assert_eq!(result, Some(("echo \\)".to_string(), 10)));
    }

    // =========================================================================
    // extract_backticks
    // =========================================================================

    #[test]
    fn extract_backticks_simple() {
        let result = extract_backticks("`echo hi`", 1);
        assert_eq!(result, Some(("echo hi".to_string(), 9)));
    }

    #[test]
    fn extract_backticks_empty() {
        let result = extract_backticks("``", 1);
        assert_eq!(result, Some((String::new(), 2)));
    }

    #[test]
    fn extract_backticks_unclosed() {
        let result = extract_backticks("`echo hi", 1);
        assert_eq!(result, None);
    }

    #[test]
    fn extract_backticks_with_escaped_backtick() {
        let result = extract_backticks("`echo \\`hi`", 1);
        assert_eq!(result, Some(("echo \\`hi".to_string(), 11)));
    }

    #[test]
    fn extract_backticks_backslash_escape() {
        let result = extract_backticks("`echo \\\\`", 1);
        // \\\\ = literal backslash followed by closing backtick
        assert_eq!(result, Some(("echo \\\\".to_string(), 9)));
    }

    // =========================================================================
    // extract_command_substitutions
    // =========================================================================

    #[test]
    fn extract_command_substitutions_empty() {
        let result = extract_command_substitutions("");
        assert!(result.is_empty());
    }

    #[test]
    fn extract_command_substitutions_none() {
        let result = extract_command_substitutions("echo hello");
        assert!(result.is_empty());
    }

    #[test]
    fn extract_command_substitutions_dollar_paren() {
        let result = extract_command_substitutions("echo $(whoami)");
        assert_eq!(result, vec!["whoami"]);
    }

    #[test]
    fn extract_command_substitutions_backtick() {
        let result = extract_command_substitutions("echo `whoami`");
        assert_eq!(result, vec!["whoami"]);
    }

    #[test]
    fn extract_command_substitutions_multiple() {
        let mut result = extract_command_substitutions("echo $(a) `b` $(c)");
        result.sort();
        assert_eq!(result, vec!["a", "b", "c"]);
    }

    #[test]
    fn extract_command_substitutions_nested() {
        let result = extract_command_substitutions("echo $(echo $(inner))");
        // Only top-level substitutions are returned; inner is included in outer
        assert_eq!(result, vec!["echo $(inner)"]);
    }

    #[test]
    fn extract_command_substitutions_in_double_quotes() {
        let result = extract_command_substitutions("echo \"$(whoami)\"");
        assert_eq!(result, vec!["whoami"]);
    }

    #[test]
    fn extract_command_substitutions_in_single_quotes_skipped() {
        let result = extract_command_substitutions("echo '$(whoami)'");
        assert!(result.is_empty());
    }

    #[test]
    fn extract_command_substitutions_backtick_in_single_quotes_skipped() {
        let result = extract_command_substitutions("echo '`whoami`'");
        assert!(result.is_empty());
    }

    #[test]
    fn extract_command_substitutions_unclosed_skipped() {
        let result = extract_command_substitutions("echo $(whoami");
        assert!(result.is_empty());
    }

    #[test]
    fn extract_command_substitutions_escaped_char_skips() {
        let result = extract_command_substitutions("echo \\(whoami\\)");
        // Backslash skips the next character, so $( is not formed
        assert!(result.is_empty());
    }

    // =========================================================================
    // strip_shell_data
    // =========================================================================

    #[test]
    fn strip_shell_data_empty() {
        assert_eq!(strip_shell_data(""), "");
    }

    #[test]
    fn strip_shell_data_no_shell_data() {
        assert_eq!(strip_shell_data("git reset --hard"), "git reset --hard");
    }

    #[test]
    fn strip_shell_data_single_quotes_replaced() {
        let result = strip_shell_data("echo 'quoted text'");
        // Characters inside quotes become spaces; quote chars become spaces too
        assert_eq!(result.len(), "echo 'quoted text'".len());
        assert!(result.starts_with("echo "));
        // Everything after "echo " should be spaces
        assert!(result.as_bytes()[5..].iter().all(|&c| c == b' '));
    }

    #[test]
    fn strip_shell_data_double_quotes_replaced() {
        let result = strip_shell_data("echo \"quoted\"");
        // Characters inside quotes become spaces; quote chars become spaces too
        assert_eq!(result.len(), "echo \"quoted\"".len());
        assert!(result.starts_with("echo "));
        assert!(result.as_bytes()[5..].iter().all(|&c| c == b' '));
    }

    #[test]
    fn strip_shell_data_dollar_paren_replaced() {
        let result = strip_shell_data("echo $(whoami)");
        // "$(whoami)" is replaced by a single space; unquoted text preserved
        assert_eq!(result, "echo  ");
    }

    #[test]
    fn strip_shell_data_backtick_replaced() {
        let result = strip_shell_data("echo `whoami`");
        // The entire backtick substitution is replaced by a single space
        assert_eq!(result, "echo  ");
    }

    #[test]
    fn strip_shell_data_unclosed_dollar_paren_preserves_dollar() {
        let result = strip_shell_data("echo $(whoami");
        assert_eq!(result, "echo $(whoami");
    }

    #[test]
    fn strip_shell_data_unclosed_backtick_preserves_backtick() {
        let result = strip_shell_data("echo `whoami");
        assert_eq!(result, "echo `whoami");
    }

    #[test]
    fn strip_shell_data_mixed() {
        let result = strip_shell_data("echo 'single' && cmd \"double\"");
        assert!(result.contains("echo"));
        assert!(result.contains("&&"));
        assert!(result.contains("cmd"));
        // quoted content should be spaces
        assert!(result.len() > "echo  && cmd ".len());
    }

    #[test]
    fn strip_shell_data_escaped_in_double_quotes() {
        let result = strip_shell_data("echo \"foo\\\"bar\"");
        // Each char inside double quotes becomes a space
        assert_eq!(result.len(), "echo \"foo\\\"bar\"".len());
        assert!(result.starts_with("echo "));
        assert!(result.as_bytes()[5..].iter().all(|&c| c == b' '));
    }

    #[test]
    fn strip_shell_data_alternating_quotes() {
        let result = strip_shell_data("a'b'c\"d\"e");
        assert_eq!(result.len(), "a'b'c\"d\"e".len());
        // "a", "c", "e" preserved; other positions are spaces
        assert_eq!(result.as_bytes()[0], b'a');
        assert_eq!(result.as_bytes()[4], b'c');
        assert_eq!(result.as_bytes()[8], b'e');
    }
}
