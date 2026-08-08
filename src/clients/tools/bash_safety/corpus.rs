// Corpus-driven regression tests for the bash_safety guard.
//
// The repetitive `assert_blocked` / `assert_allowed` / `assert_warned` cases
// that used to live in `checks.rs` moved to `corpus/commands.jsonl`, one JSON
// object per line:
//
//   {"command": "git reset --hard HEAD~3", "expect": "blocked", "note": "..."}
//
// `expect` is one of `blocked` / `warned` / `allowed`; `note` is optional and
// carries the reason the case was added. The corpus is compiled in with
// `include_str!`, so the test runs with no runtime path resolution or
// working-directory assumption. Adding a regression case means appending one
// line; see CONTRIBUTING.md.

use super::*;
use serde::Deserialize;

/// Compiled-in corpus of command-safety cases.
const CORPUS: &str = include_str!("corpus/commands.jsonl");

/// One case: a command, the outcome the guard must produce, and an optional
/// note explaining why the case exists.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    command: String,
    expect: Expect,
    note: Option<String>,
}

/// The outcome the guard must produce for a corpus command.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Expect {
    Blocked,
    Warned,
    Allowed,
}

/// Every corpus case must match the guard's current behavior. A regression
/// fails loudly and names every offending command before panicking, so one
/// broken case cannot hide the rest.
#[test]
fn cases_match_guard_behavior() {
    let mut failures: Vec<String> = Vec::new();
    let mut case_count = 0usize;

    for (index, raw_line) in CORPUS.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let line_number = index + 1;

        let entry: Case = match serde_json::from_str(line) {
            Ok(entry) => entry,
            Err(err) => {
                failures.push(format!(
                    "line {line_number}: malformed corpus entry ({raw_line:?}): {err}"
                ));
                continue;
            },
        };
        case_count += 1;

        if let Err(mismatch) = check_case(&entry) {
            let note = entry
                .note
                .as_deref()
                .map_or_else(String::new, |note| format!(" (note: {note})"));
            failures.push(format!(
                "line {line_number}: command {:?}{note}: {mismatch}",
                entry.command
            ));
        }
    }

    if case_count == 0 {
        let detail = if failures.is_empty() {
            "the corpus file is empty".to_string()
        } else {
            format!("no usable cases:\n{}", failures.join("\n"))
        };
        panic!("bash_safety corpus rejected: {detail}");
    }

    assert!(
        failures.is_empty(),
        "bash_safety corpus: {} case(s) failed:\n{}",
        failures.len(),
        failures.join("\n"),
    );
}

/// Check one case against `validate_command_safety`, returning a description
/// of the mismatch when the guard's outcome differs from `expect`.
fn check_case(entry: &Case) -> Result<(), String> {
    match validate_command_safety(&entry.command) {
        Ok(warnings) => match entry.expect {
            Expect::Blocked => Err(format!("expected BLOCKED, got Ok(warnings: {warnings:?})")),
            Expect::Warned if !warnings.is_empty() => Ok(()),
            Expect::Warned => Err("expected WARNING, got Ok with no warnings".to_string()),
            Expect::Allowed => Ok(()),
        },
        Err(block) => match entry.expect {
            Expect::Blocked => Ok(()),
            Expect::Warned => Err(format!("expected WARNING, got BLOCKED: {block}")),
            Expect::Allowed => Err(format!("expected ALLOWED, got BLOCKED: {block}")),
        },
    }
}
