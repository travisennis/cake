## Establish the manual dependency sweep

This ExecPlan is a living document, maintained per `docs/workflow/exec-plans.md`. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective are kept current as work proceeds.

## Purpose / Big Picture

Cake needs a repeatable dependency-maintenance process that a maintainer can run from a clean checkout today. The process must discover every version-bearing surface, preserve Dependabot's ownership of Cargo and GitHub Actions, detect toolchain updates without guessing, and produce output that tells a maintainer whether there is no work, safe work, or work that needs human review. It must not merge or automerge updates, and it must remain usable by the future scheduler described by issue #231.

After this change, a maintainer can run one documented command sequence. The deterministic report lists the owning file, owner, current value, and authoritative source for each surface. Given the official Rust stable channel data, it detects the known Rust 1.97.1 to 1.98.0 update only after the seven-day cooldown. A fixture proves the same result without network access.

## Progress

- [x] (2026-08-27) Inspect issue #80, related issues, repository pins, scheduled checks, and existing toolchain enforcement.
- [x] (2026-08-27) Create the feature branch and claim issue #80.
- [x] (2026-08-27) Choose the manual-only design; leave scheduling to #231 and keep Dependabot as the Cargo and GitHub Actions owner.
- [x] (2026-08-27) Add the deterministic dependency-surface report, isolated fixture tests, and `just` entry points; wire the fixture suite into CI.
- [x] (2026-08-27) Document the inventory, report schema, cooldown, review, grouping, bounds, and manual GitHub workflow in `docs/automations/dependency-sweep.md` and reconcile `docs/automations/README.md` and `docs/dependencies.md`.
- [x] (2026-08-27) Run focused checks, documentation checks, and the routed final gate; review the diff.
- [x] (2026-08-27) Update issue acceptance notes and add verification evidence before opening the pull request.

## Surprises & Discoveries

- The current official Rust stable channel is dated 2026-08-20 and contains Rust 1.98.0. This matches issue #80's demonstration case and is exactly seven days old on 2026-08-27.
- The repository already pins local tools and checks `.mise.toml` against `rust-toolchain.toml`; the new report must read these existing owners rather than add a second version manifest.
- `scheduled.yml` deliberately uses nightly for `cargo-udeps` and an older MSRV pin in a named exception. The procedure will document both as explicit review surfaces rather than silently treating them as synchronized project-toolchain pins.
- TOML stores target-specific Cargo dependencies under `manifest["target"]`, and several workflow action references use mapping-form `uses:` keys. The report now handles both shapes instead of relying on the first scan's assumptions.
- `just` reports non-zero recipe statuses as errors unless `[no-exit-message]` is used. The report keeps exit codes 10 and 20 while suppressing misleading wrapper noise.

## Decision Log

- Decision: Implement the report in Python using only the standard library. Rationale: JSON output, TOML parsing, fixture tests, and explicit exit categories are needed, while adding a Cargo or Python package would add an unrelated dependency surface. Date/Author: 2026-08-27 / Codex.
- Decision: Make local inventory deterministic and accept a saved official Rust channel file as input. Rationale: network discovery must not make the report's output nondeterministic or hide an unavailable release source; the documented `curl` step supplies authoritative input, and fixtures can prove update detection offline. Date/Author: 2026-08-27 / Codex.
- Decision: Keep this issue review-only. The report may identify actionable work, but the procedure requires upstream source review and a separate bounded PR per domain; it never edits, merges, or automerges dependency updates itself. Rationale: this is the issue's stated direction and preserves #231's scheduler decision boundary. Date/Author: 2026-08-27 / Codex.
- Decision: Use report status values `no-work`, `actionable`, and `review-required`, with exit codes 0, 10, and 20. Rationale: both humans and a future wrapper need a stable machine-readable branch without interpreting prose. Date/Author: 2026-08-27 / Codex.
- Decision: Keep `--json` out of the command interface because the report has one output format. Rationale: a no-op flag would imply an unsupported alternate renderer; JSON remains the default and only output. Date/Author: 2026-08-27 / Codex.
- Decision: Keep the fixture repository in `scripts/test-dependency-sweep.py` rather than commit a second fixture tree. Rationale: the isolated temporary repository keeps release behavior stable without duplicating a large source tree, while the live inventory test protects the current repository surface. Date/Author: 2026-08-27 / Codex.
- Decision: Keep `dependency-sweep-check` as a standalone recipe and CI `changes` step, not a dependency of `just ci`. Rationale: the repository's other fixture suites follow this pattern, and the full gate should not run the same standard-library test twice. Date/Author: 2026-08-27 / Codex.

