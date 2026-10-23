---
status: accepted
date: 2026-10-23
decision-makers: Travis Ennis
informed: ahm task 090, ahm task 083
---

# Expose main-request context usage on stream-json

## Context and Problem Statement

`cake-repl` treats Cake as an external engine: it drives `cake --output-format stream-json` for each prompt and `cake --output-format stream-json replay <uuid>` when it reopens a session. Its status line has room for a context meter, but Cake exposes no supported value to put in it.

The only stream usage is the final `task_complete.usage`, which is the task aggregate: it sums every attempt, including retries and discarded turns, so it grows for the life of the task and cannot be read as "how full is the context now". Cake already keeps the correct measurement internally: `turn_usage` records the normalized usage of every provider attempt that reported it, and `log_context_budget()` computes the remaining budget from the configured `context_window` minus the latest attempt's input tokens. Those records are session-only today.

A client-side estimate from the rendered transcript (characters divided by four over the visible timeline) omits the system prompt, the skill catalog, tool schemas, timeline entries the client trimmed, and cache reads. It is wrong by multiples, which is worse than showing nothing for a number a user acts on. Reading Cake's session files or telemetry sidecar is prohibited by the client's engine-isolation contract.

ADR 025 decided that session-only audit records stay out of stream-json so the live feed does not become a copy of the session audit ledger. ADR 047 repeated that boundary for usage records. This decision changes that boundary narrowly: the main-request context measurement is a client input, not audit bookkeeping, and withholding it leaves machine-mode clients with no truthful source.

## Decision Drivers

- A machine-mode client needs one truthful, supported way to measure current context use without reading session files.
- The most recent main-agent request's input tokens are the measurement: they count exactly what was sent to the provider, including the system prompt, tools, and tool outputs.
- The configured input window is known per model, but some models configure none, so the budget must be optional rather than fabricated.
- A partial provider usage report must never read as a measured context size or a fraction.
- Retries and failed attempts must remain visible and distinguishable so a client can tell which attempt produced a measurement. A first successful attempt that is later discarded keeps the legacy shape because settlement happens before classification (ADR 025).
- Summary and compaction attempts measure an internal prompt, not the conversation's context, so they must not move a client's meter.
- `task_complete.usage` and the completion JSON keep their existing aggregate shape and meaning.
- The change must be additive: old clients that ignore unknown record types and fields keep working.

## Considered Options

- **Expose only the aggregate.** Rejected: it is the sum of every attempt and cannot answer the question the client asks.
- **Add a replay-only or stream-only `context_usage` record.** Rejected: replay would have to reconstruct the budget from current configuration rather than the historical attempt, and a second record family would duplicate the same accounting without eliminating the need to distinguish summary attempts.
- **Emit the existing `turn_usage` payload on stream-json with optional budget fields (chosen).** The session record, its ordering, its presence semantics, and its provider identity are already settled. Replay can emit the stored record verbatim, and one record family serves both persistence and the live contract.
- **Let the client estimate from the visible timeline.** Rejected: it measures the wrong thing and can be wrong by multiples.
- **Poll a new introspection command.** Rejected: it widens the client invocation contract, needs a separate decision, and is always at least one turn stale.

## Decision Outcome

Chosen option: emit `turn_usage` as a live and replayed stream-json record for every main-agent provider attempt that reported usage.

The stream record carries the same payload as the session record: `session_id`, `task_id`, `turn`, normalized `usage`, `timestamp`, optional 1-based `attempt`, optional `terminal_class`, optional `usage_presence`, `model`, `response_model`, and `provider_request_id`. `attempt` and `terminal_class` appear on retried or failed attempts; a first successful attempt keeps the legacy shape with both absent, even if the run later discards that response as context pressure. It adds two optional fields:

- `context_window`: the configured input token window for the attempt.
- `remaining_context_tokens`: the window minus the attempt's reported input tokens, saturating at zero.

Both budget fields are present only when the model configures a window and the provider reported complete usage. A partial report, an unknown window, or a historical record written before the fields existed omits them rather than sending zero or a fabricated value. `usage_presence` remains the marker that says whether the provider reported every counter.

Live streams emit one record per main-agent attempt in provider-attempt order, before that response's conversation records. Replay emits stored `turn_usage` records in physical order and never rewrites them. Older sessions without the budget fields replay with the measured usage and no window. `task_complete.usage` remains the sum of every reported attempt and its shape is unchanged.

Summary and compaction attempts are not main-agent attempts for this contract. Their usage stays in the session file and telemetry sidecar for cost accounting, and it is never emitted as a context measurement nor used to change a resumed session's context basis.

### Consequences

- Good, because a client can render a fraction from the latest complete main-agent attempt and update it as a task progresses.
- Good, because replay hydrates the same meter before a resumed session makes its first new request.
- Good, because retries and failed attempts stay visible with `attempt` and `terminal_class`, while clients choose the latest complete record.
- Bad, because a successful first attempt that is later discarded as context pressure keeps the legacy shape and cannot be identified as discarded from the usage record alone; the outcome remains the authoritative signal. Marking it at settlement would require knowing the discard verdict before classification, which ADR 025 deliberately forbids.
- Good, because `task_complete.usage`, completion JSON, and session format version 4 remain unchanged.
- Bad, because stream-json now carries one record kind that was previously session-only, so the absolute boundary stated by ADR 025 and ADR 047 no longer holds; this ADR supersedes only that narrow part.
- Bad, because summary and compaction cost is not visible to stream consumers; that remains a telemetry and session audit concern by design.

## More Information

- ahm task 090 owns the implementation; ahm task 083 is the consuming `cake-repl` status-line work.
- [ADR 021](021-session-transcript-replay.md), [ADR 025](025-provider-usage-settlement.md), [ADR 030](030-provider-attempt-usage-semantics.md), and [ADR 047](047-compact-model-context-with-self-contained-append-only-checkpoints.md).
- [Integration contracts](../integrations.md), Stream JSON, Session replay, and Persisted sessions.
- Partial supersession: this decision supersedes only the driver "Keep session-only audit records out of stream-json" and its consequence in ADR 025, and the sentence "Session-only usage records remain absent from stream-json" in ADR 047. Every other part of those decisions remains accepted.
