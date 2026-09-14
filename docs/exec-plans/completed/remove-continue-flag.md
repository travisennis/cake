# Remove the `--continue` session mode

This ExecPlan is a living document, maintained per `docs/workflow/exec-plans.md`.

## Purpose / Big Picture

Cake currently offers `--continue` to select a latest session implicitly, but that selection can restore the wrong conversation because it is based on session creation metadata rather than recent activity. It also duplicates the explicit `sessions list` and `--resume <UUID>` workflow. After this change, users discover a session UUID with `cake sessions list` and continue exactly that session with `cake --resume <UUID> <PROMPT>`. The removed spelling must be rejected before Cake creates a worktree, session, telemetry sidecar, or provider request.

The change intentionally breaks the `--continue` CLI compatibility surface as requested by issue #431. `--resume <UUID>`, `--fork [UUID]`, and `--no-session` retain their existing behavior. Telemetry no longer has a `continue` run-mode value: every explicit restoration is recorded as `resume`, so the sidecar vocabulary describes the remaining CLI modes and no new invocation can emit the retired value.

## Progress

- [x] (2026-09-14) Inspected issue #431, claimed it, confirmed the worktree was clean, and created `feat/remove-continue`.
- [x] (2026-09-14) Remove the flag, implicit latest-session run mode, restore path, and obsolete tests.
- [x] (2026-09-14) Retarget integration coverage to explicit UUID resume and add fail-fast rejection coverage for the removed option.
- [x] (2026-09-14) Update user, integration, ADR, and runbook documentation without changing unrelated session or fork semantics.
- [x] (2026-09-14) Run focused tests, formatting, and the routed `just check` gate.
- [x] (2026-09-14) Record verification and acceptance notes on issue #431, complete this plan, and move it to `docs/exec-plans/completed/` before opening a pull request.

## Surprises & Discoveries

- Observation: Clap's derived parser rejects an unknown `--continue` argument before `CodingAssistant::run`, so the existing input-error path already provides the required fail-fast behavior and prevents session setup. Evidence: unknown flags are covered by `tests/exit_codes.rs`, and `--continue` is currently declared only on `CodingAssistant`.
- Observation: `SessionTelemetryRunMode` is serialization-only and is not used to read historical sidecars. Evidence: the enum derives `Serialize` but not `Deserialize`, so deleting its `Continue` variant affects only newly written telemetry.
- Observation: The first full coverage-baseline attempt inherited `CAKE_JUDGE=off` from the shell and failed 11 existing judge tests because they expected real verdicts. Evidence: rerunning the same command with `env -u CAKE_JUDGE` passed all 1,473 tests and regenerated the baseline.
- Observation: Removing the parser field made the existing Clippy expectation for excessive booleans unnecessary. Evidence: `just check` reported an unfulfilled `clippy::struct_excessive_bools` expectation; deleting that expectation restored a clean gate.

## Decision Log

- Decision: Remove the telemetry `Continue` variant rather than preserve or remap a hidden compatibility alias. Rationale: no supported invocation can produce a distinct continue mode after the flag is deleted; explicit restoration is already represented by `Resume`, and retaining an unused serialized label would preserve dead surface. Date/Author: 2026-09-14 / Cake.
- Decision: Rely on Clap's standard unknown-argument error instead of adding custom parsing. Rationale: it fails before resource/session setup, already returns the repository's input exit code, and avoids special-case parsing for a deliberately unsupported flag. Date/Author: 2026-09-14 / Cake.
- Decision: Convert the end-to-end latest-session restore test to named UUID resume, rather than remove all restore coverage. Rationale: the acceptance criteria require the explicit `sessions list` plus `--resume <UUID>` workflow to remain functional. Date/Author: 2026-09-14 / Cake.

## Outcomes & Retrospective

Issue #431 is implemented. Cake no longer declares or dispatches the implicit latest-session restore option; Clap rejects the retired spelling before run preparation, and the dedicated latest-session restore helper, cross-directory diagnostic, data-directory helper, run-mode variant, and telemetry label were deleted. Explicit UUID resume remains on the shared restore path, fork-latest and session listing retain latest-session discovery, and newly written telemetry uses only `new`, `resume`, or `fork`.

The focused unit and integration tests passed, including the named-session provider round trip, parser rejection with exit code 3 and no session creation, remaining session-mode construction, and telemetry label serialization. `cargo fmt` and `env -u CAKE_JUDGE just check` passed. The coverage-derived cargo-crap baseline was regenerated with the same environment normalization. Documentation now directs users through `sessions list` and `--resume <UUID>`; no session transcript migration was needed.

The main lesson is that removing a CLI mode must remove its entire compatibility surface, not just the Clap field: restore construction, diagnostics, telemetry vocabulary, generated complexity records, tests, and user workflow documentation all needed retargeting. The ambient `CAKE_JUDGE=off` setting is a relevant local verification hazard and was explicitly removed for full-suite commands.

## Context and Orientation

`src/main.rs` derives the root Clap parser and now groups only `resume`, `fork`, and `no_session` as mutually exclusive session options. `src/cli/run_mode.rs` translates those parsed arguments into `RunMode`; implicit latest-session restoration is no longer a run mode. `src/cli/session_factory.rs` retains the shared `restored_run` constructor for UUID resume and leaves `DataDir::load_latest_session` for fork-latest and session listing.

