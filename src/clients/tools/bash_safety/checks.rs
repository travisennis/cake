// Destructive-command check rules for the Bash tool safety guard.
//
// Each check function follows a uniform signature:
//   fn(segment, normalized, lower) -> Option<String>
//
// `segment` is the original segment text, `normalized` is the whitespace-
// collapsed inspection segment (case preserved), and `lower` is the
// lowercased normalized text.
//
// Returns `Some(message)` when the check triggers, `None` when it passes.
//
// This module contains no parsing logic; see `parse.rs` for that.

use super::{blocked, warned};

// =============================================================================
// Check function type (public for use in the CHECKS registry)
// =============================================================================

pub(super) type CheckFn = fn(segment: &str, normalized: &str, lower: &str) -> Option<String>;

// =============================================================================
// Git Checks
// =============================================================================

/// `git reset --hard` / `git reset --merge`
pub(super) fn check_git_reset(_segment: &str, _normalized: &str, lower: &str) -> Option<String> {
    if lower.contains("git reset --hard") || lower.contains("git reset --merge") {
        return Some(blocked(
            "git reset --hard/--merge destroys uncommitted changes",
            "Use 'git stash' to save changes first, or 'git reset --soft' to preserve them.",
        ));
    }
    None
}

/// `git checkout -- <file>`
pub(super) fn check_git_checkout(_segment: &str, _normalized: &str, lower: &str) -> Option<String> {
    if lower.contains("git checkout --") {
        // Verify there's a path after `--`
        if let Some(pos) = lower.find("git checkout --") {
            let after = lower.get(pos + "git checkout --".len()..)?;
            let after = after.trim();
            if !after.is_empty() && !after.starts_with('-') {
                return Some(blocked(
                    "git checkout -- <file> discards uncommitted file changes",
                    "Use 'git restore --staged <file>' to unstage, or 'git stash' to save changes.",
                ));
            }
        }
    }
    None
}

/// `git restore <file>` without `--staged`, or with `--worktree`
/// `git restore -b <branch>` is allowed (creates a branch).
pub(super) fn check_git_restore(_segment: &str, _normalized: &str, lower: &str) -> Option<String> {
    if !lower.contains("git restore") {
        return None;
    }

    // --worktree is always destructive, even with --staged
    if lower.contains("--worktree") {
        return Some(blocked(
            "git restore --worktree discards uncommitted changes",
            "Use 'git restore --staged <file>' to only unstage, or 'git stash' to save changes.",
        ));
    }

    // --staged alone is safe (only unstages)
    if lower.contains("--staged") {
        return None;
    }

    // Find what comes after `git restore`
    if let Some(pos) = lower.find("git restore") {
        let after = lower.get(pos + "git restore".len()..)?;
        let after = after.trim();
        // -b creates a branch, not destructive
        if after.is_empty() || after.starts_with("-b") || after.starts_with("-b ") {
            return None;
        }
        // Bare `git restore <file>` without --staged is destructive
        if !after.is_empty() {
            return Some(blocked(
                "git restore <file> without --staged discards uncommitted changes",
                "Use 'git restore --staged <file>' to only unstage, or 'git stash' to save changes.",
            ));
        }
    }

    None
}

/// `git clean -f` / `--force` — includes combined flags like `-fd`, `-fdx`
pub(super) fn check_git_clean(_segment: &str, _normalized: &str, lower: &str) -> Option<String> {
    if !lower.contains("git clean") {
        return None;
    }

    if lower.contains("git clean --force") || lower.contains("git clean -f") {
        return Some(blocked(
            "git clean -f permanently deletes untracked files",
            "Use 'git clean -n' to preview what would be deleted first.",
        ));
    }

    // Check for combined flags containing 'f', e.g. `-fd`, `-xfd`, `-fdx`
    if let Some(pos) = lower.find("git clean") {
        let after = lower.get(pos + "git clean".len()..)?;
        let after = after.trim();
        // Look for a dash-flag group containing 'f'
        for token in after.split_whitespace() {
            if token.starts_with('-') && !token.starts_with("--") && token.contains('f') {
                return Some(blocked(
                    "git clean -f permanently deletes untracked files",
                    "Use 'git clean -n' to preview what would be deleted first.",
                ));
            }
        }
    }

    None
}

