# Code complexity targets

## Scope

This guardrail records the project's cyclomatic complexity (CC) and CRAP targets, the enforcement mechanism that gives agents early feedback on high-complexity code, and the coverage-first refactoring workflow for reducing complexity. These targets apply to every function in the production codebase.

## Targets

  | Metric                            | Target | Applies to                          |
  | --------------------------------- | ------ | ----------------------------------- |
  | McCabe cyclomatic complexity (CC) | ≤ 10   | Most functions                      |
  |                                   | ≤ 15   | Inherently dispatch-heavy functions |
  | CRAP score                        | ≤ 30   | Every function                      |
  |                                   | ≤ 15   | Stretch goal for CC ≤ 10 functions  |

New functions must meet the CC target before merge. A function listed in the baseline (a grandfather, above the target) may not exceed its recorded baseline CC. Grandfathered functions are recorded in the baseline and carry a `#[expect(clippy::cognitive_complexity, reason = "...")]` annotation (where clippy's cognitive complexity also fires) referencing their reduction task.

## Enforcement

### Per-function CC gate

`just cc-check` (`scripts/check-cc.sh`) is the per-function CC gate, and it runs inside `just check`, so a function that exceeds its allowed CC fails the fast local gate rather than only the Coverage job. `scripts/check-coverage.sh` runs the same check from the lcov file it already produced in `just check-full`, and CI runs it through that script. Cyclomatic complexity is coverage-independent, so the check needs no coverage pass:

- A function not listed in `ci/cargo-crap-baseline.json` may not exceed CC 10 (the target).
- A function listed in the baseline may not exceed `max(CC 10, its baseline CC)`. The committed baseline therefore lists only the grandfathers --- the functions above the target --- because for a function at or below the target both ceilings are the target. Reductions are tracked in the reduction tasks referenced below; when a reduction lands, regenerate the baseline with `just change-risk-baseline` to drop the entry.
- Raising an allowed CC requires a deliberate baseline regeneration plus a documented reason in the change (and, for functions above CC 15, the reduction task record below must be updated).

The clippy cognitive-complexity ceiling is a separate, complementary signal: `cognitive-complexity-threshold = 15` in `clippy.toml`, enabled by `cognitive_complexity = "warn"` in `Cargo.toml`. It is enforced in CI by `-D warnings`. Functions above that ceiling carry `#[expect(clippy::cognitive_complexity, reason = "...")]` referencing their reduction task. Clippy's cognitive complexity is not the same metric as McCabe CC; the McCabe CC target is enforced by the per-function gate above.

### Absolute CRAP gate

`scripts/check-crap.sh` is the CRAP gate. `scripts/check-coverage.sh` runs it from the lcov file it already produced, so it runs in the CI Coverage job and in `just check-full`, but not in the fast local gate (`just check`), which has no coverage pass. CRAP is `CC² · (1 − coverage)³ + CC`, so a function with 100% coverage scores exactly its CC:

- A function whose CRAP exceeds 30 fails the gate.
- A function at or below CC 10 whose CRAP exceeds the 15 stretch target prints a non-failing warning.
- A function listed in the baseline with a `crap` allowance (a grandfather) may not exceed that allowance instead of 30.

This replaces the per-function CRAP delta ratchet that compared every function against a committed snapshot and failed on any increase over 0.5 ([ADR 039](../adr/039-absolute-complexity-targets.md)). At 100% coverage that ratchet restated the CC gate more strictly than this document allows, so adding one branch to a well-covered function failed CI and demanded a baseline edit (#656, #703). The gate now enforces the target stated above. The accepted tradeoff is that a function may grow to the CC target, and a within-target regression or a coverage collapse while under CRAP 30 is not gated by any per-change signal; whether to restore a diff-scoped one is tracked by #660.

### Grandfathered functions

The committed baseline currently has no functions at or above the ≤ 15 dispatch-heavy allowance. Functions at CC 11--14 are within that allowance and remain ratcheted by `max(CC 10, baseline CC)`; the current highest entries are `Skill::parse_frontmatter_fallback`, `ensure_secure_temp_dir`, `run_command_hook`, and `TaskOutcome::deserialize` at CC 14, followed by `scan_directory` and `escape_control_chars_in_strings` at CC 13. Functions not listed use the default target as their ceiling, so the baseline does not permit growth through the target. `Skill::parse_frontmatter_fallback` is also the one CRAP grandfather: at CC 14 and 0% coverage its CRAP is 210, so the baseline records a `crap` allowance for it until its malformed-frontmatter fallback is covered or removed (#659). That allowance is temporary and must stay removable.

### Baseline regeneration

`ci/cargo-crap-baseline.json` is the CC grandfather list plus any deliberate CRAP allowance, regenerated by `just change-risk-baseline`. The recipe measures a fresh cargo-crap report under `scripts/hermetic-coverage.sh` (a scratch `HOME`, so a test that reads the developer's home cannot move the numbers, #699), then `scripts/prune-crap-baseline.py` keeps only the functions above the CC target and carries forward any `crap` allowance already in the committed file. The result records only verdict-changing entries, has no `line`, `coverage`, or per-entry `crap` field except a deliberate allowance, and is byte-identical when regenerated on a clean tree. Regeneration is therefore a near-never operation: it is needed only when a function's CC legitimately crosses the target, or to drop an entry after a reduction lands. A new test should still avoid reading the developer's home directly; the hermetic wrapper makes a stray dependency harmless to the committed file, but a plain `cargo test`
does not run under it.

## Coverage-first refactoring workflow

When reducing complexity in an existing function:

1. Write focused tests achieving ≥ 80% line and branch coverage.
2. Run `cargo-crap --lcov lcov.info` to confirm the CRAP drop.
3. Refactor by extracting sub-functions, replacing match ladders with lookup tables or typed dispatch, and reducing nesting.
4. Re-run `cargo-crap` to confirm CC target is met without regressing coverage.
5. Regenerate the baseline with `just change-risk-baseline` so an entry whose CC is now at or below the target is dropped, and remove any now-unfulfilled `#[expect(clippy::cognitive_complexity)]` annotation (`-D warnings` fails on unfulfilled expectations).

## Provenance

Targets and workflow were established in task #325 (2026-07-30), accepted contingent on mechanical enforcement (see #335). Enforcement mechanisms (per-function CC gate, clippy cognitive-complexity ceiling, operating-loop check) landed in #103. The CRAP delta ratchet was replaced by the absolute CRAP gate in #658 under [ADR 039](../adr/039-absolute-complexity-targets.md). The full rationale and dependent refactoring tasks are listed in #325.
