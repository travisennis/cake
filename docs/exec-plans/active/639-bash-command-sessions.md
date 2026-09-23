## Add in-run Bash command sessions

This ExecPlan is a living document, maintained per docs/workflow/exec-plans.md. Keep Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective current.

ADR 036 (`docs/adr/036-in-run-bash-command-sessions.md`) records the durable decision this plan implements.

## Purpose / Big Picture

Cake currently kills a Bash command when its tool-call timeout expires. A build or test that needs longer than the yield window should instead keep running while the model receives a session ID, then report new output and its eventual exit status through `BashSession`. The feature is limited to one Cake run; exiting Cake terminates its process groups.

## Progress

- [x] (2026-09-23) Inspected issue #639, Bash execution, tool registry, settings, and shutdown paths; claimed the issue, raised Effort to L, and recorded ADR 036.
- [ ] Add a bounded, shared in-run process registry and focused lifecycle tests.
- [ ] Change Bash's timeout to a yield window and add background mode and a BashSession tool.
- [ ] Wire four session limits, shutdown cleanup, model descriptions, and tool snapshots.
- [ ] Update configuration and security documentation, verify on macOS and Linux, run the repository gate, and archive this plan.

## Surprises & Discoveries

- The current `run_bash_child` owns capture, timeout termination, and reaping as one future. A yielded process therefore needs a new owner for the child, pipes, sandbox guard, and group guard; wrapping the current future in a timeout would cancel and kill it.
- `ToolContext` is cloned when the judge is attached. The registry must be held through `Arc` so that clone cannot create an independent session map.
- `Bash` is currently registered as read-safe. The new session contract requires removing that capability as well as keeping `BashSession` unavailable under read-only policy.

## Decision Log

- Decision: implement in bounded stages because this changes complex execution and security logic and the repository caps such diffs at 500 changed lines. Rationale: a separate session core can be verified before changing the model-visible Bash contract. Date/Author: 2026-09-23, Codex.
- Decision: the four safety keys accept positive integers; `"unlimited"` is not meaningful for session count, hard wall clock, exit retention, or a bounded journal. Rationale: these are mandatory lifecycle bounds, unlike optional output budgets. Date/Author: 2026-09-23, Codex.

## Security and Compatibility Impact

The command-safety judge still authorizes exactly the normalized command before spawn. Its authorization now lasts until exit, explicit kill, Cake shutdown, or the hard wall clock, rather than one tool call. The OS sandbox must remain in force throughout that lifetime. The changed meaning of `Bash.timeout` is model-visible and breaks callers that expected a kill deadline; CLI flags, exit codes, stream JSON, and persisted session records keep their shapes.

The bypass classes to defend against are a backgrounded descendant surviving a shell exit, a child ignoring SIGTERM, a tool call cancelled before the session ID reaches the model, a Cake interrupt or normal exit leaving a group alive, a dropped Seatbelt profile while a command runs, journal flooding or invalid UTF-8 at a trim boundary, and a stale session ID in a resumed transcript. The trailing `&` normalization addresses only the simple background operator. Process-group ownership and shutdown cleanup must handle descendants regardless of shell syntax, including pipelines, quoting, and child scripts; no shell-text parser is a security boundary.

## Outcomes & Retrospective

Pending implementation and verification.

## Context and Orientation

`src/clients/tools/bash.rs` parses and judges a command, prepares an OS-sandboxed child, captures both pipes, and currently kills the process group at timeout. Its `PreparedBashCommand` holds a `SandboxGuard`; dropping that guard before completion can remove the Seatbelt profile. `src/clients/tools/mod.rs` owns the shared `ToolContext` and built-in tool registry. `src/config/settings.rs` merges global, project, and profile limits. `src/main.rs` races agent work against Ctrl-C and SIGTERM. `ToolboxProcessGuard` kills a process group on drop. A session journal is bounded unread output, ordered as pipe reads complete, with a monotonically increasing byte cursor and a count of bytes dropped before reading.

## Plan of Work

First, add a session core in a new module under `src/clients/tools/`. It owns child execution after spawn, both pipe readers, process-group cleanup, the sandbox guard, a bounded UTF-8-safe unread journal, timestamps, final status, and a shared registry. Its tests should prove incremental reads, overflow reporting, repeat final reads, cap enforcement, hard deadline, and descendant cleanup. The first stage must have production call sites or remain entirely test-scoped; do not add dead production code merely to stage the diff. Keep the model-visible Bash behavior while this core is developed, so the stage is independently testable and does not expose an incomplete tool.

Next, make `src/clients/tools/bash.rs` judge and normalize the command before spawning. Remove a trailing shell background operator before the judge sees it, retaining a result annotation. A command finishing inside its yield window must still use the existing formatting path. On yield or `background: true`, transfer the child, its sandbox guard, and its process-group guard to the shared core and return the running footer. Cancellation before the ID is delivered must kill the group. `BashSession` receives only action, ID, and optional wait, and never invokes the command judge.

Then register `BashSession` in `src/clients/tools/mod.rs`, remove Bash's read-safe capability, connect the registry through an `Arc` in `ToolContext`, and ensure Cake shutdown drops or explicitly kills every live session. Add the four positive integer settings keys in `src/config/settings.rs` and document them in `docs/configuration.md`. Update `docs/security.md` with the changed authorization lifetime and verification on each supported platform. Regenerate tool-definition snapshots through `just snapshots` and review only changes caused by these contracts.

## Concrete Steps

From the repository root, implement and verify each bounded stage with a focused `cargo test` filter, `cargo fmt --check`, and `git diff --check`. For the final stage run `just snapshots`, review with `cargo insta review`, run `just check`, and run `just docs-check`. Exercise yield, poll, kill, and Cake exit under Seatbelt on macOS and Landlock on Linux. If Linux tooling is unavailable locally, record the exact CI/platform check required in the pull request.

## Validation and Acceptance

A short Bash command returns the same output and exit footer as before. A long command yields with a session ID and remains alive; `BashSession read` reports only new output, waits at most its configured window, and returns the final exit status after completion. A second read within retention repeats the final result. `kill`, the hard deadline, Ctrl-C, SIGTERM, and Cake exit leave no descendant in the command's process group. The journal never exceeds its byte limit, preserves valid UTF-8, and reports the precise dropped-byte count. Unknown IDs report active IDs. The model cannot select `BashSession` under read-only policy and can select it via `--tools BashSession` otherwise.

## Idempotence and Recovery

Focused tests and formatting can be repeated. Session IDs and process records exist only in memory, so rerunning Cake starts empty. If a stage fails verification, keep its branch and repair the focused stage before proceeding. Do not change the persisted session schema. Regenerate snapshots from code rather than editing them manually.

## Artifacts and Notes

Issue: https://github.com/travisennis/cake/issues/639. Record test commands and platform results here as they run. A final pull request should close #639 only after the model-visible behavior, settings, documentation, and lifecycle checks all pass.

## Interfaces and Dependencies

The new registry belongs to `crate::clients::tools` and is shared by `ToolContext` clones through `Arc`. `Bash` starts a command and uses the registry on yield. `BashSession` uses the registry for read, kill, and list. The registry owns `SandboxGuard` until the process is reaped and owns `ToolboxProcessGuard` until it has killed the group or confirmed all process work is finished. No new crate or persisted record type is required.
