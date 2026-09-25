## Add a SessionEnd Hook for One-Shot Invocation Cleanup

This ExecPlan is a living document, maintained per `docs/workflow/exec-plans.md`. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept current as work proceeds.

## Purpose / Big Picture

Cake's existing `Stop` and `ErrorOccurred` hooks run only after the provider send resolves. A project that reports `working` from `SessionStart` or `UserPromptSubmit` therefore has no reliable lifecycle cleanup event when prompt processing fails before send or when a turn is interrupted. This change adds a final `SessionEnd` command hook with a small, stable reason so an external host can release authority on every graceful Cake invocation path.

After this work, a project can configure a `SessionEnd` hook that runs once with `reason` set to `success`, `error`, or `interrupted`. The command receives the same common identity and location fields as other lifecycle hooks, and a broken final reporter cannot change the result Cake already produced.

How to verify it works:

- Run `cargo test session_end` and observe the focused parser, payload, dispatch, exit-path, and failure-tolerance tests pass.
- Run `just check` and `just cc-check` and observe the repository gate and complexity ratchet pass.
- Configure a `SessionEnd` script that appends its stdin to a file, run successful and failing Cake invocations against a local mock provider, and observe one record per invocation with the expected reason.
- On Unix, interrupt a delayed local-provider run and observe one `interrupted` record before exit code 130.

## Progress

- [x] (2026-09-25T12:45Z) Claimed issue 542, created `feat/session-end-hook` from current `origin/master`, and recorded ADR 037.
- [ ] Add the `SessionEnd` event, typed reason, and focused configuration and runner tests.
- [ ] Dispatch the event once for setup failure, successful turn, failed turn, and graceful interrupt while preserving `task_complete` as the final task record.
- [ ] Update hook protocol, configuration, and Herdr integration documentation.
- [ ] Run focused tests, manual process checks, `just check`, and `just cc-check`.
- [ ] Run preflight, commit, push, open the pull request, and record acceptance evidence on issue 542.

## Surprises & Discoveries

- Observation: `execute_agent_turn` currently collapses pre-send setup errors and terminal-record write errors into the same `anyhow::Error`. Evidence: `?` on `session_start`, `user_prompt_submit`, record emission, and `handle_agent_turn_result` all return from the same function in `src/main.rs`.
- Observation: `task_complete` last is an existing stream/session ordering contract even though `SessionEnd` is conceptually last. Evidence: the completed append-only session-management ExecPlan requires both sinks to receive `task_complete` last, and current stream tests inspect the final record.
- Observation: A failed turn in stream-json mode normally exits 0, so `SessionEnd` must report the turn's `error` reason rather than infer the reason from the process exit code. Evidence: `CliOutputSink::stream_json_exit_result` suppresses ordinary in-stream errors.
- Observation: The second-interrupt handler is an intentional hard-exit escape hatch and can bypass a hung `SessionEnd` subprocess. Evidence: `handle_interrupt` spawns a task that calls `std::process::exit(130)` on the next interrupt.

## Decision Log

- Decision: Use a typed `SessionEndReason` serialized as `reason` with the exact values `success`, `error`, and `interrupted`. Rationale: hosts need a stable three-state classifier, while free-form strings would shift protocol interpretation to every hook author. Date/Author: 2026-09-25 / Codex.
- Decision: Dispatch `SessionEnd` after the send result or setup failure is known, but before the terminal `task_complete` record. Rationale: this is post-result and still before process exit, while preserving the established rule that `task_complete` is the final task record. Date/Author: 2026-09-25 / Codex.
- Decision: Keep normal `fail_closed` parsing and tracing inside `HookRunner`, but have the CLI ignore the returned `SessionEnd` error. Rationale: final reporters observe and release external state; they must not replace the turn outcome they are reporting. Date/Author: 2026-09-25 / Codex.
- Decision: Limit the runtime guarantee to paths where Cake has loaded a valid hook set and reaches graceful cleanup. Rationale: malformed hook configuration cannot execute reliably, and `SIGKILL`, crashes, and the second-interrupt hard exit are outside an in-process hook's control. Date/Author: 2026-09-25 / Codex.

## Outcomes & Retrospective

## Context and Orientation

