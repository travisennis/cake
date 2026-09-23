---
status: accepted
date: 2026-09-23
decision-makers: Travis Ennis
consulted: Codex
informed: issue 639
---

# Keep yielded Bash commands in bounded in-run sessions

## Context and Problem Statement

The Bash tool currently treats `timeout` as a kill deadline, capped at 600 seconds. A build or test that exceeds its window is terminated even when the agent could usefully poll it later. The command-safety judge approves the command before spawn, and the OS sandbox and process-group guard currently live only as long as the tool call. Returning early therefore requires a new owner for both the running process and its security resources.

## Decision Drivers

- A model should be able to start a command without predicting its duration.
- A command must not escape the sandbox or survive Cake shutdown because its tool call returned.
- Model-visible output and in-memory buffering must stay bounded.
- The first slice must preserve CLI, transcript, and stream JSON shapes; durable resume belongs to issue #391.

## Considered Options

- Keep killing at `timeout` and require longer values or retries. This still blocks a turn and cannot run beyond the 600-second cap.
- Add a separate tool only for background starts. This requires the model to predict whether a command will run long.
- Make every Bash command an in-run session internally, returning inline on quick completion and yielding a session ID otherwise.

## Decision Outcome

Choose an in-run session for every Bash command. `timeout` retains its name, unit, default, and range but becomes a yield window. An optional `background: true` yields immediately. A new `BashSession` tool reads new output, kills a process group, and lists live or recently exited sessions. Sessions are shared across `ToolContext` clones, kept only for the Cake run, and inaccessible under the read-only tool policy.

The session owner retains the sandbox guard and process-group guard until the child and its descendants are finished or terminated. It kills the group on explicit kill, a hard wall clock, Cake exit, Ctrl-C, or SIGTERM, escalating from SIGTERM to SIGKILL. Cancelling a Bash call before it returns the ID kills the group. A cancelled `BashSession` read leaves the process running. The judge runs once, before spawn, on the exact command that executes; a simple trailing `&` is removed before that preflight. Polling does not run the judge again.

The session journal bounds unread bytes, reports dropped bytes, and truncates on UTF-8 boundaries. A bounded count of live sessions prevents unbounded process growth. Exited sessions remain available for a bounded retention window so repeated final reads return the same status. Four positive-integer `[limits]` keys configure those bounds: `bash_session_output_max_bytes`, `bash_session_max`, `bash_session_max_seconds`, and `bash_session_exited_ttl_seconds`.

### Consequences

- Commands longer than the yield window can finish without blocking the agent turn.
- `Bash.timeout` changes from a kill deadline to a yield window, a deliberate model-visible compatibility break.
- Judge authorization lasts for the process lifetime, potentially much longer than one tool call. The OS sandbox continues to enforce filesystem policy throughout that lifetime; the hard wall clock bounds the exposure.
- Session IDs in a resumed transcript cannot be used in a later Cake process. An unknown-ID result must say so clearly and list active IDs.
- Pipe-backed capture remains; PTY, stdin, readiness conditions, persistence, and journal rotation are deferred to issue #391.

## More Information

- Issue #639 records the model-visible contract and acceptance criteria.
- `docs/exec-plans/active/639-bash-command-sessions.md` records the implementation and platform verification sequence.
- This extends the command judge boundary in ADR 018; it does not replace the judge or the OS sandbox.