## Outcomes & Retrospective

The change delivers a manual dependency-sweep specification and a read-only report. `scripts/dependency-sweep.py` inventories 113 current records across Cargo, Rust, mise, Cargo-installed tools, workflow-installed tools, and all GitHub Action references, including target-specific dependencies and mapping-form action steps. It identifies the current official Rust channel's 1.97.1 to 1.98.0 update after the seven-day cooldown, and it fails closed for missing, ambiguous, too-new, major, or drifted inputs while identifying human-owned exceptions. `scripts/test-dependency-sweep.py` proves no work, actionable work, cooldown, security override, major-version review, drift, malformed release data, and the current inventory without network access. The procedure defines Dependabot ownership, manual review, bounded PRs, issue routing, stable automation notes, and the boundary with scheduled CI and #231.

Verification completed on 2026-08-27: the 8-test `just dependency-sweep-check` suite passed; the live official channel report returned status `actionable`, exit 10, current `1.97.1`, candidate `1.98.0`, and age 7; focused `panache format --check`, `panache lint`, `python3 -m py_compile`, and `git diff --check` passed; and `just ci` passed with exit 0. `just check-deps` remains blocked by the existing RUSTSEC-2026-0258 advisory for transitive `h2` 0.4.15, patched in 0.4.16; this issue intentionally leaves Cargo/lockfile updates to Dependabot and the security review path. `just --fmt --check` remains blocked by pre-existing formatting differences elsewhere in `justfile`; no unrelated formatting was applied.

No dependency pin, lockfile, action ref, or release was changed. The live report intentionally identifies Rust 1.98.0 but does not apply it. The next action is to review that Rust update and the h2 advisory as separate dependency-maintenance work, then open bounded PRs only after upstream source review.

## Context and Orientation

Cake is a Rust binary repository. Cargo dependencies and GitHub Actions are already owned by Dependabot through `.github/dependabot.yml`. The dependency posture in `docs/dependencies.md` requires exact developer and CI tool pins, one owner per version-bearing input, a one-week cooldown for new tooling releases, upstream source-diff review, and prompt handling for security updates. The current project Rust version is `1.97.1` in `rust-toolchain.toml`; `.mise.toml` restates it; the stable Rust workflow inputs in `.github/workflows/ci.yml`, `.github/workflows/release.yml`, and most jobs in `.github/workflows/scheduled.yml` are synchronized by `scripts/check-rust-toolchain.sh`. The scheduled MSRV job uses `1.91.0` and the unused-dependency job uses the floating nightly channel by deliberate exception.

The report will discover, without creating a duplicate authority, the Cargo manifest and lockfile, action references, Rust toolchain and its synchronized workflow copies, `.mise.toml` tools, `cargo install` commands in `justfile` and workflows, `taiki-e/install-action` tool pins, the MSRV exception, and other workflow tool inputs. It will extract values from those files and print an inventory plus findings. The optional official channel file is the Rust distribution `channel-rust-stable.toml`; its `[pkg.rust].version` and top-level `date` are the authoritative release value and publication date.

## Plan of Work

Add `scripts/dependency-sweep.py`. It will locate the repository from the script path, parse the owned TOML files with Python's `tomllib`, scan workflow and Just recipes with narrow regular expressions, and emit stable JSON with a schema version, inventory, findings, and status summary. It will accept `--rust-channel PATH` and `--as-of YYYY-MM-DD`; without a channel file it will report the local inventory and a review-required missing release source rather than inventing a candidate. It will compare the project Rust pin with `[pkg.rust].version`, require seven full calendar days unless the caller explicitly records a security override, and fail closed for malformed or ambiguous release data. The script will keep version extraction separate from ownership metadata so no second authoritative pin exists.

Add `scripts/test-dependency-sweep.py`, which builds a small isolated repository for release-state tests and scans the real repository for the complete inventory. The tests cover current inventory, a no-work result, the 1.97.1 to 1.98.0 update after cooldown, a release still inside cooldown, a major update, security override, pin drift, and malformed release data. Add `dependency-sweep` and `dependency-sweep-check` to `justfile` and run the fixture suite in the CI `changes` job, without changing existing dependency pins or Dependabot ownership.

