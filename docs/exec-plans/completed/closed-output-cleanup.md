# Preserve cleanup after closed output

This ExecPlan is a living document maintained according to `docs/workflow/exec-plans.md`.

## Purpose / Big Picture

When a consumer closes Cake's stdout while a run is still active, Cake must stop producing output without aborting the process from inside the output writer. Returning a typed closed-output signal through the normal asynchronous run path lets `PreparedRun` leave scope, so `WorktreeGuard` restores the original directory and removes an unchanged temporary worktree. The user-visible result remains a silent successful exit, and stream-json records already written before closure keep their existing shape and order. Text and JSON final responses use the same deliberate fallible stdout path.

## Progress

- [x] (2026-09-06) Inspect issue #415, integration contracts, architecture, interrupt ADR, output/replay/agent/hook code, and existing tests.
- [x] (2026-09-06) Claim issue #415 and create `fix/closed-output-cleanup` from `origin/master`.
- [x] (2026-09-06) Add the typed closed-output signal and propagate fallible stream and hook output through normal return paths.
- [x] (2026-09-06) Make text and JSON stdout writes fallible and add deterministic closed-writer tests.
- [x] (2026-09-06) Add replay and worktree cleanup regression coverage and update the integration contract.
- [x] (2026-09-06) Run focused tests, `just check`, direct Markdown checks, and the three preflight review passes.
- [x] (2026-09-06) Update issue acceptance notes and move this plan to `docs/exec-plans/completed/`; commit, push, and PR opening are the remaining handoff operations.

## Surprises & Discoveries

- Observation: The existing replay subprocess test already proves that a closed stream exits 0 and preserves the first record, but the old implementation achieved this with `process::exit(0)`, so it did not exercise destructor cleanup. Evidence: `tests/replay.rs::replay_exits_cleanly_when_consumer_closes_stdout` and `src/cli/output.rs` before this change.
- Observation: Agent streaming callbacks were infallible and hook event sinks were also infallible, so both boundaries had to become fallible for a closed stdout error to reach `CodingAssistant::run`. Evidence: `src/clients/agent_observer.rs`, `src/hooks.rs`, and `src/main.rs`.
- Observation: `WorktreeGuard` cleanup is easiest to verify before any provider request by closing stream-json stdout before the first task record, using a fixture repository with an upstream branch. Evidence: the new `tests/stdin_handling.rs` subprocess test.

## Decision Log

- Decision: Represent a closed stdout consumer with the typed `ClosedOutput` error and handle it only at the top-level CLI return boundary. Rationale: the output writer must not terminate the process, and matching the typed error at `main` preserves exit 0 and suppresses diagnostics after all run-local destructors execute. Date/Author: 2026-09-06 / cake.
- Decision: Keep the existing infallible `with_streaming_json` test fixture API under `cfg(test)` and add a fallible production callback method. Rationale: existing agent unit tests collect records with closures returning `()`, while the production CLI needs error propagation; this avoids unrelated test churn. Date/Author: 2026-09-06 / cake.
- Decision: Propagate hook-event sink errors as well as ordinary agent stream callback errors. Rationale: hook events are part of stream-json output and must not bypass the same closed-output cleanup path. Date/Author: 2026-09-06 / cake.
- Decision: Preserve `process::exit(130)` in the second-interrupt escape hatch. Rationale: the interrupt ADR explicitly requires a hard exit if graceful cleanup hangs; output cancellation is unrelated. Date/Author: 2026-09-06 / cake.

## Outcomes & Retrospective

Implemented and validated the typed closed-output return path. Stream-json, text, and JSON stdout writes now map `BrokenPipe` to a private typed signal; agent and hook callbacks return that signal through `CodingAssistant::run`; `main` maps it to silent exit 0 only after run-local values, including `WorktreeGuard`, leave scope. Replay preserves records already written and returns the same signal when its consumer closes stdout. The second-interrupt `process::exit(130)` remains unchanged.

Verification completed on 2026-09-06: the focused output tests passed (6 tests), the hook sink regression passed, replay passed all 15 integration tests, stdin/worktree passed all 16 integration tests, `just check` passed (1,398 unit/integration tests plus strict Clippy and repository lints), and direct `panache` format/lint checks passed for `docs/integrations.md`. No known limitations remain within issue #415; platform-specific CI checks remain the responsibility of CI.

