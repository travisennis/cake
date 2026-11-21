# Code complexity targets

## Scope

This guardrail records the project's cyclomatic complexity (CC) target and the enforcement mechanism that gives agents early feedback on high-complexity code. The target applies to every function in the production codebase.

## Targets

  | Metric                            | Target | Applies to                          |
  | --------------------------------- | ------ | ----------------------------------- |
  | McCabe cyclomatic complexity (CC) | ≤ 10   | Most functions                      |
  |                                   | ≤ 15   | Inherently dispatch-heavy functions |

New functions must meet the CC target before merge. A function listed in the baseline (a grandfather, above the target) may not exceed its recorded baseline CC. Grandfathered functions carry a `#[expect(clippy::cognitive_complexity, reason = "...")]` annotation where clippy's cognitive complexity also fires.

## Enforcement

### Per-function CC gate

`just cc-check` (`scripts/check-cc.sh`) is the per-function CC gate, and it runs inside `just check`, so a function that exceeds its allowed CC fails the local gate:

- A function not listed in `ci/complexity-baseline.json` may not exceed CC 10 (the target).
- A function listed in the baseline may not exceed `max(CC 10, its baseline CC)`. The committed baseline therefore lists only the grandfathers --- the functions above the target --- because for a function at or below the target both ceilings are the target.
- Raising an allowed CC requires a deliberate baseline edit plus a documented reason in the change.

The clippy cognitive-complexity ceiling is a separate, complementary signal: `cognitive-complexity-threshold = 15` in `clippy.toml`, enabled by `cognitive_complexity = "warn"` in `Cargo.toml`. It is enforced in CI by `-D warnings`. Clippy's cognitive complexity is not the same metric as McCabe CC; the McCabe CC target is enforced by the per-function gate above.

### Grandfathered functions

The committed baseline currently has no functions at or above the ≤ 15 dispatch-heavy allowance. Functions at CC 11--14 are within that allowance and remain ratcheted by `max(CC 10, baseline CC)`; the current highest entries are `ensure_secure_temp_dir`, `run_command_hook`, and `TaskOutcome::deserialize` at CC 14, followed by `scan_directory` and `escape_control_chars_in_strings` at CC 13.

### Baseline maintenance

`ci/complexity-baseline.json` is the CC grandfather list. `just cc-check` reports the functions that exceed their allowed ceiling; when a function's CC legitimately crosses the target, add its `{file, function, cyclomatic}` entry to the baseline and explain why in the change. When a reduction brings a function to or below the target, delete its entry and remove any now-unfulfilled `#[expect(clippy::cognitive_complexity)]` annotation (`-D warnings` fails on unfulfilled expectations).

## Reducing complexity

When reducing complexity in an existing function:

1. Refactor by extracting sub-functions, replacing match ladders with lookup tables or typed dispatch, and reducing nesting.
2. Run `just cc-check` to confirm the target is met.
3. Drop the baseline entry if the function is now at or below the target, and remove any annotation that no longer applies.

## Provenance

Targets were established in task #325 (2026-07-30), accepted contingent on mechanical enforcement (see #335). Enforcement mechanisms (per-function CC gate, clippy cognitive-complexity ceiling, operating-loop check) landed in #103.