`tests/session_modes.rs` exercises restore/fork provider round trips. `src/main_tests.rs` covers argument-to-run-mode translation. `tests/exit_codes.rs` covers Clap and application input errors. `README.md`, `docs/integrations.md`, `docs/configuration.md`, and the runbooks describe the user workflow. ADRs and completed plans contain historical or durable references that need to distinguish the retired option from the current workflow; no session file format migration is needed.

## Plan of Work

First, remove the retired option from the parser group, delete its field and help text, and remove the corresponding validation entry. Update the model-resolution comment so it names only resume and fork. Then simplify `RunMode` to NewSession, Ephemeral, Resume, ForkLatest, and Fork, deleting parsing and hook/telemetry arms for implicit latest-session restoration. Remove that restore dispatch, its dedicated helper and error, and their unit tests from `src/cli/session_factory.rs`. Keep `DataDir::load_latest_session` because `--fork` without a value and session listing still use it.

Next, retarget the restore integration test to `--resume SESSION_ID`, update its comments and test names, and replace tests that asserted conflicts involving `--continue` with a subprocess test asserting the unknown option exits 3, emits no stdout, and does not create session files. Add focused run-mode tests for the remaining restore modes and telemetry mapping if needed so the removal of `Continue` is directly tested.

Finally, replace every current user-facing `--continue` example with `cake sessions list` followed by `cake --resume <UUID>`. Update the integration contract, configuration wording, session-analysis runbook, debugging runbook, and durable ADR text. Historical completed plans may retain references only when explicitly documenting the behavior that existed during that plan; all live documentation and all code/test/fixture references must use the explicit UUID workflow. Confirm with a repository search that no retired symbols or current `--continue` guidance remains outside intentionally historical records, and inspect the final diff for unrelated changes.

## Concrete Steps

All commands run from `/Users/travisennis/Projects/cake/cake-0`.

1. Edit the parser, run-mode, session factory, tests, telemetry enum, and documentation listed above. Keep `DataDir::load_latest_session` and the `ForkLatest` path unchanged.

2. Run focused tests while iterating:

   ```
   cargo test test_run_mode
   cargo test session_factory
   cargo test --test session_modes
   cargo test --test exit_codes
   ```

   Expected result: all selected tests pass; the removed option is rejected as an input error, named resume still reaches the mock provider, and fork behavior remains covered.

3. Format and inspect the retired-surface search:

   ```
   cargo fmt
   rg -n --hidden --glob '!target' --glob '!.git' -- '--continue' README.md docs src tests ci
   ```

   Expected result: only the intentional parser-rejection tests and this plan's validation text mention the removed spelling; no current user guidance or implementation declaration remains.

4. Run the routed Rust gate:

   ```
   just check
   ```

   Expected result: formatting, strict Clippy, tests, import/dependency checks, instruction-size, module-size, and glossary checks pass.

5. Before handoff, add issue #431 acceptance and verification notes, fill this plan's Outcomes & Retrospective, move it with `git mv docs/exec-plans/active/remove-continue-flag.md docs/exec-plans/completed/remove-continue-flag.md`, and review `git diff --check`.

## Validation and Acceptance

A human can run `cake --help` and see no `--continue` option. Running `cake --continue "prompt"` exits with input-error status 3, prints the parser error on stderr, produces no stdout, and does not start a provider or create a session. Running `cake sessions list` exposes session UUIDs, and `cake --resume <UUID> "prompt"` restores that exact transcript; the integration test demonstrates the restored history and new prompt reach the provider. `--fork` without a value, `--fork <UUID>`, and `--no-session` retain their existing tests and semantics. Newly written telemetry uses `new`, `resume`, or `fork` only; the run-mode unit test protects that `Resume` serializes as `resume` and that no retired telemetry label is emitted.

## Idempotence and Recovery

Source and documentation edits are safe to repeat only after rereading the current file and checking the exact match. `cargo fmt` and all test commands are repeatable. If a focused test fails, preserve the failure output, fix only the relevant implementation or expectation, and rerun that test before the full gate. If plan archival is interrupted, leave the plan in `active/` until its outcomes are complete, then retry the same `git mv`; do not create duplicate plans. No persisted session files or user data are modified by this change.

## Artifacts and Notes

The important proof artifacts are the focused test output, `cake --help`/unknown-option behavior, the retired-surface search, the final diff, and the `just check` result. The issue acceptance comment will summarize these results before the pull request is opened.

## Interfaces and Dependencies

The root parser remains `crate::CodingAssistant` and exposes `--resume <UUID>`, `--fork [UUID]`, and `--no-session`. `crate::cli::RunMode::from_cli` parses UUID resume and optional-UUID fork; `crate::cli::session_factory::build_client_and_session` routes `Resume` through `restored_run`, `ForkLatest` and `Fork` through `forked_run`, and new/ephemeral runs through `new_run`. `crate::session_telemetry::SessionTelemetryRunMode` retains `New`, `Resume`, and `Fork`, with `RunMode::telemetry_mode` mapping the corresponding modes. `crate::config::DataDir::load_latest_session` remains required by fork-latest discovery and `sessions list`. No dependency changes are expected.
