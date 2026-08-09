use super::*;

/// The registry-check source for each verdict code, mirroring the mapping
/// recorded in the LLM-judge `ExecPlan` Decision Log. `unknown-destructive` has
/// no registry source (it covers the long tail). `git-stash-destructive`
/// absorbs #68's cancelled `git stash pop` scenario.
const REGISTRY_MAPPING: &[(VerdictCode, &[&str])] = &[
    (VerdictCode::GitHistoryRewrite, &["git_reset"]),
    (
        VerdictCode::GitWorktreeDiscard,
        &["git_checkout", "git_restore"],
    ),
    (VerdictCode::GitUntrackedDelete, &["git_clean"]),
    (VerdictCode::GitForcePush, &["git_push"]),
    (VerdictCode::GitBranchForceDelete, &["git_branch_delete"]),
    (VerdictCode::GitStashDestructive, &["git_stash"]),
    (VerdictCode::DestructiveRm, &["dangerous_rm"]),
    (VerdictCode::GitCommitBackticks, &["git_commit_backticks"]),
    (VerdictCode::RgReplaceFootgun, &["rg_replace_flag"]),
    (VerdictCode::UnknownDestructive, &[]),
];

#[test]
fn every_verdict_code_maps_to_a_representative_command() {
    for code in VerdictCode::ALL {
        let (example_code, example) = VERDICT_CODE_EXAMPLES
            .iter()
            .find(|(c, _)| c == code)
            .unwrap_or_else(|| panic!("no representative command for {}", code.as_str()));
        assert_eq!(example_code, code);
        assert!(!example.is_empty(), "empty example for {}", code.as_str());
    }
    assert_eq!(
        VERDICT_CODE_EXAMPLES.len(),
        VerdictCode::ALL.len(),
        "example table must have exactly one entry per code"
    );
}

#[test]
fn code_spellings_are_stable_and_namespaced() {
    let expected = [
        "git-history-rewrite",
        "git-worktree-discard",
        "git-untracked-delete",
        "git-force-push",
        "git-branch-force-delete",
        "git-stash-destructive",
        "destructive-rm",
        "git-commit-backticks",
        "rg-replace-footgun",
        "unknown-destructive",
    ];
    assert_eq!(VerdictCode::ALL.len(), expected.len());
    for (code, spelling) in VerdictCode::ALL.iter().zip(expected) {
        assert_eq!(code.as_str(), spelling);
        assert_eq!(
            VerdictCode::from_str(spelling).unwrap(),
            *code,
            "round-trip parse failed for {spelling}"
        );
    }
    assert!(
        VerdictCode::from_str("made-up-code").is_err(),
        "unknown codes must not parse"
    );
}

#[test]
fn vocabulary_covers_every_registry_check() {
    // All nine hard-block checks and the single warning of the compiled
    // `bash_safety` registry must map onto the vocabulary, per the ExecPlan
    // Decision Log ("this mapping is complete and preserves current out-of-box
    // behavior").
    let mut mapped: Vec<&str> = REGISTRY_MAPPING
        .iter()
        .flat_map(|(_, sources)| sources.iter().copied())
        .collect();
    mapped.sort_unstable();
    mapped.dedup();
    let mut registry = [
        "git_reset",
        "git_checkout",
        "git_restore",
        "git_clean",
        "git_push",
        "git_branch_delete",
        "git_stash",
        "git_commit_backticks",
        "dangerous_rm",
        "rg_replace_flag",
    ];
    registry.sort_unstable();
    assert_eq!(mapped, registry, "every registry check must map to a code");
}

#[test]
fn default_rubric_documents_every_code_and_example() {
    for (code, example) in VERDICT_CODE_EXAMPLES {
        assert!(
            DEFAULT_RUBRIC.contains(code.as_str()),
            "rubric must document code {}",
            code.as_str()
        );
        assert!(
            DEFAULT_RUBRIC.contains(example),
            "rubric must document the representative command for {}: {example}",
            code.as_str()
        );
    }
}

#[test]
fn default_rubric_snapshot() {
    insta::assert_snapshot!("judge_default_rubric", DEFAULT_RUBRIC);
}

#[test]
fn user_rubric_is_appended_after_default() {
    let with_user = build_judge_system_prompt(Some("Block any command touching ~/secrets."));
    assert!(with_user.starts_with(DEFAULT_RUBRIC));
    assert!(with_user.contains("# User-added rubric guidance"));
    assert!(with_user.contains("Block any command touching ~/secrets."));
}

#[test]
fn empty_user_rubric_leaves_default_unchanged() {
    assert_eq!(build_judge_system_prompt(None), DEFAULT_RUBRIC);
    assert_eq!(build_judge_system_prompt(Some("")), DEFAULT_RUBRIC);
    assert_eq!(build_judge_system_prompt(Some("   \n\t ")), DEFAULT_RUBRIC);
}
