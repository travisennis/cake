---
status: accepted
date: 2026-10-07
decision-makers: Travis Ennis
informed: issue 658
---

# Enforce absolute complexity targets instead of a CRAP delta ratchet

## Context and Problem Statement

Cake's Coverage job runs three gates from one instrumented pass: total coverage must stay at or above 90%, a per-function CRAP score is compared against the committed `ci/cargo-crap-baseline.json`, and a per-function cyclomatic-complexity (CC) ceiling is enforced. The middle gate is a delta ratchet. It fails when any function's measured CRAP exceeds its recorded value by more than a fixed tolerance (default 0.5), so any change that raises a function's CRAP by a full point turns CI red and demands a baseline edit.

CRAP is computed from complexity and coverage (`CRAP = CC² · (1 − coverage)³ + CC`). At 100% coverage it collapses to `CRAP = CC`, so for a well-tested function the delta ratchet restates the CC gate with a tolerance far stricter than the complexity guardrail. `docs/guardrails/complexity-targets.md` permits an existing function to grow up to the greater of CC 10 and the CC it had when the baseline was generated; the delta ratchet rejects the same growth as soon as one branch is added.

Two recent pull requests show the contradiction:

- #656 moved `HookEvent::from_str` from CC 9 to CC 10 by adding a single enum arm. `just cc-check` passed with zero functions over their allowed value; the CRAP delta gate reported Δ+1.000.
- PR #703 added image support and raised three functions by exactly one branch each --- `ChatMessageBuilder::push_function_call_output` (CC 1→2), `ResponsesApiInputItem::from` (CC 6→7), and `SettingsLoader::merge_output_budgets` (CC 7→8) --- all far below the CC 10 target. The CC gate passed; the CRAP delta gate reported Δ+1.0 on all three.

The ratchet also compares against a committed snapshot rather than the code under test. `ci/cargo-crap-baseline.json` records the line, coverage, and CRAP of every function as measured on whichever machine last ran `just change-risk-baseline`. #658 recorded that a regeneration rewrote roughly 600 lines for a two-entry semantic change, that coverage drift moved untouched functions in both directions, and that `SandboxScanState::reset_command` measured 0% on one machine and 100% in the committed baseline with no code difference. #700 has since made measurement hermetic (a scratch `HOME` via `scripts/hermetic-coverage.sh`, so a test that reads the developer's home cannot move the numbers) and #698 refreshed the baseline, which removes the environment divergence. The artifact still conflates line moves, coverage noise, and complexity into one committed file that every branch must keep in step, and the gate is invisible in the local loop because `just check` runs only the coverage-free CC
gate.

## Decision Drivers

- The gate a contributor meets in CI must enforce the same contract the guardrail documents. Two gates that disagree on the same function erode trust in both.
- The enforced signal must be deterministic and independent of which machine produced it.
- Regenerating `ci/cargo-crap-baseline.json` must stop being a routine, cross-machine, conflict-prone operation.
- Complexity must remain bounded by a gate that catches real structural growth, and it must run in the fast local loop.

## Considered Options

- **Keep and fix the delta ratchet.** This is the decision recorded in #74 on 2026-08-16: keep the ratchet and reduce its noise. Rejected: the disagreement with the CC guardrail is intrinsic to a delta gate whose tolerance is smaller than the CC allowance, and the committed-snapshot design keeps the artifact machine-dependent and conflict-prone even after the hermetic fix.
- **Drop the complexity signal entirely.** Keep total coverage only. Rejected: it removes every bound on structural growth and every signal that a function became both complex and poorly covered.
- **Absolute complexity targets with the coverage-free CC gate (chosen).** Enforce the targets the guardrail already states, keep the deterministic CC gate, and shrink the baseline to the grandfather set.
- **A diff-scoped delta against the merge base.** Compute per-function CRAP at the merge base and the head and report only the functions a pull request touched. Not rejected, but deferred: it costs a second instrumented run in CI and is tracked separately by #660.

## Decision Outcome

Chosen option: "Absolute complexity targets with the coverage-free CC gate", because it enforces the documented targets, removes the machine-dependent artifact, and keeps the deterministic gate that already runs in `just check`.

Concretely:

- `scripts/check-coverage.sh` stops comparing per-function CRAP against `ci/cargo-crap-baseline.json`. The `--fail-regression` comparison and the `CRAP_REGRESSION_EPSILON` knob are deleted.
- An absolute CRAP check replaces it: fail any function whose CRAP exceeds 30, and warn --- without failing the build --- on a function whose CRAP exceeds 15 while its CC is 10 or lower. This is the target and stretch pair `docs/guardrails/complexity-targets.md` already states.
- `scripts/check-cc.sh` is unchanged. It stays the coverage-independent gate in `just check` and keeps enforcing `max(CC 10, baseline CC)` per function.
- `ci/cargo-crap-baseline.json` shrinks to the CC grandfather set: one `{file, function, cyclomatic}` entry per function whose CC exceeds the target, plus a single CRAP allowance for the one function the CRAP target check must grandfather (`Skill::parse_frontmatter_fallback`, CRAP 210.0, CC 14, 0% coverage, tracked by #659). The artifact drops its `line`, `coverage`, and `crap` fields, which were the churn and cross-machine sources.
- `scripts/coverage-guard.py` is re-audited. The stale-profile and duplicate-source-root checks stay, because they fixed real false totals (#520); any part that exists only to protect the committed baseline is removed.

### Consequences

- Good, because a fully covered function may grow within the CC target without a baseline edit, so the failing condition matches the guardrail a contributor reads.
- Good, because the baseline becomes machine-independent and near-never regenerated, removing the per-branch conflict and the roughly 2.5% of commits that had to touch it (the cost recorded in #74).
- Good, because the fast, coverage-free `just cc-check` remains the only complexity signal a contributor meets before pushing, and it can now disagree with CI only by the CC allowance, not by a stricter delta.
- Bad, because the gate loses the signal that a specific pull request made a function harder within the target or reduced its coverage while staying under CRAP 30. That gap is deferred to #660, which should decide whether to reintroduce a merge-base diff report rather than a committed snapshot.
- Bad, because `Skill::parse_frontmatter_fallback` is grandfathered against the CRAP target until #659 covers or removes its malformed-frontmatter fallback. The grandfather must stay removable, not become a permanent exemption.

## More Information

This decision changes the enforcement contract established by #325/#335 and reverses the "keep the ratchet" decision recorded in #74 on 2026-08-16. It is implemented by #658. Related records: #659 removes the CRAP grandfather once the fallback is covered or removed; #660 decides whether a diff-scoped signal returns; #656 and PR #703 are the failures that motivated the change. The enforced rules are documented in `docs/guardrails/complexity-targets.md`.
