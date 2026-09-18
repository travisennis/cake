# Code complexity targets

## Scope

This guardrail records the project's cyclomatic complexity (CC) and CRAP targets, the enforcement mechanism that gives agents early feedback on high-complexity code, and the coverage-first refactoring workflow for reducing complexity. These targets apply to every function in the production codebase.

## Targets

  | Metric                            | Target | Applies to                                             |
  | --------------------------------- | ------ | ------------------------------------------------------ |
  | McCabe cyclomatic complexity (CC) | ≤ 10   | Most functions                                         |
  |                                   | ≤ 15   | Inherently dispatch-heavy functions                    |
  | CRAP score                        | ≤ 30   | Every function                                         |
  |                                   | ≤ 15   | Stretch goal for CC ≤ 10 functions with ≥ 90% coverage |

New functions must meet the CC target before merge. Existing functions are allowed up to the greater of the target and their current baseline CC, so functions below the target cannot grow past it while grandfathered functions above it cannot grow past their baseline. Grandfathered exceptions are recorded in the per-function baseline and in the table below, and carry a `#[expect(clippy::cognitive_complexity, reason = "...")]` annotation (where clippy's cognitive complexity also fires) referencing their reduction task.

## Enforcement

### Per-function CC gate

`just cc-check` (`scripts/check-cc.sh`) is the per-function CC gate, and it runs inside `just check`, so a function that exceeds its allowed CC fails the fast local gate rather than only the Coverage job. `scripts/check-coverage.sh` runs the same check from the lcov file it already produced in `just check-full`, and CI runs it through that script. Cyclomatic complexity is coverage-independent, so the check needs no coverage pass:

- A function absent from `ci/cargo-crap-baseline.json` (a new function) may not exceed CC 10 (the target).
- A function present in the baseline may not exceed `max(CC 10, its baseline CC)`. This keeps existing functions below the target under the target ceiling while preserving the historical ceiling for grandfathered functions above it. Reductions are tracked in the reduction tasks referenced below; when a reduction lands, regenerate the baseline with `just change-risk-baseline`.
- Raising an allowed CC requires a deliberate baseline regeneration plus a documented reason in the change (and, for functions above CC 15, the reduction task record below must be updated).

The clippy cognitive-complexity ceiling is a separate, complementary signal: `cognitive-complexity-threshold = 15` in `clippy.toml`, enabled by `cognitive_complexity = "warn"` in `Cargo.toml`. It is enforced in CI by `-D warnings`. Functions above that ceiling carry `#[expect(clippy::cognitive_complexity, reason = "...")]` referencing their reduction task. Clippy's cognitive complexity is not the same metric as McCabe CC; the McCabe CC target is enforced by the per-function gate above.

### Grandfathered functions

The committed baseline currently has no functions at or above the ≤ 15 dispatch-heavy allowance. Functions at CC 11--14 are within that allowance and remain ratcheted by `max(CC 10, baseline CC)`; the current highest entries are `Skill::parse_frontmatter_fallback`, `ensure_secure_temp_dir`, `run_command_hook`, and `TaskOutcome::deserialize` at CC 14, followed by `scan_directory` and `escape_control_chars_in_strings` at CC 13. Functions below CC 10 use the default target as their ceiling, so the baseline does not permit growth through the target.

## Coverage-first refactoring workflow

When reducing complexity in an existing function:

1. Write focused tests achieving ≥ 80% line and branch coverage.
2. Run `cargo-crap --lcov lcov.info` to confirm the CRAP drop.
3. Refactor by extracting sub-functions, replacing match ladders with lookup tables or typed dispatch, and reducing nesting.
4. Re-run `cargo-crap` to confirm CC target is met without regressing coverage.
5. Regenerate the baseline with `just change-risk-baseline` so the reduced CC becomes the new ceiling, and remove any now-unfulfilled `#[expect(clippy::cognitive_complexity)]` annotation (`-D warnings` fails on unfulfilled expectations).

## Provenance

Targets and workflow were established in task #325 (2026-07-30), accepted contingent on mechanical enforcement (see #335). Enforcement mechanisms (per-function CC gate, clippy cognitive-complexity ceiling, operating-loop check) landed in #103. The full rationale and dependent refactoring tasks are listed in #325.