Cake is a one-shot Rust CLI: a root invocation creates a session and runs one agent turn in `CodingAssistant::run` in `src/main.rs`. `CodingAssistant::execute_agent_turn` first runs `SessionStart` and `UserPromptSubmit`, emits task-start records, and calls `Agent::send`. It then passes the send result to `handle_agent_turn_result`, which invokes `Stop` for success or cut-off, invokes `ErrorOccurred` for other failures, and emits the terminal `task_complete` record.

Hook configuration is loaded by `HooksLoader` in `src/config/hooks.rs`. `HookEvent` is the accepted event-name enum. `HookRunner` in `src/hooks.rs` builds the versioned JSON payload, runs matching command hooks concurrently, records their invocation, and aggregates decisions. Hooks are trusted commands and run outside the model tool sandbox, as documented in `docs/security.md`.

A graceful SIGINT or SIGTERM races the turn future in `CodingAssistant::run`. The signal branch calls `handle_interrupt`, which records an interrupted task and returns an error classified as exit 130. A second signal exits immediately so cleanup cannot hang forever.

The common hook payload has `version`, `session_id`, `task_id`, `transcript_path`, `cwd`, `hook_event_name`, `model`, and `timestamp`. `SessionEnd` adds only `reason`. Matchers remain limited to source-bearing events, so `SessionEnd` accepts no matcher.

ADR 037 records the durable decision. The current public contracts are `docs/configuration.md` and `docs/integrations.md`; `docs/integrations/herdr.md` is a worked consumer that should release pane lifecycle authority on this event.

## What We're NOT Doing

This change does not add process-level cleanup for `SIGKILL`, process crashes, aborts, or the existing second-interrupt hard exit.

This change does not emit lifecycle hooks for CLI subcommands that do not run an agent turn.

This change does not allow a `SessionEnd` decision, timeout, invalid JSON, or `fail_closed` setting to alter the turn result, output, or exit code.

This change does not add matchers, new hook configuration versions, a new dependency, or a new persisted record type. `SessionEnd` uses the existing `hook_event` record and command-hook protocol.

## Milestones

### Milestone 1: Recognize and Dispatch the Event

Overview: At the end of this milestone, `hooks.json` accepts `SessionEnd`, `HookRunner` sends the documented reason, and focused tests prove the event name and payload.

Plan of Work: Add `HookEvent::SessionEnd` to configuration parsing and `as_str`; keep it out of `has_source`. Add a copyable `SessionEndReason` in `src/hooks.rs` with `Success`, `Error`, and `Interrupted`. Add `HookRunner::session_end`, which builds `{"reason": reason.as_str()}` over the common payload and uses the existing `run_and_aggregate` path. Add a config test that loads a matcher-free `SessionEnd` entry and rejects a matcher. Add a runner test whose command validates the common fields and reason on stdin and whose event sink observes a `SessionEnd` hook record.

Concrete Steps:

Run from `/Users/travisennis/Projects/cake/cake-0`:

```
cargo test session_end
```

Expected: parser, payload, and runner tests pass.

Validation and Acceptance: The event round-trips through the loader, has no source matcher, and sends exactly one of the three documented reason strings.

Idempotence and Recovery: These are additive enum and method changes. Re-running the focused test is safe; if compilation fails, check the exhaustive `HookEvent` matches in configuration and runner code.

### Milestone 2: Cover Every Graceful Root Exit Path

Overview: At the end of this milestone, one configured `SessionEnd` command runs for a successful send, a failed send, a pre-send setup failure, and a first graceful interrupt. It runs before `task_complete`, and its own failure cannot change the result.

Plan of Work: Wrap pre-send setup in `execute_agent_turn`; on setup error, invoke a best-effort `run_session_end_hook` with `Error` and return the original error. After `Agent::send` resolves, classify the send result and invoke the same helper before Stop or ErrorOccurred so `SessionEnd` remains the final lifecycle hook before `task_complete`. Pass the hook runner into an async `handle_interrupt` and invoke the helper with `Interrupted` before the interrupted task record. The helper logs any runner error and returns no result.

Add focused tests with a capturing hook-event sink. Prove success and failed send each produce one `SessionEnd`; a `SessionStart` failure produces one error event; `handle_interrupt` produces one interrupted event; and invalid JSON or a failing fail-closed final hook still leaves the normal task outcome intact.

