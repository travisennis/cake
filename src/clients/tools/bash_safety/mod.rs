// Best-effort destructive command guard for the Bash tool.
//
// Blocks known-destructive commands that operate within the sandbox's allowed
// zone (e.g. destructive git operations inside the repo) or affect remote
// state (e.g. force-push). This is a best-effort guard, not a security
// boundary or shell policy engine. The OS-level sandbox remains the primary
// filesystem enforcement layer.

mod checks;
mod parse;

// =============================================================================
// Public API
// =============================================================================

/// Validate that a command does not contain known-destructive operations.
/// Returns `Ok(())` if safe, or `Err(formatted_message)` if blocked.
pub(super) fn validate_command_safety(command: &str) -> Result<(), String> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    // Best-effort split on documented separators outside simple quotes.
    for segment in parse::split_segments(trimmed) {
        let seg = segment.trim();
        if seg.is_empty() {
            continue;
        }

        // Check documented shell -c wrappers and recurse into the inner script.
        if let Some(inner) = parse::extract_inline_script(seg) {
            validate_command_safety(&inner)?;
            continue;
        }

        for substitution in parse::extract_command_substitutions(seg) {
            validate_command_safety(&substitution)?;
        }

        let inspection_segment = parse::strip_shell_data(seg);
        let normalized = parse::normalize_whitespace(&inspection_segment);
        let lower = normalized.to_lowercase();

        checks::check_git_reset(&lower, seg)?;
        checks::check_git_checkout(&lower, seg)?;
        checks::check_git_restore(&lower, seg)?;
        checks::check_git_clean(&lower, seg)?;
        checks::check_git_push(&lower, seg)?;
        checks::check_git_branch_delete(&normalized, seg)?;
        checks::check_git_stash(&lower, seg)?;
        checks::check_git_commit_backticks(seg)?;
        checks::check_dangerous_rm(&normalized, seg)?;
    }

    Ok(())
}

// =============================================================================
// Error Formatting
// =============================================================================

/// Format a blocked-command error message.
pub(super) fn blocked(reason: &str, command: &str, tip: &str) -> String {
    format!("BLOCKED\n\nReason: {reason}\n\nCommand: {command}\n\nTip: {tip}")
}
