---
status: accepted
date: 2026-09-25
decision-makers: Travis Ennis
informed: issue 542
---

# SessionEnd Hooks Report One-Shot Invocation Outcomes

## Context and Problem Statement

Cake runs one agent turn per root invocation. Its command hooks cover session start, prompt submission, tool execution, stop, and post-send errors, but they do not cover every end-of-invocation path. In particular, a pre-send hook failure, a graceful interrupt, or another failure after a host has already reported `working` can leave an external lifecycle integration with stale authority.

`Stop` and `ErrorOccurred` cannot provide that boundary. They run only after `Agent::send` resolves, and they describe turn-boundary conditions rather than the final process outcome. An external reporter also needs a stable distinction between success, error, and interruption.

## Decision Drivers

- Release external lifecycle authority exactly once when a configured Cake process can still execute cleanup.
- Distinguish successful, failed, and gracefully interrupted one-shot invocations without exposing arbitrary error text as a protocol enum.
- Preserve the existing exit code and turn result when a final reporter fails.
- Preserve `task_complete` as the final stream and session record for the current task.
- Keep the hook payload aligned with the existing common lifecycle fields and trusted-command execution model.

## Considered Options

- Reuse `Stop` as an exit event. Rejected because it is post-send, best-effort turn completion, and has no interruption or pre-send coverage.
- Add `SessionEnd`, but let its decision or `fail_closed` setting alter the result. Rejected because reporters at process exit are cleanup observers and must not replace the outcome they are reporting.
- Add `SessionEnd` with a free-form reason string. Rejected because consumers would need error-prone string interpretation for a three-value state machine.
- Add `SessionEnd` with a typed reason enum and best-effort dispatch. Chosen because it is explicit, testable, and compatible with the existing hook runner.

## Decision Outcome

Chosen option: add one post-result, best-effort `SessionEnd` command event to root agent invocations, because it gives trusted external integrations a reliable cleanup boundary without changing task outcomes.

The event has no matcher. Its JSON payload contains the existing common fields and a `reason` string with exactly one of these values:

- `success` when `Agent::send` resolves successfully;
- `error` when setup fails before send, or when the provider turn fails, is cut off, or reaches another error outcome;
- `interrupted` when the first SIGINT or SIGTERM starts graceful shutdown.

Cake dispatches `SessionEnd` once after the send result is known and before the terminal `task_complete` record. A setup failure dispatches it before returning the setup error. The first graceful interrupt dispatches it before the interrupted `task_complete` record. This ordering keeps `task_complete` as the final task record for stream and session consumers while still running the external cleanup command before process exit.

Hook subprocess output is parsed through the existing command-hook machinery for transcript and tracing consistency, but the result is ignored. A nonzero exit, invalid JSON, block decision, timeout, or `fail_closed: true` on `SessionEnd` is logged as a best-effort failure and cannot change the turn result, exit code, or output. A second interrupt remains the existing hard-exit escape hatch and may bypass a slow or hung final hook, as may termination that Cake cannot intercept.

The event is emitted only after hook loading succeeds and a `SessionEnd` command is configured. Input, configuration, or hook-file failures that occur before a runnable hook set exists cannot invoke a hook from that same invalid configuration.

### Consequences

- Good, because lifecycle hosts can release custom agent authority on success, failure, setup failure, and graceful interruption.
- Good, because the three-value reason is small and stable while detailed errors remain in existing diagnostics and session records.
- Good, because final hook failure cannot mask the outcome being reported.
- Good, because existing stream and session consumers continue to see `task_complete` last.
- Bad, because process crashes, `SIGKILL`, and a forced second interrupt cannot run an in-process cleanup hook.
- Bad, because the event adds another trusted, potentially slow subprocess before terminal task persistence and output rendering.

## More Information

- Issue [#542](https://github.com/travisennis/cake/issues/542) defines the user-visible problem and acceptance criteria.
- ADR 005 established command hooks; this ADR extends its lifecycle coverage.
- ADR 011 established graceful SIGINT and SIGTERM shutdown and the second-interrupt escape hatch.
- `docs/integrations.md` and `docs/configuration.md` define the current hook protocol and configuration surface.
- `docs/integrations/herdr.md` demonstrates releasing pane lifecycle authority from `SessionEnd`.
