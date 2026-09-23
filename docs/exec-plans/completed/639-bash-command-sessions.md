## Add in-run Bash command sessions

This ExecPlan is a living document, maintained per docs/workflow/exec-plans.md. Keep Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective current.

ADR 036 (`docs/adr/036-in-run-bash-command-sessions.md`) records the durable decision this plan implements.

## Purpose / Big Picture

Cake currently kills a Bash command when its tool-call timeout expires. A build or test that needs longer than the yield window should instead keep running while the model receives a session ID, then report new output and its eventual exit status through `BashSession`. The feature is limited to one Cake run; exiting Cake terminates its process groups.

## Progress

- [x] (2026-09-23) Inspected issue #639, Bash execution, tool registry, settings, and shutdown paths; claimed the issue, raised Effort to L, and recorded ADR 036.
- [x] (2026-09-23) Resolved planning review: inlined the tool contract, decided `bash_read_cap` and saturation behavior, and recorded the read-only Bash availability break.
- [x] (2026-09-23) Added the bounded, shared in-run process registry and focused lifecycle tests; production Bash now transfers spawned children, sandbox guards, and process-group cleanup to it.
- [x] (2026-09-23) Added a test-scoped shared registry and bounded output journal with incremental reads, final-read replay, UTF-8 boundary handling, live-cap refusal, TTL pruning, and reservation discard. Process ownership and production wiring remain in the next stage.
- [x] (2026-09-23) Corrected invalid-byte accounting and bounded retained exited sessions after review of the registry stage.
- [x] (2026-09-23) Added and validated the four positive Bash session settings through resolved limits and settings precedence; process wiring and public documentation remain in the later stages.
- [x] (2026-09-23) Added a test-scoped process owner to the registry: pipe capture, kill signal, hard wall clock, and abort-on-drop process-group cleanup. Production Bash integration remains.
- [x] (2026-09-23) Changed Bash timeout to a yield window, added background mode, and registered BashSession read, kill, and list actions.
- [x] (2026-09-23) Wired the four session limits, synchronous process-group cleanup on registry drop, model descriptions, and tool snapshots.
- [x] (2026-09-23) Updated configuration, integration, and security documentation; verified yield, poll, and kill under macOS Seatbelt and passed `just check` and `just docs-check`. The same platform test awaits Linux CI because local Linux execution and its cross compiler are unavailable.

## Surprises & Discoveries

- The current `run_bash_child` owns capture, timeout termination, and reaping as one future. A yielded process therefore needs a new owner for the child, pipes, sandbox guard, and group guard; wrapping the current future in a timeout would cancel and kill it.
- `ToolContext` is cloned when the judge is attached. The registry must be held through `Arc` so that clone cannot create an independent session map.
- `Bash` is currently registered as read-safe. The new session contract requires removing that capability as well as keeping `BashSession` unavailable under read-only policy.
- The existing cancellation test proves that dropping a Bash call eventually kills a descendant process group. Moving the lifecycle into an owned task keeps that tested result and creates a handoff point for the later registry.
- The journal and registry state machine can be tested without changing the current model-visible Bash contract. The module stays `#[cfg(test)]` until the child lifecycle and sandbox guard are transferred to it, so this stage does not add unused production code.
- A worker that reaps the direct shell before its descendants close inherited pipes loses `Child::id()`. The capture must finish before reaping so explicit kill and the hard wall clock can still signal the original process group.
- An abort now schedules the worker's drop before its process-group guard sends SIGKILL, rather than sending SIGKILL synchronously in the caller's drop. A hard process exit during that scheduling gap can skip the kill. The shutdown-cleanup stage must signal registered process groups explicitly on normal exit and first interrupt rather than depend on worker drop ordering; a second interrupt remains a hard exit.

## Decision Log