Write `docs/automations/dependency-sweep.md`. It will define the inventory table, authoritative upstream sources, the exact clean-checkout command sequence, the report JSON and exit statuses, Rust release acquisition, cooldown and security rules, Dependabot handling, upstream review requirements, grouping and work bounds, the PR and issue paths, stable `Cake automation note:` comments, failure behavior, and review/merge rules. It will state that the ordinary `.github/workflows/scheduled.yml` remains non-mutating CI: advisories, unused-dependency, MSRV, and documentation checks continue there, while its non-gating `cargo outdated` job is an informational signal and does not create a PR. Update `docs/automations/README.md` to link the procedure and remove the unresolved #80 wording.

## Concrete Steps

All commands run from `/Users/travisennis/Projects/cake`.

```
python3 scripts/dependency-sweep.py --help
python3 scripts/test-dependency-sweep.py
just dependency-sweep-check
just dependency-sweep --rust-channel /tmp/cake-rust-channel.toml --as-of 2026-08-27
panache format --check docs/automations/dependency-sweep.md docs/automations/README.md docs/dependencies.md docs/exec-plans/completed/dependency-maintenance.md --quiet
panache lint docs/automations/dependency-sweep.md docs/automations/README.md docs/dependencies.md docs/exec-plans/completed/dependency-maintenance.md --quiet
git diff --check
just ci
```

For a live manual run, save the official channel before invoking the report:

```
curl --fail --silent --show-error https://static.rust-lang.org/dist/channel-rust-stable.toml > /tmp/cake-rust-channel.toml
just dependency-sweep --rust-channel /tmp/cake-rust-channel.toml --as-of "$(date -u +%F)"
```

Expected machine-readable output contains exactly one top-level `status` from `no-work`, `actionable`, or `review-required`. The fixture's known update has `status: actionable`, current `1.97.1`, candidate `1.98.0`, publication date `2026-08-20`, and age `7`; the same candidate with an earlier as-of date is `review-required` and cannot be applied.

## Validation and Acceptance

A maintainer can run the documented sequence from a clean checkout without undocumented local configuration. JSON parses with the standard library, includes all inventory surfaces and their owner and authority, and has a clear status. An isolated fixture proves no work, actionable work, cooldown deferral, major-version review, drift detection, security override, and fail-closed ambiguity. The live channel check identifies Rust 1.98.0 from the current 1.97.1 pin. Dependabot remains named as owner for Cargo and GitHub Actions. The procedure describes one bounded reviewable PR per logical domain for safe work, an issue or stop for ambiguous or high-risk work, required checks, and no merge or automerge. Documentation checks and the Rust/configuration/CI final gate pass.

## Idempotence and Recovery

The report and fixture tests are read-only and safe to repeat. The saved channel file is disposable and can be replaced by a fresh download. If download or parsing fails, retain the report as `review-required`, do not update a version, and ask a maintainer to retry or inspect the source. Documentation edits can be rerun through format and lint checks. No Cargo lockfile, dependency pin, workflow action, or release is changed by this issue.

## Artifacts and Notes

The final plan update records the report command, fixture result, live Rust channel result, documentation checks, the known advisory-check blocker, and the `just ci` result here before the pull request.

Evidence:

```
just dependency-sweep-check                  # 8 tests passed
just dependency-sweep --rust-channel ...     # actionable, exit 10; Rust 1.97.1 -> 1.98.0, age 7
just ci                                     # exit 0
just check-deps                             # exit 1: RUSTSEC-2026-0258, h2 0.4.15
just --fmt --check                          # exit 1: pre-existing justfile formatting drift
```

## Interfaces and Dependencies

The stable interface is the command `python3 scripts/dependency-sweep.py` and its JSON report. The report uses schema version `1` and status values `no-work`, `actionable`, and `review-required`; exit codes are 0, 10, and 20 respectively. `just dependency-sweep` is a convenience wrapper, and `just dependency-sweep-check` runs the offline fixture suite. The implementation uses Python 3.11+'s standard-library `tomllib`, `argparse`, `datetime`, `json`, `pathlib`, and `re` modules. It reads existing authority files and does not introduce a runtime or Cargo dependency.
