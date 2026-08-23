# Code complexity targets

## Scope

This guardrail records the project's cyclomatic complexity (CC) and CRAP targets, the enforcement mechanism that gives agents early feedback on high-complexity code, and the coverage-first refactoring workflow for reducing complexity. These targets apply to every function in the production codebase.

## Targets

  | Metric                            | Target | Applies to                                              |
  | --------------------------------- | ------ | ------------------------------------------------------- |
  | McCabe cyclomatic complexity (CC) | ≤ 10   | Most functions                                          |
  |                                   | ≤ 15   | Inherently dispatch-heavy functions                     |
  |                                   | ≤ 20   | Grandfathered structurally complex loop (`Agent::send`) |
  | CRAP score                        | ≤ 30   | Every function                                          |
  |                                   | ≤ 15   | Stretch goal for CC ≤ 10 functions with ≥ 90% coverage  |

Functions introduced or modified in a change must meet the CC target before merge. Grandfathered exceptions are recorded in the per-function baseline and in the table below, and carry a `#[expect(clippy::cognitive_complexity, reason = "...")]` annotation (where clippy's cognitive complexity also fires) referencing their reduction task.

## Enforcement

### Per-function CC gate

`just cc-check` (`scripts/check-cc.sh`) is the operating-loop check agents run before handoff. It is part of `just ci` via the cyclomatic-complexity gate in `scripts/check-coverage.sh`, so CI fails on exceedances. Cyclomatic complexity is coverage-independent, so the gate runs without a coverage pass:

- A function absent from `ci/cargo-crap-baseline.json` (a new function) may not exceed CC 10 (the target).
- A function present in the baseline may not exceed the CC it had when the baseline was generated (a ratchet). Reductions are tracked in the reduction tasks referenced below; when a reduction lands, regenerate the baseline with `just change-risk-baseline`.
- Raising an allowed CC requires a deliberate baseline regeneration plus a documented reason in the change (and, for functions above CC 15, the reduction task record below must be updated).

The clippy cognitive-complexity ceiling is a separate, complementary signal: `cognitive-complexity-threshold = 15` in `clippy.toml`, enabled by `cognitive_complexity = "warn"` in `Cargo.toml`. It is enforced in CI by `-D warnings`. Functions above that ceiling carry `#[expect(clippy::cognitive_complexity, reason = "...")]` referencing their reduction task. Clippy's cognitive complexity is not the same metric as McCabe CC; the McCabe CC target is enforced by the per-function gate above.

### Grandfathered functions

Functions whose current McCabe CC is at or above the ≤ 15 dispatch-heavy allowance are grandfathered in the baseline and tracked by the reduction tasks below. Until their task lands, they may not grow past their baseline CC.

  | Current CC | Function                            | File                                      | Reduction task |
  | ---------- | ----------------------------------- | ----------------------------------------- | -------------- |
  | 38         | `has_unsafe_message_flag`           | `src/clients/tools/bash_safety/checks.rs` | #96            |
  | 27         | `check_rg_replace_flag`             | `src/clients/tools/bash_safety/checks.rs` | #96            |
  | 26         | `split_segments`                    | `src/clients/tools/bash_safety/parse.rs`  | #97            |
  | 22         | `check_dangerous_rm`                | `src/clients/tools/bash_safety/checks.rs` | #96            |
  | 20         | `Agent::send`                       | `src/clients/agent/agent_loop.rs`         | #101           |
  | 18         | `SettingsLoader::load_with_profile` | `src/config/settings.rs`                  | #100           |
  | 16         | `Session::load`                     | `src/config/session.rs`                   | #102           |
  | 16         | `read_file`                         | `src/clients/tools/read.rs`               | #102           |
  | 16         | `check_git_commit_backticks`        | `src/clients/tools/bash_safety/checks.rs` | #96            |
  | 15         | `HookRunner::run_and_aggregate`     | `src/hooks.rs`                            | #102           |
  | 15         | `collect_matching_files`            | `src/config/worktree.rs`                  | #102           |
  | 15         | `resolve_write_path`                | `src/clients/tools/mod.rs`                | #102           |
  | 15         | `execute_edit`                      | `src/clients/tools/edit.rs`               | #102           |
  | 15         | `strip_shell_data`                  | `src/clients/tools/bash_safety/parse.rs`  | #97            |

Functions at CC 11--14 are within the dispatch-heavy allowance and are likewise ratcheted by the baseline; they are candidates for future reduction tasks when touched.

## Coverage-first refactoring workflow

When reducing complexity in an existing function:

1. Write focused tests achieving ≥ 80% line and branch coverage.
2. Run `cargo-crap --lcov lcov.info` to confirm the CRAP drop.
3. Refactor by extracting sub-functions, replacing match ladders with lookup tables or typed dispatch, and reducing nesting.
4. Re-run `cargo-crap` to confirm CC target is met without regressing coverage.
5. Regenerate the baseline with `just change-risk-baseline` so the reduced CC becomes the new ceiling, and remove any now-unfulfilled `#[expect(clippy::cognitive_complexity)]` annotation (`-D warnings` fails on unfulfilled expectations).

## Provenance

Targets and workflow were established in task #325 (2026-07-30), accepted contingent on mechanical enforcement (see #335). Enforcement mechanisms (per-function CC gate, clippy cognitive-complexity ceiling, operating-loop check) landed in #103. The full rationale and dependent refactoring tasks are listed in #325.