- Decision: implement in bounded stages because this changes complex execution and security logic and the repository caps such diffs at 500 changed lines. Rationale: a separate session core can be verified before changing the model-visible Bash contract. Date/Author: 2026-09-23, Codex.
- Decision: the four safety keys accept positive integers; `"unlimited"` is not meaningful for session count, hard wall clock, exit retention, or a bounded journal. Rationale: these are mandatory lifecycle bounds, unlike optional output budgets. Date/Author: 2026-09-23, Codex.
- Decision: session-managed commands ignore `bash_read_cap`; the bounded unread journal drops old output and reports an `output_truncation` event on the result that first reports the gap. Rationale: a read cap that kills the process conflicts with the session's purpose. Date/Author: 2026-09-23, Codex.
- Decision: when the live-session cap is full, refuse a new Bash call before judge preflight or spawn and name the cap and active IDs. Rationale: killing an existing session would discard work the model may still need. Date/Author: 2026-09-23, Codex.
- Decision: retain at most four exited sessions per live-session slot, evicting the oldest completed entry when full. Rationale: a TTL alone permits rapid short commands to accumulate unbounded unread journals; the derived count avoids a fifth settings key. Date/Author: 2026-09-23, Codex.

## Security and Compatibility Impact

The command-safety judge still authorizes exactly the normalized command before spawn. Its authorization now lasts until exit, explicit kill, Cake shutdown, or the hard wall clock, rather than one tool call. The OS sandbox must remain in force throughout that lifetime. The changed meaning of `Bash.timeout` is model-visible and breaks callers that expected a kill deadline. `bash_read_cap` stops killing Bash commands, and removing Bash's read-safe capability removes shell exploration from `--sandbox read-only`. CLI flags, exit codes, stream JSON, and persisted session records keep their shapes. Update `docs/security.md` and the relevant read-only tests during implementation.

The bypass classes to defend against are a backgrounded descendant surviving a shell exit, a child ignoring SIGTERM, a tool call cancelled before the session ID reaches the model, a Cake interrupt or normal exit leaving a group alive, a dropped Seatbelt profile while a command runs, journal flooding or invalid UTF-8 at a trim boundary, and a stale session ID in a resumed transcript. The trailing `&` normalization addresses only the simple background operator. Process-group ownership and shutdown cleanup must handle descendants regardless of shell syntax, including pipelines, quoting, and child scripts; no shell-text parser is a security boundary.

## Model-visible Contract and Limits

`Bash` keeps `command`, `cwd`, and `reason`. Its `timeout` remains seconds, defaults to 60, and clamps to 1--600, but is a yield window. Optional `background: true` yields immediately. A command with output below the caps that finishes inside the window keeps the current output and `[exit:0 | 1.2s]` footer format. If it outlives the window or starts in background, Bash returns output so far followed by `[still running | session: bash_a1b2c3 | 12.3s]` and `The command was NOT killed; it is still running. Use BashSession with session "bash_a1b2c3" to poll for new output or kill it.` A stripped trailing `&` is noted in this result.

`BashSession` accepts `{"action":"read","session":"bash_a1b2c3"}`, the same call with `"wait":30`, `{"action":"kill","session":"bash_a1b2c3"}`, or `{"action":"list"}`. `read` returns only output since the previous call and returns on new output, exit, or expiry of its wait window. `wait` defaults to 10 seconds, is capped at 120, and `0` polls without blocking. A live read ends with `[running | session: bash_a1b2c3 | 45.1s elapsed]`; a completed read ends with `[exit:0 | total 61.2s]`. `kill` returns remaining output and final status. `list` gives each live or recently exited session's ID, status, elapsed time, and truncated command. A journal gap adds `[Oldest output was dropped: N bytes since the last read.]` to the footer. A repeated read of a retained exited session returns the same final output and exit code. An unknown ID returns an error naming active IDs and explaining that sessions do not survive Cake restarts.

The four positive-integer `[limits]` keys default to `bash_session_output_max_bytes = 1048576` (unread bytes per session), `bash_session_max = 16` (concurrent live sessions), `bash_session_max_seconds = 3600` (hard wall clock), and `bash_session_exited_ttl_seconds = 600` (post-exit retention). Retained exited sessions are also capped at four times `bash_session_max` (64 by default); reaching that cap evicts the oldest completed entry, even before its TTL. At the live cap, a new Bash call is refused before judge preflight or spawn; no older process is evicted. `bash_read_cap` remains parseable but does not kill session-managed commands. Journal overflow drops the oldest unread bytes, reports the precise gap, and emits `output_truncation` on the first Bash or BashSession result that reports that gap. `bash_output_max_bytes` still bounds inline Bash output.

## Outcomes & Retrospective