/// `git push --force` / `-f` — allows `--force-with-lease`
pub(super) fn check_git_push(_segment: &str, _normalized: &str, lower: &str) -> Option<String> {
    if !lower.contains("git push") {
        return None;
    }

    // --force-with-lease is safe
    if lower.contains("--force-with-lease") {
        return None;
    }

    if lower.contains("git push --force") {
        return Some(blocked(
            "git push --force overwrites remote commit history",
            "Use 'git push --force-with-lease' for safer force pushes.",
        ));
    }

    // Check for short flag -f (but not part of a longer flag group that isn't force)
    if let Some(pos) = lower.find("git push") {
        let after = lower.get(pos + "git push".len()..)?;
        let after = after.trim();
        for token in after.split_whitespace() {
            if token == "-f" {
                return Some(blocked(
                    "git push -f overwrites remote commit history",
                    "Use 'git push --force-with-lease' for safer force pushes.",
                ));
            }
            // Combined flags like -fu, -uf
            if token.starts_with('-')
                && !token.starts_with("--")
                && token.len() > 1
                && token.contains('f')
            {
                return Some(blocked(
                    "git push -f overwrites remote commit history",
                    "Use 'git push --force-with-lease' for safer force pushes.",
                ));
            }
        }
    }

    None
}

/// `git branch -D` — uppercase D only (force delete without merge check).
/// Uses the original (case-preserved, whitespace-normalized) string.
pub(super) fn check_git_branch_delete(
    _segment: &str,
    normalized: &str,
    _lower: &str,
) -> Option<String> {
    // Match "git branch" case-insensitively, then inspect the original flag
    // spelling so lowercase `-d` remains allowed while uppercase `-D` blocks.
    let mut tokens = normalized.split_whitespace();
    while let Some(token) = tokens.next() {
        if !token.eq_ignore_ascii_case("git") {
            continue;
        }

        if !tokens
            .next()
            .is_some_and(|token| token.eq_ignore_ascii_case("branch"))
        {
            continue;
        }

        if let Some(flags) = tokens
            .next()
            .and_then(|token| token.strip_prefix('-'))
            .filter(|flags| !flags.starts_with('-'))
            && flags.chars().any(|c| c == 'D')
        {
            return Some(blocked(
                "git branch -D force-deletes branches without checking merge status",
                "Use 'git branch -d' (lowercase) to safely delete only merged branches.",
            ));
        }
    }

    None
}

/// `git stash drop` / `git stash clear`
pub(super) fn check_git_stash(_segment: &str, _normalized: &str, lower: &str) -> Option<String> {
    if lower.contains("git stash drop") || lower.contains("git stash clear") {
        return Some(blocked(
            "git stash drop/clear permanently deletes stashed changes",
            "Use 'git stash list' to review stashes, or 'git stash pop' to apply and remove.",
        ));
    }
    None
}

/// `git commit -m` with backticks or `$()` inside double-quoted message.
///
/// Shell interprets backticks and `$()` as command substitution even inside
/// double-quoted arguments passed to `-m`/`--message`. This blocks such calls
/// and tells the agent to use `git commit -F -` with a heredoc, or single
/// quotes around the message instead.
pub(super) fn check_git_commit_backticks(
    segment: &str,
    _normalized: &str,
    _lower: &str,
) -> Option<String> {
    // Quick bail-out: if there's no git commit, skip.
    if !segment.contains("git commit") {
        return None;
    }

    let bytes = segment.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // Find the start of a "git commit" token pair.
        // Skip past whitespace to find "git" then check the next token is "commit".
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len {
            break;
        }
        let word_start = i;
        while i < len && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let git_end = i;
        // SAFETY: word_start..git_end spans ASCII whitespace-delimited bytes,
        // which are valid UTF-8 by construction.
        let git_word = std::str::from_utf8(&bytes[word_start..git_end]).unwrap_or("");
        if git_word.eq_ignore_ascii_case("git") {
            // Find the next non-whitespace token
            while i < len && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let commit_start = i;
            while i < len && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let commit_end = i;
            if commit_start < len {
                // SAFETY: commit_start..commit_end spans ASCII bytes, safe.
                let commit_word =
                    std::str::from_utf8(&bytes[commit_start..commit_end]).unwrap_or("");
                if commit_word.eq_ignore_ascii_case("commit") {
                    // Found "git commit", now scan for -m/--message flags with
                    // double-quoted values containing backticks or $(.
                    if has_unsafe_message_flag(segment, i) {
                        return Some(blocked(
                            "git commit -m/--message with backticks or $() in double-quoted message",
                            "Use 'git commit -F -' with a heredoc to pass the message via stdin, \
                             or use single quotes around the message instead of double quotes.",
                        ));
                    }
                }
            }
        }
    }

    None
}