## Context and Orientation

`src/cli/output.rs` owns user-facing text, JSON, and stream-json rendering. `src/clients/agent_observer.rs` fans stream records to persistence and the live stream callback. `src/hooks.rs` emits hook records through the CLI-provided hook sink. `src/cli/replay.rs` re-emits persisted records as stream-json. `src/main.rs` owns `CodingAssistant::run`, top-level exit mapping, and `WorktreeGuard`. The output contract in `docs/integrations.md` requires a closed stream consumer to be treated as a successful silent cancellation. The second-interrupt hard exit in `src/main.rs` is intentionally retained under the decision in `docs/adr/011-interrupt-handling.md`.

## Plan of Work

`src/cli/output.rs` defines `ClosedOutput`, maps stdout `BrokenPipe` errors to that type, and makes stream-json, text, and JSON writes return `anyhow::Result`. It provides small writer seams so deterministic tests can use a writer that returns `BrokenPipe` without manipulating the process stdout. Stream-json exit-result handling preserves the typed signal instead of swallowing it.

`src/clients/agent_observer.rs` and `src/clients/agent.rs` carry callback results through record emission. The normal test-only callback helper remains available for existing record-capture tests, while the CLI installs the fallible callback. `src/hooks.rs` makes hook event sink errors return through hook aggregation, and `src/main.rs` maps a closed-output error to exit 0 after `CodingAssistant::run` has returned and its guards have dropped. The intentional second-interrupt `process::exit(130)` remains unchanged.

`src/cli/replay.rs` returns output errors from successful record emission and lets a closed output replace a replay error only when writing the error record itself is impossible. `tests/replay.rs` retains the subprocess closed-consumer coverage, `src/cli/output.rs` tests all three writer modes with a deterministic failing writer, and `tests/stdin_handling.rs` verifies an unchanged worktree is removed after stream-json stdout closes. `docs/integrations.md` states the resulting all-format closed-stdout behavior.

## Concrete Steps

From `/Users/travisennis/Projects/cake/cake-0`, run focused tests for `cli::output::tests`, the hook sink regression, the complete `replay` integration test, and the complete `stdin_handling` integration test. Then run `just check`, inspect the final diff, update issue #415 acceptance notes, move this plan to `docs/exec-plans/completed/`, commit the staged repository paths, push `fix/closed-output-cleanup`, and open a PR with `Closes #415`.

Expected closed-output behavior is exit status `0`, no BrokenPipe or panic diagnostic, records already accepted by the pipe unchanged, and a worktree count returning to its pre-run value.

## Validation and Acceptance

A successful implementation must compile with the repository toolchain; pass the focused output and hook tests; pass replay's complete integration suite including the closed-consumer case; pass stdin/worktree integration including the new guard-cleanup case; and pass `just check`. A code search must show no output-path `process::exit`; the only remaining ordinary-run hard exit is the intentional second-interrupt path. The integration contract must continue to describe exit 0 and silent closed-consumer behavior.

## Idempotence and Recovery

All tests create isolated temporary environments and can be rerun. The writer tests do not touch process stdout. If a test leaves a temporary worktree, its `TempDir` owns the fixture and the next run uses a fresh fixture. If final validation finds a source issue, fix only the affected repository paths, rerun the narrow test, then rerun `just check`. Do not merge or close the pull request; issue closure remains tied to a later merge.

## Artifacts and Notes

The key evidence is the typed error assertion in `src/cli/output.rs`, the replay subprocess's first-record and exit assertions in `tests/replay.rs`, and the worktree count assertion in `tests/stdin_handling.rs`. The final issue comment and pull request will record exact verification commands and the documentation assessment.

## Interfaces and Dependencies

`CliOutputSink::write_stream_record`, `CliOutputSink::write_json_value`, and the text renderer return `anyhow::Result`; `ClosedOutput` is the typed normal-cancellation signal. `Agent::with_fallible_streaming_json` and `AgentObserver::stream_record` carry callback errors through the agent loop. `HookContext::hook_event_sink` returns `anyhow::Result` so hook records cannot bypass output failure propagation. `CodingAssistant::run` returns the signal to `main`, which maps it to `exit_code::code::SUCCESS` only after local state has unwound. No provider, session record, or stream record schema is changed.