Bash now yields a live session when its window expires or background mode is requested. BashSession can read incremental output, kill a process group, and list retained sessions. The registry bounds live processes, unread bytes, hard run time, and completed-session retention. Short commands keep their ordinary output and exit footer. The full local gate, docs gate, and macOS Seatbelt yield/poll/kill test pass. Linux Landlock runtime verification remains a CI requirement: this macOS host has the Linux Rust target but lacks `x86_64-linux-gnu-gcc` and a Linux container runtime. The final integration diff exceeds the 500-line complex-logic budget because replacing the old Bash lifecycle, registering the tool, updating its tests, and regenerating snapshots form one model-visible contract; the preceding core stages were delivered separately.

## Context and Orientation

`src/clients/tools/bash.rs` parses and judges a command, prepares an OS-sandboxed child, captures both pipes, and currently kills the process group at timeout. Its `PreparedBashCommand` holds a `SandboxGuard`; dropping that guard before completion can remove the Seatbelt profile. `src/clients/tools/mod.rs` owns the shared `ToolContext` and built-in tool registry. `src/config/settings.rs` merges global, project, and profile limits. `src/main.rs` races agent work against Ctrl-C and SIGTERM. `ToolboxProcessGuard` kills a process group on drop. A session journal is bounded unread output, ordered as pipe reads complete, with a monotonically increasing byte cursor and a count of bytes dropped before reading.

## Plan of Work

### Milestone 1: Own a running process after the call returns

First, move the current Bash child lifecycle into an owned task while preserving its output and timeout behavior. The task retains the sandbox guard and aborts on caller cancellation; `cargo test clients::tools::bash::tests` must pass, including the descendant-cancellation test. Then add a session core in a new module under `src/clients/tools/`. It owns child execution after yield, both pipe readers, process-group cleanup, the sandbox guard, a bounded UTF-8-safe unread journal, timestamps, final status, and a shared registry. Its tests should prove incremental reads, overflow reporting, repeat final reads, cap enforcement, hard deadline, and descendant cleanup. A stage must have production call sites or remain entirely test-scoped; do not add dead production code merely to stage the diff. Keep the model-visible Bash behavior while the core is developed. From the repository root, run `cargo test bash_session` and `cargo fmt --check`; the tests should pass while existing Bash results
remain unchanged.

### Milestone 2: Yield and manage Bash sessions

Make `src/clients/tools/bash.rs` judge and normalize the command before spawning. Remove a trailing shell background operator before the judge sees it, retaining a result annotation. A command finishing inside its yield window must still use the existing formatting path for output within the old caps. On yield or `background: true`, transfer the child, its sandbox guard, and its process-group guard to the shared core and return the running footer. Cancellation before the ID is delivered must kill the group. `BashSession` receives only action, ID, and optional wait, and never invokes the command judge. From the repository root, run `cargo test bash` and `cargo test bash_session`; a short command must return inline, a long one must yield, and read, kill, list, repeat final read, and unknown-ID cases must pass.

### Milestone 3: Wire limits, shutdown, and public contracts

Register `BashSession` in `src/clients/tools/mod.rs`, remove Bash's read-safe capability, connect the registry through an `Arc` in `ToolContext`, and ensure Cake shutdown drops or explicitly kills every live session. Add the four positive integer settings keys in `src/config/settings.rs` and document them in `docs/configuration.md`; explain that `bash_read_cap` no longer kills Bash processes and add `BashSession` to the registered names for `tools.enabled`. Update `docs/security.md` with the authorization lifetime and the loss of Bash under read-only policy. Regenerate tool-definition snapshots through `just snapshots` and review only changes caused by these contracts. From the repository root, run `just snapshots`, `just check`, and `just docs-check`; all must pass. Exercise yield, poll, kill, and Cake exit under macOS Seatbelt and Linux Landlock, recording the result or exact unavailable platform prerequisite before the final PR.

## Concrete Steps

From the repository root, implement and verify each bounded stage with a focused `cargo test` filter, `cargo fmt --check`, and `git diff --check`. For the final stage run `just snapshots`, review with `cargo insta review`, run `just check`, and run `just docs-check`. Exercise yield, poll, kill, and Cake exit under Seatbelt on macOS and Landlock on Linux. If Linux tooling is unavailable locally, record the exact CI/platform check required in the pull request.

## Validation and Acceptance