Concrete Steps:

```
cargo test session_end
cargo test handle_agent_turn
cargo test handle_interrupt
```

Expected: all focused tests pass, including unchanged Stop and ErrorOccurred behavior.

Validation and Acceptance: Each test asserts one `SessionEnd` event rather than only successful process exit. The failed-final-hook tests assert the original success or error completion record remains present.

Idempotence and Recovery: The dispatch points are explicit branches. If a future path returns before one of them, add a focused regression test before changing the shared flow.

### Milestone 3: Document and Verify the Contract

Overview: At the end of this milestone, users and lifecycle hosts can configure and understand the event, and repository gates pass.

Plan of Work: Add `SessionEnd` to the supported event list in `docs/configuration.md`. Define its common fields, `reason` values, once-per-invocation behavior, ordering, best-effort semantics, and unavoidable hard-exit cases in `docs/integrations.md`. Update `docs/integrations/herdr.md` so its reporter calls `herdr pane release-agent` from `SessionEnd`, and remove the obsolete no-exit-event limitation.

Run:

```
cargo fmt --check
cargo test session_end
just check
just cc-check
```

Manual process verification should use a temporary project hook that appends stdin to a file and a local mock provider. Confirm one `success` record, one `error` record for a provider failure, and one `interrupted` record for SIGTERM.

Validation and Acceptance: Documentation names every reason value and explains that `task_complete` remains the final task record. A malformed or failing SessionEnd hook leaves Cake's original result unchanged.

Idempotence and Recovery: Documentation edits and checks can be repeated. Manual fixtures live in temporary directories and should not be committed.

## Concrete Steps

All commands run from `/Users/travisennis/Projects/cake/cake-0`.

1. Create the feature branch and managed records:

   ```
   just claim 542
   just branch feat/session-end-hook
   ```

2. Implement and test the event incrementally:

   ```
   cargo test session_end
   cargo test handle_agent_turn
   cargo test handle_interrupt
   ```

3. Format and run the required gates:

   ```
   cargo fmt
   just check
   just cc-check
   ```

4. Run the preflight skill, inspect the final diff, stage only changed paths, and commit with a Conventional Commit subject.

5. Push the branch, create the pull request with `just pr`, and stop for review.

## Validation and Acceptance

Automated validation must demonstrate:

- `SessionEnd` parses from `hooks.json` and has no matcher support.
- The payload carries the standard common fields and one stable `reason` value.
- Successful, failed, pre-send-error, and graceful-interrupt paths each emit one `SessionEnd` hook record.
- A failing or non-JSON `SessionEnd` hook does not change the original task outcome.
- Existing `task_complete` ordering, Stop behavior, ErrorOccurred behavior, and interrupt exit code remain unchanged.

Documentation validation must demonstrate:

- `docs/configuration.md` recognizes `SessionEnd`.
- `docs/integrations.md` defines the payload and failure semantics.
- `docs/integrations/herdr.md` releases lifecycle authority and no longer claims Cake lacks an exit event.

## Idempotence and Recovery

Focused tests and formatting are safe to repeat. The managed branch can be recreated only after deleting or renaming the existing local branch; do not delete it while work is in progress.

If a provider test is slow, use a local wiremock response with a bounded delay and terminate the child through its captured `Child` handle. Never send a signal to a PID that is not the direct child created by that test.

If documentation formatting changes unrelated lines, revert only that unrelated churn and run the targeted formatter on the four changed Markdown files.

## Artifacts and Notes

Record focused test counts, `just check`, `just cc-check`, and manual process exit codes in the pull request. Record any platform limitation, especially if signal verification cannot run on a non-Unix host.

## Interfaces and Dependencies

- `crate::config::hooks::HookEvent::SessionEnd` is the recognized configuration event.
- `crate::hooks::SessionEndReason` is a copyable three-variant protocol classifier with `as_str() -> &'static str`.
- `HookRunner::session_end(reason)` returns `anyhow::Result<()>` for internal tests and consistent tracing, but the CLI caller always discards the result after logging it.
- `CodingAssistant::run_session_end_hook` is the process-orchestration boundary that enforces best-effort behavior.
- No new external dependency is required.
