//! Embedded default rubric and stable verdict-code vocabulary for the LLM
//! command-safety judge.
//!
//! `Milestone 3` of the LLM-judge `ExecPlan`: the rubric text is the judge's
//! system prompt, distilled from the nine hard-block checks and the single
//! warning of the compiled `bash_safety` guard (deleted in Milestone 5). The
//! verdict-code vocabulary is the stable, namespaced replacement for ADR-015's
//! rule IDs: block and warn verdicts must carry one of the codes enumerated
//! here, and any other code fails closed as a malformed verdict.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "verdict-code vocabulary is consumed by the judge preflight in ExecPlan Milestone 5"
    )
)]

use std::str::FromStr;

/// The embedded default rubric: the judge's system prompt.
///
/// Embedded via `include_str!` so the text ships in the binary, and guarded by
/// an insta snapshot so changes are reviewable. An optional user rubric file
/// (setting `[tools.bash.judge] rubric_file`) is appended by
/// [`build_judge_system_prompt`].
pub const DEFAULT_RUBRIC: &str = include_str!("judge_rubric.md");

/// Stable verdict codes the judge may return for block and warn verdicts.
///
/// The `as_str` spelling is the wire and telemetry vocabulary. `allow`
/// verdicts need no code; `unknown-destructive` covers long-tail catches that
/// fit no named class. Explicit discriminants fix the order `as_str` indexes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VerdictCode {
    /// `git reset --hard` / `git reset --merge` destroys uncommitted changes.
    GitHistoryRewrite = 0,
    /// `git checkout -- <file>` / bare `git restore` discard worktree changes.
    GitWorktreeDiscard = 1,
    /// `git clean -f` deletes untracked files.
    GitUntrackedDelete = 2,
    /// `git push --force` overwrites remote history (never `--force-with-lease`).
    GitForcePush = 3,
    /// `git branch -D` force-deletes a branch without a merge check.
    GitBranchForceDelete = 4,
    /// `git stash drop` / `git stash clear` permanently delete stashes.
    GitStashDestructive = 5,
    /// `rm -rf` outside the temp-directory carve-outs.
    DestructiveRm = 6,
    /// `git commit -m` with backticks or `$()` in a double-quoted message.
    GitCommitBackticks = 7,
    /// `rg -rn` footgun: `-r` swallows the intended `n` as the replacement.
    RgReplaceFootgun = 8,
    /// Long-tail destructive commands that fit no named class.
    UnknownDestructive = 9,
}

impl VerdictCode {
    /// All verdict codes in the v1 vocabulary, in stable order.
    pub const ALL: &[Self] = &[
        Self::GitHistoryRewrite,
        Self::GitWorktreeDiscard,
        Self::GitUntrackedDelete,
        Self::GitForcePush,
        Self::GitBranchForceDelete,
        Self::GitStashDestructive,
        Self::DestructiveRm,
        Self::GitCommitBackticks,
        Self::RgReplaceFootgun,
        Self::UnknownDestructive,
    ];

    /// The stable, namespaced wire spelling of this code.
    pub const fn as_str(self) -> &'static str {
        // Indexed by discriminant; `CODE_SPELLINGS` is in discriminant order
        // and the round-trip test guards against drift.
        CODE_SPELLINGS[self as usize].0
    }

    /// Whether this code's class warns rather than blocks.
    ///
    /// Every code except `rg-replace-footgun` names a destructive class, so a
    /// `warn` verdict may only carry that one code; any other code on a `warn`
    /// contradicts the rubric and fails closed.
    pub const fn is_warn_class(self) -> bool {
        matches!(self, Self::RgReplaceFootgun)
    }
}

/// Stable spelling for each verdict code, in discriminant order: the single
/// source of truth for both directions of the mapping.
const CODE_SPELLINGS: [(&str, VerdictCode); 10] = [
    ("git-history-rewrite", VerdictCode::GitHistoryRewrite),
    ("git-worktree-discard", VerdictCode::GitWorktreeDiscard),
    ("git-untracked-delete", VerdictCode::GitUntrackedDelete),
    ("git-force-push", VerdictCode::GitForcePush),
    ("git-branch-force-delete", VerdictCode::GitBranchForceDelete),
    ("git-stash-destructive", VerdictCode::GitStashDestructive),
    ("destructive-rm", VerdictCode::DestructiveRm),
    ("git-commit-backticks", VerdictCode::GitCommitBackticks),
    ("rg-replace-footgun", VerdictCode::RgReplaceFootgun),
    ("unknown-destructive", VerdictCode::UnknownDestructive),
];

impl FromStr for VerdictCode {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        CODE_SPELLINGS
            .iter()
            .find(|(spelling, _)| *spelling == s)
            .map(|(_, code)| *code)
            .ok_or(())
    }
}

/// Representative command for each verdict code.
///
/// The rubric text documents the same mapping in prose; this table drives the
/// vocabulary-completeness tests and is the machine-readable statement that
/// every registry check maps to a code.
pub const VERDICT_CODE_EXAMPLES: &[(VerdictCode, &str)] = &[
    (VerdictCode::GitHistoryRewrite, "git reset --hard HEAD~1"),
    (
        VerdictCode::GitWorktreeDiscard,
        "git checkout -- src/main.rs",
    ),
    (VerdictCode::GitUntrackedDelete, "git clean -fd"),
    (VerdictCode::GitForcePush, "git push --force origin main"),
    (VerdictCode::GitBranchForceDelete, "git branch -D feature/x"),
    (VerdictCode::GitStashDestructive, "git stash drop"),
    (VerdictCode::DestructiveRm, "rm -rf ./node_modules"),
    (
        VerdictCode::GitCommitBackticks,
        "git commit -m \"update $(date)\"",
    ),
    (VerdictCode::RgReplaceFootgun, "rg -rn foo"),
    (
        VerdictCode::UnknownDestructive,
        "find . -name '*.tmp' -delete",
    ),
];

/// The judge's system prompt: the embedded default rubric, with the optional
/// user rubric file's guidance appended.
///
/// A `None`, empty, or whitespace-only user rubric leaves the default rubric
/// unchanged.
pub fn build_judge_system_prompt(user_rubric: Option<&str>) -> String {
    let Some(user_rubric) = user_rubric.filter(|text| !text.trim().is_empty()) else {
        return DEFAULT_RUBRIC.to_string();
    };
    format!("{DEFAULT_RUBRIC}\n\n# User-added rubric guidance\n\n{user_rubric}")
}

#[cfg(test)]
#[path = "judge_rubric_tests.rs"]
mod tests;
