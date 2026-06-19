// Best-effort destructive command guard for the Bash tool.
//
// Blocks known-destructive commands that operate within the sandbox's allowed
// zone (e.g. destructive git operations inside the repo) or affect remote
// state (e.g. force-push). Produces soft warnings for suspicious-but-not-
// destructive patterns (e.g. `rg -rn` footgun).
//
// This is a best-effort guard, not a security boundary or shell policy
// engine. The OS-level sandbox remains the primary filesystem enforcement
// layer.

mod checks;
mod parse;

// =============================================================================
// Check Registry
// =============================================================================

/// Severity of a safety check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CheckSeverity {
    /// Command is blocked; execution stops.
    HardBlock,
    /// Command proceeds; a warning is shown to the agent.
    SoftWarning,
}

/// A named safety check registered in the guard.
///
/// The `check` function takes `(segment, normalized, lower)` where:
/// - `segment`: original segment text (before shell-data stripping)
/// - `normalized`: whitespace-collapsed inspection segment (case preserved)
/// - `lower`: lowercased normalized text
///
/// Returns `Some(message)` when the check triggers, `None` when it passes.
struct CheckDef {
    #[expect(
        dead_code,
        reason = "name is reserved for future introspection of the check registry"
    )]
    name: &'static str,
    severity: CheckSeverity,
    check: checks::CheckFn,
}

/// Enumerated list of all safety checks.
///
/// New checks are added here and automatically participate in
/// `validate_command_safety` without modifying the main validation loop.
const CHECKS: &[CheckDef] = &[
    CheckDef {
        name: "git_reset",
        severity: CheckSeverity::HardBlock,
        check: checks::check_git_reset,
    },
    CheckDef {
        name: "git_checkout",
        severity: CheckSeverity::HardBlock,
        check: checks::check_git_checkout,
    },
    CheckDef {
        name: "git_restore",
        severity: CheckSeverity::HardBlock,
        check: checks::check_git_restore,
    },
    CheckDef {
        name: "git_clean",
        severity: CheckSeverity::HardBlock,
        check: checks::check_git_clean,
    },
    CheckDef {
        name: "git_push",
        severity: CheckSeverity::HardBlock,
        check: checks::check_git_push,
    },
    CheckDef {
        name: "git_branch_delete",
        severity: CheckSeverity::HardBlock,
        check: checks::check_git_branch_delete,
    },
    CheckDef {
        name: "git_stash",
        severity: CheckSeverity::HardBlock,
        check: checks::check_git_stash,
    },
    CheckDef {
        name: "git_commit_backticks",
        severity: CheckSeverity::HardBlock,
        check: checks::check_git_commit_backticks,
    },
    CheckDef {
        name: "dangerous_rm",
        severity: CheckSeverity::HardBlock,
        check: checks::check_dangerous_rm,
    },
    CheckDef {
        name: "rg_replace_flag",
        severity: CheckSeverity::SoftWarning,
        check: checks::check_rg_replace_flag,
    },
];

// =============================================================================
// Public API
// =============================================================================

/// Validate that a command does not contain known-destructive operations.
///
/// Returns `Ok(warnings)` if safe (warnings may be empty), or
/// `Err(block_message)` if a hard-block check triggers. The first block
/// message is returned; execution stops.
///
/// Warning messages are non-blocking — the command still executes, but
/// the agent receives the warnings in the tool output.
pub(super) fn validate_command_safety(command: &str) -> Result<Vec<String>, String> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let mut warnings = Vec::new();

    // Best-effort split on documented separators outside simple quotes.
    for segment in parse::split_segments(trimmed) {
        let seg = segment.trim();
        if seg.is_empty() {
            continue;
        }

        // Check documented shell -c wrappers and recurse into the inner script.
        if let Some(inner) = parse::extract_inline_script(seg) {
            // Recurse: blocks propagate, warnings accumulate.
            let inner_warnings = validate_command_safety(&inner)?;
            warnings.extend(inner_warnings);
            continue;
        }

        for substitution in parse::extract_command_substitutions(seg) {
            let sub_warnings = validate_command_safety(&substitution)?;
            warnings.extend(sub_warnings);
        }

        let inspection_segment = parse::strip_shell_data(seg);
        let normalized = parse::normalize_whitespace(&inspection_segment);
        let lower = normalized.to_lowercase();

        // Run every registered check against this segment.
        for def in CHECKS {
            if let Some(message) = (def.check)(seg, &normalized, &lower) {
                match def.severity {
                    CheckSeverity::HardBlock => return Err(message),
                    CheckSeverity::SoftWarning => warnings.push(message),
                }
            }
        }
    }

    Ok(warnings)
}

// =============================================================================
// Message Formatting
// =============================================================================

/// Format a blocked-command error message.
pub(super) fn blocked(reason: &str, tip: &str) -> String {
    format!("BLOCKED\n\nReason: {reason}\n\nTip: {tip}")
}

/// Format a soft warning (non-blocking guidance).
pub(super) fn warned(notice: &str, tip: &str) -> String {
    format!("NOTICE: {notice}\n\nTip: {tip}")
}