/// Scan the remainder of a command (starting at `start`) for `-m`/`--message`
/// flags whose double-quoted value contains backticks or `$(`.
///
/// Handles both `-m "..."` (space-separated) and `-m="..."` / `-m"..."`
/// (no space / equals sign) forms. Respects single-quote contexts where
/// backticks are literal and harmless.
fn has_unsafe_message_flag(command: &str, start: usize) -> bool {
    let bytes = command.as_bytes();
    let len = bytes.len();
    let mut i = start;

    while i < len {
        // Skip whitespace
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len {
            return false;
        }

        // Check if we're entering a single-quoted region — everything inside
        // is literal data, not evaluated by the shell.
        if bytes[i] == b'\'' {
            i += 1;
            while i < len && bytes[i] != b'\'' {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if i < len {
                i += 1; // skip closing quote
            }
            continue;
        }

        // Use byte-based checks to avoid clippy string-slice warnings.
        // Check if current position starts with -m (not preceded by another dash,
        // so we don't match --message here).
        let starts_with_m_flag = i + 1 < len && bytes[i] == b'-' && bytes[i + 1] == b'm';
        let starts_with_message_flag = i + 8 < len
            && bytes[i] == b'-'
            && bytes[i + 1] == b'-'
            && bytes[i + 2..i + 9].eq(b"message");

        if !starts_with_m_flag && !starts_with_message_flag {
            // Skip this token
            while i < len && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            continue;
        }

        let flag_len: usize = if starts_with_m_flag { 2 } else { 9 };
        i += flag_len;

        // Skip optional whitespace before the message value
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }

        if i >= len {
            return false;
        }

        // Handle -m="value" or --message="value"
        if bytes[i] == b'=' {
            i += 1;
            while i < len && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
        }

        // Handle -m"value" (no space, no equals)
        if i < len && bytes[i] == b'"' {
            // Scan inside the double-quoted string for backticks or $(
            i += 1; // skip opening quote
            while i < len && bytes[i] != b'"' {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2; // skip escaped character
                    continue;
                }
                // Check for backtick or $(
                if bytes[i] == b'`' {
                    return true;
                }
                if bytes[i] == b'$' && i + 1 < len && bytes[i + 1] == b'(' {
                    return true;
                }
                i += 1;
            }
            if i < len {
                i += 1; // skip closing quote
            }
            // If we get here, there was no double-quoted value after -m/--message.
            // Continue scanning for more flags.
        }
    }

    false
}

// =============================================================================
// rm -rf Check
// =============================================================================

/// Block `rm -rf` targeting obviously dangerous paths.
/// Allowed: `/tmp/*`, `/var/tmp/*`.
pub(super) fn check_dangerous_rm(_segment: &str, normalized: &str, _lower: &str) -> Option<String> {
    let lower = normalized.to_lowercase();
    if !lower.contains("rm ") {
        return None;
    }

    // Find `rm` invocations with recursive + force flags (including combined flags)
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    for (i, &tok) in tokens.iter().enumerate() {
        if tok.eq_ignore_ascii_case("rm") {
            let mut has_r = false;
            let mut has_f = false;
            let mut targets = Vec::new();
            let mut options_ended = false;

            for &arg in tokens.iter().skip(i + 1) {
                if !options_ended && arg == "--" {
                    options_ended = true;
                    continue;
                }

                if !options_ended && arg.starts_with("--") {
                    match arg {
                        "--recursive" => has_r = true,
                        "--force" => has_f = true,
                        _ => {},
                    }
                    continue;
                }

                if !options_ended && let Some(flags) = arg.strip_prefix('-') {
                    if flags.contains('r') || flags.contains('R') {
                        has_r = true;
                    }
                    if flags.contains('f') || flags.contains('F') {
                        has_f = true;
                    }
                    continue;
                }

                targets.push(arg);
            }

            if has_r && has_f {
                for target in targets {
                    if !is_allowed_rm_target(target) {
                        return Some(blocked(
                            "rm -rf outside of temporary directories can cause permanent data loss",
                            "rm -rf is only allowed for literal /tmp/* or /var/tmp/* paths.",
                        ));
                    }
                }
            }
        }
    }

    None
}

