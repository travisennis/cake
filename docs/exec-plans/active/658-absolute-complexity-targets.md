# Replace the CRAP Delta Ratchet With Absolute Complexity Targets

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows the ExecPlan workflow ([docs/workflow/exec-plans.md](../../workflow/exec-plans.md)). Any contributor implementing this plan must keep this file self-contained and update it whenever the implementation changes direction, discovers new behavior, or completes a milestone. It implements [issue #658](https://github.com/travisennis/cake/issues/658) under the decision recorded in [ADR 039](../adr/039-absolute-complexity-targets.md).

## Purpose / Big Picture

Cake's Coverage job currently fails on complexity the project's own guardrail permits. A per-function CRAP delta ratchet compares every function against a committed snapshot, `ci/cargo-crap-baseline.json`, and fails when a function's CRAP rises by more than 0.5. Because CRAP equals cyclomatic complexity (CC) at 100% coverage, adding one branch to a well-tested function produces a Δ+1.0 failure even though `docs/guardrails/complexity-targets.md` allows an existing function to grow up to CC 10. PR #703 hit this on three ordinary functions; #656 hit the same contradiction one release earlier. Separately, the snapshot records whichever machine last regenerated it, so keeping it current is churn that parallel branches conflict over.

After this change, the project enforces the targets its guardrail documents. A contributor can add a branch to a fully covered function, run the gates, and pass with no baseline edit, as long as the function stays within CC 10. A function that is both complex and poorly covered still fails, at the actual CRAP 30 target rather than at a delta. The `ci/cargo-crap-baseline.json` artifact shrinks from roughly 1,180 per-function rows to the dozen or so functions whose CC exceeds the target, becomes machine-independent, and stops being a routine regenerated file.

What someone can do after this change that they could not do before:

- Add an enum arm to a fully covered `match` and pass every gate without editing `ci/cargo-crap-baseline.json`.
- Read `docs/guardrails/complexity-targets.md` and find that its stated targets are exactly what CI enforces, with no stricter hidden delta.
- Run `just change-risk-baseline` on a clean checkout and produce no diff, proving the artifact is deterministic.

How to verify it works:

- From the repository root, `just cc-check` prints `PASS: No cyclomatic complexity exceedances` with zero functions over their allowed value.
- From the repository root, `just check-coverage` prints `PASS: All coverage gates passed` on an unmodified `master`.
- From the repository root, `just change-risk-baseline` followed by `git diff --exit-code ci/cargo-crap-baseline.json` produces no output and exits 0.
- A deliberately bloated function (CC 10 at 20% coverage, CRAP well above 30) fails `just check-coverage` with a message naming the function.

## Progress

- [x] (2026-10-07T00:00:00Z) Wrote ADR 039 recording the decision to retire the CRAP delta ratchet for absolute targets.
- [x] (2026-10-07T00:00:00Z) Created this ExecPlan from issue #658 and the current scripts, justfile, baseline, and guardrail.
- [ ] Milestone 1: retire the delta comparison and add an absolute CRAP gate.
- [ ] Milestone 2: shrink `ci/cargo-crap-baseline.json` and update every consumer of it.
- [ ] Milestone 3: update the guardrail, `CONTRIBUTING.md`, the justfile comments, and the Coverage CI step.
- [ ] Milestone 4: prove acceptance --- the enum-arm case passes, regeneration is a no-op, and a bloated function fails.

## Surprises & Discoveries

- Observation: `cargo-crap` 0.2.2 already supports the failing half of the absolute gate directly. Evidence: `cargo crap --help` lists `--threshold <THRESHOLD>` (score above which a function is "crappy", default 30) and `--fail-above` (exit non-zero if any function exceeds the threshold). It has no warn tier and no per-function allowance, so the full gate still needs a small wrapper, but the failure comparison does not need to be hand-rolled from scratch.
- Observation: the baseline today is 1,181 entries; 1,169 are at CC ≤ 10 and 12 are above it. The only entry above CRAP 30 is `Skill::parse_frontmatter_fallback` (CRAP 210.0, CC 14, 0% coverage). The only two entries above CRAP 15 with CC ≤ 10 are `ReasoningContentKind::from` (CRAP 20.0, CC 4) and `Skill::read_frontmatter` (CRAP 16.6, CC 10). Evidence: computed from `ci/cargo-crap-baseline.json` with `scripts/check-cc.sh`'s `(file, function)` keying. These match the counts in #658, so its evidence is current.
- Observation: the fast local gate never runs the CRAP comparison, so this failure class is first seen in CI. Evidence: `just check` runs `cc-check` (coverage-independent) but not `check-coverage`; `CONTRIBUTING.md` routes code changes to `just check`.

## Decision Log

- Decision: retire the CRAP regression comparison and enforce absolute targets (fail above CRAP 30, warn above CRAP 15 for CC ≤ 10). Rationale: the delta gate contradicts the CC guardrail and depends on a machine-specific snapshot. Date/Author: 2026-10-07, recorded in [ADR 039](../adr/039-absolute-complexity-targets.md).
- Decision: keep the CRAP check in `scripts/check-coverage.sh` as its own gate rather than replacing it with `cargo crap --threshold 30 --fail-above`. Rationale: the warn tier is conditioned on CC ≤ 10 and the grandfather is per-function, neither of which `cargo-crap`'s single-threshold interface can express; a small wrapper mirrors `scripts/check-cc.sh` and stays testable with a fixture. Date/Author: 2026-10-07.
- Decision: shrink the baseline to `{file, function, cyclomatic}` entries plus one `crap` allowance on the grandfathered function. Rationale: `line`, `coverage`, and the per-entry `crap` value are the churn and cross-machine sources; `scripts/check-cc.sh` already reads only `file`, `function`, and `cyclomatic`. Date/Author: 2026-10-07.

## Outcomes & Retrospective

Not yet started. This section is filled in before the implementing pull request opens, per the ExecPlan workflow.

## Context and Orientation

The repository enforces code quality through a set of shell and Python gates. The relevant ones:

- `scripts/check-coverage.sh` runs three gates from one instrumented coverage pass. Gate 1 checks total coverage against a 90% threshold. Gate 2 runs the CRAP comparison: it calls `scripts/cargo-crap.sh --lcov lcov.info --baseline ci/cargo-crap-baseline.json --fail-regression` and fails on a regression greater than `CRAP_REGRESSION_EPSILON` (default 0.5). Gate 3 calls `scripts/check-cc.sh` for the per-function CC ceiling. A `--cargo-crap-format github` argument makes Gate 2 emit GitHub workflow commands.
- `scripts/cargo-crap.sh` is a thin wrapper: it injects `--epsilon` from `CRAP_REGRESSION_EPSILON`, then runs `cargo crap` with fixed `--exclude` patterns (`tests/**`, `**/*_tests.rs`, `src/clients/tools/sandbox/linux.rs`).
- `scripts/check-cc.sh` is the per-function CC gate. It runs `scripts/cargo-crap.sh --format json`, then a Python block keys each function by `(file, function)` and fails when a function's CC exceeds `max(target=10, baseline CC)`. A function absent from the baseline uses the target alone. It is coverage-independent and runs in the fast local gate (`just cc-check`).
- `scripts/coverage-guard.py` rejects coverage runs whose data cannot be trusted: residual profiling artifacts after a clean, and one source file recorded under two path spellings. Its docstring currently also mentions protecting the committed baseline.
- `ci/cargo-crap-baseline.json` is a JSON object with `$schema`, `version`, and `entries`. Each entry is `{file, function, line, cyclomatic, coverage, crap}`. It is produced by `cargo crap --format json` and regenerated by the `change-risk-baseline` justfile recipe.
- `scripts/hermetic-coverage.sh` runs `cargo llvm-cov` with `HOME` pointed at an empty scratch directory so a test that reads the developer's home cannot move the measured numbers (#700).
- The justfile exposes `check` (fast local gate), `check-coverage`, `cc-check`, `change-risk-baseline`, `change-risk-report`, `check-scripts`, and `check-full`. `check-scripts` runs the fixture suites, including `coverage-guard-check`, `cc-check-fixture`, and `hermetic-coverage-check`, which test these scripts against synthetic inputs without a Rust build.
- `.github/workflows/ci.yml` defines the `Coverage` job, whose only run step is `scripts/check-coverage.sh --cargo-crap-format github`.
- `docs/guardrails/complexity-targets.md` is the authority for the CC and CRAP targets and for the enforcement mechanism. `CONTRIBUTING.md` documents which command routes cover which change class and states the rule for resolving a baseline merge conflict.

Definitions used below: **CC** (cyclomatic complexity) counts independent paths through a function. **CRAP** (Change Risk Anti-Patterns) is `CC² · (1 − coverage)³ + CC`, so a function with 100% coverage scores exactly its CC and a function with no coverage scores `CC² + CC`. A **grandfather** is a baseline entry that lets one named function exceed a target because it predates the target and is tracked for reduction.

## Plan of Work

The work splits into four milestones. Milestone 1 replaces the delta comparison with an absolute gate and leaves the baseline shape alone, so the two changes can be reviewed and verified separately. Milestone 2 shrinks the baseline and updates its consumers. Milestone 3 aligns the documentation and CI wiring with the enforced rules. Milestone 4 demonstrates acceptance.

### Milestone 1: Retire the delta comparison; add the absolute CRAP gate

In `scripts/check-coverage.sh`, delete the `CRAP_REGRESSION_EPSILON` handling, the `--cargo-crap-format` argument, and the current Gate 2 body. Replace Gate 2 with a call to a new `scripts/check-crap.sh --lcov lcov.info --baseline ci/cargo-crap-baseline.json`. Keep Gate 1 and Gate 3 exactly as they are.

Create `scripts/check-crap.sh`, mirroring the structure of `scripts/check-cc.sh`: argument parsing for `--lcov`, `--baseline`, `--threshold` (default 30), and `--warn` (default 15); a `cargo-crap` presence check that names the `just setup` remedy; a call to `scripts/cargo-crap.sh --lcov "$lcov" --format json` to produce the report; and an embedded Python block that reads the report and the baseline. The Python block must fail when a function's CRAP exceeds its allowed ceiling, where the allowed ceiling is the entry's `crap` field when present (the grandfather) and `--threshold` otherwise; and print a non-failing warning when a function's CRAP exceeds `--warn` while its CC is at or below 10. It must print a summary line in the same style as `scripts/check-cc.sh` (`CRAP gate: N functions checked, M grandfathered, K over allowed`).

Update `scripts/cargo-crap.sh`: remove the `--epsilon` injection, since nothing passes `--epsilon` after Milestone 1. Keep the `--exclude` patterns.

Update the justfile: the `check-coverage` comment and `check-coverage` recipe body should describe coverage plus complexity gates, not a regression. Add a fixture recipe `check-crap-fixture` that runs `scripts/test-check-crap.sh`, and add it to the `check-scripts` recipe's dependency list.

Create `scripts/test-check-crap.sh` after the model of `scripts/test-check-cc.sh`: it writes synthetic cargo-crap JSON reports and baseline files to a scratch directory, points `scripts/check-crap.sh` at them via a `PATH` stub for `cargo-crap`, and asserts the exit status and messages for a passing case, a failure above CRAP 30, a warning above CRAP 15 with CC ≤ 10, and a grandfathered function that would otherwise fail.

### Milestone 2: Shrink the baseline artifact and update its consumers

Redefine `ci/cargo-crap-baseline.json` as an object with `version` and `entries`, where each entry is `{file, function, cyclomatic}` and the single grandfathered function additionally carries `crap` (its allowed ceiling). Drop `$schema`, `line`, `coverage`, and the per-entry `crap` on every non-grandfathered entry.

Update the `change-risk-baseline` recipe so regeneration does the pruning. After `cargo crap --format json --output` produces a fresh report, run a small Python step (a new `scripts/prune-crap-baseline.py`, or an embedded block) that keeps only entries whose CC exceeds the target, projects each to `{file, function, cyclomatic}`, and carries forward the `crap` allowance for any `(file, function)` already present in the committed baseline with a `crap` field. Carrying the allowance forward is what makes a clean-tree regeneration a no-op and keeps the grandfather a deliberate, reviewable line rather than something a regeneration silently drops.

Confirm `scripts/check-cc.sh` needs no change: it already reads only `file`, `function`, and `cyclomatic`, and `max(CC 10, baseline CC)` is unchanged.

Re-audit `scripts/coverage-guard.py`. Keep the residual-profile and duplicate-source-root checks, which fixed real false totals (#520). Remove the docstring and any logic that exists only to protect the committed baseline, since the baseline no longer records coverage.

Update the `change-risk-report` recipe, which passes `--baseline` to `scripts/cargo-crap.sh` for a markdown diff. Either repoint it at the absolute gate's output or retire it with a note, deciding during implementation and recording the choice in the Decision Log.

### Milestone 3: Align documentation and CI wiring

Rewrite the "Enforcement" and "CRAP baseline regeneration" sections of `docs/guardrails/complexity-targets.md` so they state the enforced rules: fail above CRAP 30, warn above CRAP 15 for CC ≤ 10, the coverage-independent CC gate, and the shrunk baseline. State the explicit tradeoff the plan accepts --- a function may grow to the CC target without a CRAP gate objecting, and a within-target regression or a coverage collapse while under CRAP 30 is not gated. Reference ADR 039 and #660.

Update `CONTRIBUTING.md`: the routing table rows for per-function complexity and coverage, and the paragraph that tells contributors to take `master`'s copy of `ci/cargo-crap-baseline.json` and regenerate. Verify whether that merge-conflict rule is still needed once the artifact shrinks and becomes a no-op on a clean tree; keep or remove it with a recorded reason.

Update the justfile comments for `check-coverage`, `cc-check`, and `change-risk-baseline` to match. Update the `Coverage` job step in `.github/workflows/ci.yml` (name and comment) so it no longer says "change-risk regression", and rename the job's step only --- the job name `Coverage` is a branch-protection identifier and must not change.

### Milestone 4: Prove acceptance

Run the commands in Validation and Acceptance and record the transcripts in Artifacts and Notes.

## Concrete Steps

All commands run from the repository root `/Users/travisennis/Projects/cake/cake-2` unless noted.

Milestone 1 verification, before touching the baseline:

```
just check-coverage
```

Expected: Gate 1 passes, the new CRAP gate prints `CRAP gate: ...` and passes on `master`, Gate 3 passes, and the run ends with `PASS: All coverage gates passed`.

```
just check-crap-fixture
```

Expected: the fixture asserts pass, over-threshold failure, warning, and grandfather cases, and the recipe prints a success line.

Milestone 2 verification:

```
just change-risk-baseline
git diff --stat ci/cargo-crap-baseline.json
```

Expected: the first command rewrites the file; the second shows the shrink from roughly 1,180 entries to the CC grandfather set. Then, with the file committed:

```
just change-risk-baseline
git diff --exit-code ci/cargo-crap-baseline.json
```

Expected: no output, exit 0 --- the artifact is deterministic.

Milestone 3 verification:

```
just docs-check
just cc-check
```

Expected: the documentation corpus check passes and the CC gate reports `PASS: No cyclomatic complexity exceedances`.

Milestone 4 verification:

```
just check
just check-full
```

Expected: both print their final `... passed!` lines. `just check-full` includes `check-coverage`.

## Validation and Acceptance

Acceptance mirrors issue #658 and ADR 039. Each is stated as observable behavior.

1. Adding an enum arm to a fully covered `match` passes every gate with no baseline edit. Concretely, add an arm to a fully covered `enum` `from_str` (the #656 case), run `just check-coverage`, and observe it pass. Revert the arm before committing.
2. A full `just change-risk-baseline` on a clean checkout produces no diff. Run it after committing the shrunk baseline and observe `git diff --exit-code ci/cargo-crap-baseline.json` exit 0 with no output.
3. `ci/cargo-crap-baseline.json` holds one entry per function above the CC target and no `line`, `coverage`, or `crap` fields except the deliberate CRAP grandfather. Inspect the file: count the entries and confirm the shape.
4. A deliberately bloated function, CC 10 at 20% coverage, fails the absolute CRAP gate. Temporarily add such a function (or a fixture in `scripts/test-check-crap.sh`) and observe the failure message names it. At CC 10 and 20% coverage CRAP is about 61, well above 30.
5. `just check-coverage` passes unchanged on `master`.
6. `docs/guardrails/complexity-targets.md` states the enforced rules and matches the scripts, including the explicit tradeoff that a function may grow to the CC target without a CRAP gate objecting.

## Idempotence and Recovery

`just change-risk-baseline` is safe to run repeatedly; it cleans profile artifacts, measures under a scratch `HOME`, and rewrites the file from scratch while carrying forward deliberate grandfathers. If a regeneration drops or alters the grandfather line unexpectedly, restore it with `git checkout -- ci/cargo-crap-baseline.json` and re-run; the carried-forward design means a correct regeneration is byte-stable. `just change-risk-report` is read-only. `scripts/check-coverage.sh` and `scripts/check-crap.sh` are read-only gates. If a milestone leaves CI red, revert the branch to the last green commit; the ADR and this plan are independent of the code change and can land first.

## Artifacts and Notes

The motivating failure, from the Coverage job on PR #703 (run 37648072614), with every compared function far below the CC 10 target:

```
##[warning]SettingsLoader::merge_output_budgets CRAP=8.0 (Δ+1.0) (moved from ./src/config/settings.rs) CC=8 cov=95.7%
##[warning]ResponsesApiInputItem::from CRAP=7.0 (Δ+1.0) CC=7 cov=100.0%
##[warning]ChatMessageBuilder::push_function_call_output CRAP=2.0 (Δ+1.0) (...) CC=2 cov=100.0%
FAIL: CRAP regression detected (exit code 1)
PASS: No cyclomatic complexity exceedances
```

The same run's CC gate passed, which is the contradiction this plan removes: at 100% coverage CRAP equals CC, so `Δ+1.0` is one added branch on a function the guardrail permits to reach CC 10.

## Interfaces and Dependencies

- `ci/cargo-crap-baseline.json` (new shape): a JSON object with `version` (string) and `entries` (array). Each entry is `{file: string, function: string, cyclomatic: number}` and may carry `crap: number` as an allowed ceiling. `scripts/check-cc.sh` reads `file`, `function`, and `cyclomatic`; the new `scripts/check-crap.sh` reads `file`, `function`, `cyclomatic`, and `crap`.
- `scripts/check-crap.sh` (new): accepts `--lcov FILE`, `--baseline FILE` (default `ci/cargo-crap-baseline.json`), `--threshold N` (default 30), `--warn N` (default 15). Exits 0 when no function exceeds its ceiling, 1 when one does, and 2 on a bad invocation.
- `cargo-crap` 0.2.2: used via `scripts/cargo-crap.sh` for `--lcov`, `--format json`, and the fixed `--exclude` patterns. The absolute gate does not depend on `--fail-regression` or `--epsilon`.
- `python3`: used for the embedded gate logic and the baseline-pruning step, matching the existing `scripts/check-cc.sh` and `scripts/coverage-guard.py` dependencies.
- `scripts/test-check-crap.sh` (new): asserts the gate's pass, fail, warn, and grandfather behavior against synthetic inputs with a `cargo-crap` `PATH` stub, mirroring `scripts/test-check-cc.sh`, and is wired into `just check-scripts`.