A short Bash command whose output stays within the former read cap returns the same output and exit footer as before. A long command yields with a session ID and remains alive; `BashSession read` reports only new output, waits at most its configured window, and returns the final exit status after completion. A second read while the exited session is retained repeats the final result. `kill`, the hard deadline, Ctrl-C, SIGTERM, and Cake exit leave no descendant in the command's process group. The journal never exceeds its byte limit, preserves valid UTF-8, and reports the precise dropped-byte count. Unknown IDs report active IDs. The model cannot select Bash or BashSession under read-only policy and can select `BashSession` via `--tools BashSession` otherwise.

## Idempotence and Recovery

Focused tests and formatting can be repeated. Session IDs and process records exist only in memory, so rerunning Cake starts empty. If a stage fails verification, keep its branch and repair the focused stage before proceeding. Do not change the persisted session schema. Regenerate snapshots from code rather than editing them manually.

## Artifacts and Notes

Issue: https://github.com/travisennis/cake/issues/639. The owned-task stage passed `cargo test dropping_bash_future_kills_descendants`, `cargo test bash_child_task_preserves_worker_panic`, and, with local mock-server socket access, `cargo test clients::tools::bash::tests` (133 passed), `just check`, and `just docs-check`. The registry state-machine stage and its review fixes passed `cargo test bash_session_core` (10 passed), `just check` (with local mock-server socket access), `just docs-check`, and `git diff --check`; no process-lifecycle or platform verification is claimed for that test-scoped stage. Record later platform results here as they run. A final pull request should close #639 only after the model-visible behavior, settings, documentation, and lifecycle checks all pass.

The settings stage passed `cargo test bash_session_limits_resolve_and_reject_unbounded_values`, `cargo test test_limits_output_budget_project_overrides_global_per_key`, and `just check` with local mock-server socket access. An initial sandboxed `just check` could not bind mock-server ports; the rerun passed. The settings are not yet consumed by running sessions, so `docs/configuration.md` will be updated when that behavior lands.

The process-owner prototype passed `cargo test process_owner`, `cargo test dropping_registry_kills_descendant_process_group`, `just check` with local mock-server socket access, and `just docs-check`. A worker join was found to be polled twice on normal completion; the second poll was removed. This stage remains test-scoped, so it does not yet establish the model-visible session contract or platform sandbox behavior.

PR #645 review found that the worker reaped the shell before kill and hard-wall signalling. The worker now waits for capture before reaping; focused tests cover both termination paths when a descendant outlives its shell, and capture with one pipe absent. The regression tests require the group to terminate promptly, before the former five-second capture fallback.

## Interfaces and Dependencies

The new registry belongs to `crate::clients::tools` and is shared by `ToolContext` clones through `Arc`. `Bash` starts a command and uses the registry on yield. `BashSession` uses the registry for read, kill, and list. The registry owns `SandboxGuard` until the process is reaped and owns `ToolboxProcessGuard` until it has killed the group or confirmed all process work is finished. `bash_read_cap` stays parseable for settings compatibility but is superseded for session-managed Bash execution by `bash_session_output_max_bytes`; journal gaps produce `output_truncation` telemetry instead of killing the group. No new crate or persisted record type is required.

Revision 2026-09-23: inlined the model-visible contract and limit defaults, resolved `bash_read_cap` and live-cap behavior, made the read-only Bash removal explicit, and added verifiable milestones after review of planning PR #642.

Revision 2026-09-23: split the first milestone into an owned-task bridge and the registry/journal work so the first code PR stays inside the complex-logic diff budget while preserving the existing cancellation guarantee.

Revision 2026-09-23: clarified that cancellation schedules the group kill, made `finish` own its task, and preserved panic propagation after review of the first code PR.

Revision 2026-09-23: staged and verified the registry/journal state machine separately from process ownership. The next code stage must make this module production and connect child ownership, hard deadlines, and shutdown cleanup before enabling yielded Bash results.

Revision 2026-09-23: review found that a leading invalid continuation byte was counted as dropped without overflow, and that TTL alone did not bound completed-session memory. The journal now trims only a split valid character, and completed sessions have a count cap derived from the live-session limit.

Revision 2026-09-23: completed the model-visible session integration, documented the compatibility and authorization changes, and recorded the macOS platform result and Linux CI prerequisite.