/// Check if an `rm -rf` target is in an allowed temporary directory.
fn is_allowed_rm_target(target: &str) -> bool {
    let allowed_prefixes = ["/tmp", "/var/tmp"];

    for prefix in &allowed_prefixes {
        if target == *prefix || target.starts_with(&format!("{prefix}/")) {
            return true;
        }
    }

    false
}

// =============================================================================
// rg -rn / ripgrep replace-flag footgun (Soft Warning)
// =============================================================================

/// Detect `rg -rn` where `-rn` sets the replacement string to "n".
///
/// The `-r`/`--replace` flag takes the next argument as the replacement
/// string, so `rg -rn pattern` replaces matches with the literal character
/// "n" and searches for `pattern`. This is almost certainly a mistake where
/// `rg -n` (show line numbers) was intended.
///
/// This is a soft warning — the command still executes, but the agent is
/// alerted that the output may not be what they expect.
pub(super) fn check_rg_replace_flag(
    segment: &str,
    _normalized: &str,
    _lower: &str,
) -> Option<String> {
    // Quick bail-out: no `rg` invocation.
    if !segment.contains("rg") {
        return None;
    }

    let bytes = segment.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // Find "rg" as a whitespace-delimited token.
        while i < len && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= len {
            break;
        }
        let token_start = i;
        while i < len && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let token_end = i;

        // SAFETY: token_start..token_end spans ASCII whitespace-delimited bytes.
        let token = std::str::from_utf8(&bytes[token_start..token_end]).unwrap_or("");
        if token != "rg" {
            continue;
        }

        // Found "rg" — scan subsequent tokens for -rn or -r followed by n.
        let mut prev_was_dash_r = false;
        while i < len {
            while i < len && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= len {
                break;
            }

            // If the next token starts with '-', it's a flag.
            if bytes[i] == b'-' {
                let flag_start = i;
                while i < len && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                let flag_end = i;

                let flag = std::str::from_utf8(&bytes[flag_start..flag_end]).unwrap_or("");

                // Combined short flag where n immediately follows r, e.g. -rn.
                // -nr (n before r) is a different failure mode — ripgrep errors
                // that -r requires a value, rather than silently replacing with "n".
                if flag.starts_with('-') && !flag.starts_with("--") && flag.contains("rn") {
                    return Some(warned(
                        "rg -rn sets the replacement string to \"n\". \
                         Did you mean `rg -n` (line numbers)?",
                        "The -r/--replace flag in ripgrep takes the next argument as the \
                         replacement string. `rg -rn pattern` replaces matches with \"n\". \
                         Use `rg -n` to show line numbers instead.",
                    ));
                }

                // Track bare -r so we can catch -r followed by n as separate arg
                prev_was_dash_r = flag == "-r" || flag == "--replace";

                // Pattern or path — flags end here.
                if flag == "--" {
                    break;
                }

                continue;
            }

            // Not a flag — could be a positional arg (pattern, path, or replacement).
            if prev_was_dash_r {
                let arg_start = i;
                while i < len && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                let arg_end = i;

                let arg = std::str::from_utf8(&bytes[arg_start..arg_end]).unwrap_or("");
                // If the argument after -r is exactly "n", it's suspicious.
                if arg == "n" {
                    return Some(warned(
                        "rg -r n sets the replacement string to \"n\". \
                         Did you mean `rg -n` (line numbers)?",
                        "The -r/--replace flag in ripgrep takes the next argument as the \
                         replacement string. `rg -r n pattern` replaces matches with \"n\". \
                         Use `rg -n` to show line numbers instead.",
                    ));
                }
                prev_was_dash_r = false;
                continue;
            }

            // Regular positional arg — skip it.
            while i < len && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            prev_was_dash_r = false;
        }
    }

    None
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // Shorthand: collect all check results for a command
    // Returns (warnings, blocks). Only the first block is captured since
    // validation stops at the first hard block.
    fn collect_results(command: &str) -> (Vec<String>, Vec<String>) {
        match super::super::validate_command_safety(command) {
            Ok(warnings) => (warnings, Vec::new()),
            Err(block) => (Vec::new(), vec![block]),
        }
    }

    // Shorthand: assert command is blocked
    fn assert_blocked(command: &str) {
        let (warnings, blocks) = collect_results(command);
        assert!(
            !blocks.is_empty(),
            "Expected command to be BLOCKED: {command}"
        );
        // Warnings may also be present alongside blocks, that's fine.
        _ = warnings;
    }

    // Shorthand: assert command is allowed (no blocks)
    fn assert_allowed(command: &str) {
        match super::super::validate_command_safety(command) {
            Ok(_) => {}, // OK, may have warnings
            Err(block) => {
                panic!("Expected command to be ALLOWED: {command}\nGot block: {block}");
            },
        }
    }

    // Shorthand: assert command produces a soft warning
    fn assert_warned(command: &str) {
        let (warnings, blocks) = collect_results(command);
        assert!(
            !warnings.is_empty(),
            "Expected command to produce WARNING: {command}"
        );
        // No blocks expected for soft-warning-only commands.
        assert!(
            blocks.is_empty(),
            "Expected no blocks for warning-only command: {command}\nGot: {blocks:?}"
        );
    }

    // =========================================================================
    // git reset
    // =========================================================================

    #[test]
    fn blocks_git_reset_hard() {
        assert_blocked("git reset --hard");
        assert_blocked("git reset --hard HEAD");
        assert_blocked("git reset --hard HEAD~3");
        assert_blocked("GIT RESET --HARD");
        assert_blocked("Git Reset --Hard");
    }

    #[test]
    fn blocks_git_reset_merge() {
        assert_blocked("git reset --merge");
    }

    #[test]
    fn allows_git_reset_soft() {
        assert_allowed("git reset --soft HEAD~1");
        assert_allowed("git reset HEAD~1");
    }

    // =========================================================================
    // git checkout
    // =========================================================================

    #[test]
    fn blocks_git_checkout_dash_dash_file() {
        assert_blocked("git checkout -- src/main.rs");
        assert_blocked("git checkout -- .");
    }

    #[test]
    fn allows_git_checkout_branch() {
        assert_allowed("git checkout main");
        assert_allowed("git checkout -b new-branch");
    }

    // =========================================================================
    // git restore
    // =========================================================================

    #[test]
    fn blocks_git_restore_file() {
        assert_blocked("git restore src/main.rs");
        assert_blocked("git restore .");
    }

    #[test]
    fn blocks_git_restore_worktree() {
        assert_blocked("git restore --worktree src/main.rs");
        assert_blocked("git restore --staged --worktree src/main.rs");
    }

    #[test]
    fn allows_git_restore_staged() {
        assert_allowed("git restore --staged src/main.rs");
    }

    #[test]
    fn allows_git_restore_branch() {
        assert_allowed("git restore -b new-branch");
    }

    // =========================================================================
    // git clean
    // =========================================================================

    #[test]
    fn blocks_git_clean_force() {
        assert_blocked("git clean -f");
        assert_blocked("git clean --force");
        assert_blocked("git clean -fd");
        assert_blocked("git clean -fdx");
        assert_blocked("git clean -xf");
    }

    #[test]
    fn allows_git_clean_dry_run() {
        assert_allowed("git clean -n");
        assert_allowed("git clean -nd");
    }

    // =========================================================================
    // git push
    // =========================================================================

    #[test]
    fn blocks_git_push_force() {
        assert_blocked("git push --force");
        assert_blocked("git push --force origin main");
        assert_blocked("git push -f");
        assert_blocked("git push -f origin main");
        assert_blocked("git push -fu origin main");
    }

    #[test]
    fn allows_git_push_force_with_lease() {
        assert_allowed("git push --force-with-lease");
        assert_allowed("git push --force-with-lease origin main");
    }

    #[test]
    fn allows_normal_git_push() {
        assert_allowed("git push");
        assert_allowed("git push origin main");
    }

    // =========================================================================
    // git branch -D
    // =========================================================================

    #[test]
    fn blocks_git_branch_uppercase_d() {
        assert_blocked("git branch -D feature-branch");
    }

    #[test]
    fn allows_git_branch_lowercase_d() {
        assert_allowed("git branch -d feature-branch");
    }

    // =========================================================================
    // git stash
    // =========================================================================

    #[test]
    fn blocks_git_stash_drop() {
        assert_blocked("git stash drop");
        assert_blocked("git stash drop stash@{0}");
    }

    #[test]
    fn blocks_git_stash_clear() {
        assert_blocked("git stash clear");
    }

    #[test]
    fn allows_git_stash_other() {
        assert_allowed("git stash");
        assert_allowed("git stash pop");
        assert_allowed("git stash list");
        assert_allowed("git stash apply");
    }

    // =========================================================================
    // rm -rf
    // =========================================================================

    // =========================================================================
    // is_allowed_rm_target (unit-level)
    // =========================================================================

    #[test]
    fn allowed_rm_target_accepts_tmp_variants() {
        assert!(is_allowed_rm_target("/tmp"));
        assert!(is_allowed_rm_target("/tmp/"));
        assert!(is_allowed_rm_target("/tmp/build-cache"));
        assert!(is_allowed_rm_target("/var/tmp"));
        assert!(is_allowed_rm_target("/var/tmp/"));
        assert!(is_allowed_rm_target("/var/tmp/test"));
    }

    #[test]
    fn allowed_rm_target_rejects_non_tmp() {
        assert!(!is_allowed_rm_target("/home/user"));
        assert!(!is_allowed_rm_target("/usr"));
        assert!(!is_allowed_rm_target("."));
        assert!(!is_allowed_rm_target(".."));
        assert!(!is_allowed_rm_target("/tmpsomething"));
        assert!(!is_allowed_rm_target("/var/tmpsomething"));
    }

    #[test]
    fn blocks_rm_rf_dangerous_targets() {
        assert_blocked("rm -rf /");
        assert_blocked("rm -rf /home/user");
        assert_blocked("rm -rf ~/projects");
        assert_blocked("rm -rf .");
        assert_blocked("rm -rf ..");
        assert_blocked("rm -rf /usr");
        assert_blocked("rm -rf /etc");
    }

    #[test]
    fn blocks_rm_rf_when_any_target_is_dangerous() {
        assert_blocked("rm -rf /tmp/build-cache ~/projects");
        assert_blocked("rm -rf /tmp/build-cache /home/user");
        assert_blocked("rm -r -f /tmp/build-cache ..");
        assert_blocked("rm -rf /tmp/build-cache -- ~/projects");
    }

    #[test]
    fn blocks_rm_rf_shell_variable_temp_targets() {
        assert_blocked("rm -rf $TMPDIR/foo");
        assert_blocked("rm -rf ${TMPDIR}/bar");
        assert_blocked("rm -rf $TMPDIR");
        assert_blocked("rm -rf ${TMPDIR}");
        assert_blocked("rm -rf /tmp/build-cache $TMPDIR/foo");
        assert_blocked("rm -rf /tmp/build-cache ${TMPDIR}/bar");
    }

    #[test]
    fn allows_rm_rf_temp_dirs() {
        assert_allowed("rm -rf /tmp/build-cache");
        assert_allowed("rm -rf /var/tmp/test");
    }

    #[test]
    fn allows_rm_rf_multiple_temp_targets() {
        assert_allowed("rm -rf /tmp/build-cache /var/tmp/test");
        assert_allowed("rm -rf -- /tmp/build-cache /var/tmp/test");
    }

    #[test]
    fn allows_rm_without_rf() {
        assert_allowed("rm file.txt");
        assert_allowed("rm -r dir/");
        assert_allowed("rm -f file.txt");
    }

    // =========================================================================
    // Inline scripts (bash -c / sh -c)
    // =========================================================================

    #[test]
    fn blocks_inline_script_with_destructive_command() {
        assert_blocked("bash -c \"git reset --hard\"");
        assert_blocked("sh -c \"git clean -f\"");
        assert_blocked("bash -c 'git push --force origin main'");
    }

    #[test]
    fn blocks_inline_script_with_extra_whitespace() {
        assert_blocked("bash  -c \"git reset --hard\"");
        assert_blocked("bash\t-c \"git reset --hard\"");
    }

    #[test]
    fn blocks_destructive_after_inline_script() {
        assert_blocked("bash -c 'echo safe' ; rm -rf /");
        assert_blocked("bash -c 'echo safe' && git reset --hard");
    }

    #[test]
    fn allows_inline_script_with_safe_command() {
        assert_allowed("bash -c \"echo hello\"");
        assert_allowed("sh -c \"git status\"");
    }

    #[test]
    fn unsupported_inline_script_forms_are_out_of_scope() {
        assert_allowed("zsh -c 'git reset --hard'");
        assert_allowed("sh -lc 'git clean -f'");
    }

    // =========================================================================
    // Command chaining
    // =========================================================================

    #[test]
    fn blocks_destructive_in_chain() {
        assert_blocked("echo done && git reset --hard");
        assert_blocked("git add . && git reset --hard HEAD~1");
        assert_blocked("git stash; git reset --hard; git stash pop");
    }

    #[test]
    fn blocks_destructive_via_pipe() {
        // Pipe splits segments so each side is checked independently
        assert_blocked("git status | tee log.txt && git reset --hard");
        assert_blocked("git log | head -5 ; rm -rf /");
    }

    #[test]
    fn allows_safe_chain() {
        assert_allowed("git add . && git commit -m 'test'");
        assert_allowed("cargo build && cargo test");
    }

    // =========================================================================
    // False positive avoidance
    // =========================================================================

    #[test]
    fn allows_commit_message_containing_destructive_text() {
        assert_allowed("git commit -m \"fix: handle git branch -D correctly\"");
        assert_allowed("git commit -m \"docs: explain git reset --hard\"");
    }

    #[test]
    fn allows_echo_containing_destructive_text() {
        assert_allowed("echo \"git reset --hard\"");
    }

    #[test]
    fn blocks_destructive_commands_inside_data_context_substitutions() {
        assert_blocked("echo \"$(git reset --hard)\"");
        assert_blocked("printf '%s\\n' \"$(git clean -fd)\"");
        assert_blocked("cat > out.txt <<EOF\n$(rm -rf /)\nEOF");
        assert_blocked("git commit -m \"$(git branch -D feature)\"");
        assert_blocked("git tag -a v1 -m \"$(git stash clear)\"");
        assert_blocked("git status | tee \"$(git reset --hard)\"");
    }

    #[test]
    fn blocks_destructive_commands_inside_backtick_substitutions() {
        assert_blocked("echo `git reset --hard`");
        assert_blocked("git commit -m \"`rm -rf /`\"");
    }

    #[test]
    fn allows_single_quoted_substitution_text_as_literal_data() {
        assert_allowed("echo '$(git reset --hard)'");
        assert_allowed("git commit -m '$(git clean -fd)'");
    }

    #[test]
    fn allows_quoted_separators_in_commit_messages() {
        assert_allowed("git commit -m \"fix: updated stuff ; git reset --hard\"");
        assert_allowed("git commit -m 'refactor && git clean -f'");
    }

    // =========================================================================
    // git commit -m backtick / $() safeguard
    // =========================================================================

    #[test]
    fn blocks_commit_message_with_backticks_in_double_quotes() {
        assert_blocked("git commit -m \"feat: add `Runtime::new()` support\"");
        assert_blocked("git commit -m \"fix: handle `$var` weird case\"");
    }

    #[test]
    fn blocks_commit_message_with_dollar_paren_in_double_quotes() {
        assert_blocked("git commit -m \"refactor: use $(some_command) result\"");
    }

    #[test]
    fn blocks_commit_message_with_backticks_via_message_flag() {
        assert_blocked("git commit --message \"docs: updated `function_name` docs\"");
    }

    #[test]
    fn blocks_commit_message_backticks_with_equal_sign() {
        assert_blocked("git commit -m=\"feat: add `new_feature`\"");
        assert_blocked("git commit --message=\"fix: handle `edge_case`\"");
    }

    #[test]
    fn allows_commit_message_with_backticks_in_single_quotes() {
        assert_allowed("git commit -m 'feat: add `Runtime::new()` support'");
    }

    #[test]
    fn allows_commit_message_without_backticks_or_dollar_paren() {
        assert_allowed("git commit -m \"feat: add Runtime::new() support\"");
        assert_allowed("git commit --message \"fix: handle edge case\"");
    }

    #[test]
    fn allows_git_commands_unrelated_to_commit() {
        assert_allowed("git add .");
        assert_allowed("git log");
        assert_allowed("git diff");
    }

    #[test]
    fn blocks_commit_message_dollar_paren_with_equal_sign() {
        assert_blocked("git commit -m=\"use $(pwd) for path\"");
        assert_blocked("git commit --message=\"use $(hostname) for host\"");
    }

    // =========================================================================
    // rg -rn / ripgrep replace-flag footgun (Soft Warning)
    // =========================================================================

    #[test]
    fn warns_rg_rn_combined_flag() {
        assert_warned("rg -rn pattern");
        assert_warned("rg -rn 'some pattern'");
        assert_warned("rg -rn 'pattern' src/");
    }

    #[test]
    fn warns_rg_r_with_n_as_separate_arg() {
        assert_warned("rg -r n pattern");
        assert_warned("rg --replace n pattern");
    }

    #[test]
    fn allows_rg_n_line_numbers() {
        assert_allowed("rg -n pattern");
        assert_allowed("rg -n 'some pattern' src/");
    }

    #[test]
    fn allows_rg_without_n_flag() {
        assert_allowed("rg pattern");
        assert_allowed("rg -i pattern");
        assert_allowed("rg -l pattern");
    }

    #[test]
    fn allows_rg_with_r_replace_not_n() {
        // rg -r 'replacement' is valid intentional usage
        assert_allowed("rg -r 'replacement' pattern");
        assert_allowed("rg --replace 'replacement' pattern");
    }

    #[test]
    fn warns_rg_rn_with_n_immediately_after_r() {
        // -rn sets replacement to "n" — the footgun.
        assert_warned("rg -rn pattern");
    }

    #[test]
    fn allows_rg_nr_flag() {
        // -nr means -n -r with no replacement arg. Ripgrep errors:
        //   error: The argument '--replace <ARG> ...' requires a value but none was supplied
        // This is a different failure mode, not the silent-replace footgun.
        assert_allowed("rg -nr pattern");
    }

    #[test]
    fn warns_rg_rn_in_chain() {
        assert_warned("echo test && rg -rn pattern");
    }

    #[test]
    fn allows_rg_normal_usage() {
        assert_allowed("rg TODO src/");
        assert_allowed("rg --type rust fn main");
        assert_allowed("rg -C 3 pattern");
    }

    // =========================================================================
    // Whitespace variations
    // =========================================================================

    #[test]
    fn handles_extra_whitespace() {
        assert_blocked("git  reset  --hard");
        assert_blocked("git\treset\t--hard");
    }

    #[test]
    fn handles_unicode_text_before_destructive_segments() {
        assert_blocked("echo préfix ; git reset --hard");
        assert_blocked("echo préfix && git branch -D feature-branch");
    }

    // =========================================================================
    // Error message format
    // =========================================================================

    #[test]
    fn error_message_contains_reason_and_tip() {
        let Err(err) = super::super::validate_command_safety("git reset --hard") else {
            panic!("should be blocked");
        };
        assert!(err.contains("BLOCKED"), "Missing BLOCKED header");
        assert!(err.contains("Reason:"), "Missing reason");
        assert!(err.contains("Tip:"), "Missing tip");
    }

    // =========================================================================
    // Warning message format
    // =========================================================================

    #[test]
    fn warning_message_contains_notice_and_tip() {
        let (warnings, _blocks) = collect_results("rg -rn pattern");
        assert!(!warnings.is_empty(), "Expected warning for rg -rn");
        let warning = &warnings[0];
        assert!(
            warning.contains("NOTICE"),
            "Missing NOTICE header in: {warning}"
        );
        assert!(
            warning.contains("Did you mean"),
            "Missing guidance in: {warning}"
        );
        assert!(warning.contains("Tip:"), "Missing tip in: {warning}");
    }
}
