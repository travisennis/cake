# ADR 010: Interrupt Handling with Graceful Shutdown

**Status:** Accepted
**Date:** 2026-06-11

## Context

cake installs no signal handler. When a user interrupts a run with Ctrl-C:

- The session file ends without a `TaskComplete` record, so consumers see a task
  that started and never finished.
- `emit_session_summary_telemetry` never runs, leaving the telemetry sidecar
  without a summary record.
- Worktree cleanup is skipped, leaving the temporary worktree registered and the
  original directory not restored.
- Hook lifecycle events (`stop`/`error_occurred`) never fire.

Bash tool children are already covered via `kill_on_drop(true)`, so the gap is
the session/task lifecycle, not orphaned subprocesses.

## Decision

We add a `tokio::signal::ctrl_c()` listener that races against the agent turn.
When Ctrl-C arrives:

1. The in-flight agent turn is abandoned (its future is dropped at the next
   `.await` point).
2. A `TaskComplete` record with a new `Interrupted` outcome is written to the
   session and stream.
3. Session summary telemetry is emitted with `success: false`.
4. Worktree cleanup runs (matching normal-exit policy).
5. The process exits with code 130 (128 + SIGINT = 128 + 2).
6. A second Ctrl-C during the graceful-shutdown path calls
   `std::process::exit(130)` immediately.

### Session/Stream Record Contract Change

A new `Interrupted` variant is added to both `TaskCompleteSubtype` and
`TaskOutcome`. The serialized JSON shape for an interrupted outcome is:

```json
{
  "subtype": "interrupted",
  "is_error": true,
  "result": null,
  "error": null
}
```

This is a backward-compatible addition: consumers that do not recognize the
`interrupted` subtype see `is_error: true` and can treat it as a generic
failure. No existing producer emits this subtype, so old session files are
unaffected.

### Exit Code

Exit code 130 follows the POSIX convention (128 + SIGINT signal number).
Calling scripts and CI pipelines can distinguish an interrupt from agent errors
(exit 1), API errors (exit 2), or input errors (exit 3).

### Second Ctrl-C

After the first Ctrl-C begins the graceful shutdown sequence, a second Ctrl-C
calls `std::process::exit(130)` immediately. This handles the case where the
cleanup itself hangs (e.g., a slow hook, a stuck telemetry write).

## Rationale

- **Race, not cancellation token**: Racing `ctrl_c()` directly against the
  turn future is simpler than plumbing a cancellation token through the agent
  loop. The agent loop already drops tool futures at the next turn boundary, so
  dropping the turn future is safe.
- **Distinct exit code**: 130 matches POSIX convention and lets automation
  distinguish interrupts from other failures.
- **Second Ctrl-C safety**: After the first interrupt, parts of the cleanup
  (hooks, telemetry, worktree removal) could hang. A second Ctrl-C gives the
  user a hard escape hatch without requiring a `SIGKILL` from another terminal.
- **Minimal agent changes**: The agent does not need to know about signals. All
  interrupt handling lives in the CLI layer (`main.rs`), preserving the agent's
  single-responsibility boundary.

## Consequences

### Positive

- Session files are always well-formed: every `TaskStart` is followed by a
  `TaskComplete` (even on interrupt).
- Telemetry sidecars always have a summary record.
- Worktrees are cleaned up consistently.
- Hooks get a chance to react to the abrupt stop via the error path.
- The change is isolated to the CLI layer; the agent module is untouched.

### Negative

- The agent's in-flight API call is abandoned mid-stream. The model provider
  sees a partial request. This is acceptable because the user explicitly
  requested interruption.
- Adding a new `TaskCompleteSubtype` variant is a minor serialization contract
  change. Old consumers that check for exact known subtypes will need updating.
  Consumers using `is_error` (the recommended approach) are unaffected.

## Alternatives Considered

- **Cancellation token through the agent loop**: Would require plumbing a
  `CancellationToken` through `Agent::send()` and every tool execution future.
  More invasive, no benefit over dropping the turn future at the select level.
- **Default signal behavior (process termination)**: Would lose all session
  cleanup. Not acceptable.
- **Ignore Ctrl-C entirely**: The user would have no way to interrupt a runaway
  agent loop. Not acceptable.
- **Single Ctrl-C exit without cleanup**: User-visible corruption of session
  state. Not acceptable.

## References

- Task 200: Add Ctrl-C Handling With Graceful Shutdown
- `src/main.rs` — `main()`, `CodingAssistant::run`
- `src/types/session.rs` — `TaskOutcome`, `TaskCompleteSubtype`
- `src/exit_code.rs` — exit code classification
