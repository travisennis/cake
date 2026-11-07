---
status: accepted
date: 2026-11-07
decision-makers: Travis Ennis
consulted: 037, 046
informed: task 167
---

# Bounded Stop-hook follow-up turns

## Context and Problem Statement

A Cake invocation is one prompt per process. The agent loop ends when the model stops making tool calls, and Cake then runs the `Stop` hook with the final text. A `Stop` hook may return `additional_context`, but Cake only logs it (`src/main.rs`, `run_stop_hook`). Any event that arrives after the last tool call therefore waits for the user to type another prompt.

That boundary is the difference between a monitor that reacts and one that only reports. `BashSession` waits for new output and holds the whole front end while it does; a toolbox `monitor_read` needs the model to ask. A front end such as `cake-repl` or `cake-telegram` owns the next turn, so it can deliver an event by starting one, but plain `cake` has no such owner.

The `Stop` hook is the only place Cake already knows the turn ended and still holds the session, its lock, and the invocation's budget.

## Decision Drivers

- Deliver an event without a front end and without a blocking wait.
- No new daemon, socket, or background process that spends tokens unattended.
- Keep one invocation as one task record: one `task_start`, one `task_complete`, one `SessionEnd`.
- Make the extra model turn opt-in per hook response, so existing hooks that only log context do not start spending tokens after an upgrade.
- Preserve the append-only conversation and session record shape.
- Reuse the existing request loop and limits rather than adding a second one.

## Considered Options

1. **Let a `Stop` hook request one follow-up turn in the same invocation.** Chosen. The hook already runs at the boundary, the session lock is held, and the request loop is reusable.
2. **Add a new `BackgroundEvent` hook event that Cake polls.** This is the same mechanism with more protocol surface and no additional boundary: Cake would still only poll after a turn.
3. **A resident supervisor that starts `cake --resume` when an event arrives.** It cannot reach a live process, spends quota with nobody watching, and needs a place to send output. Rejected as a default; a front end or a wrapper can still do this explicitly.
4. **A bounded wait tool such as `monitor_read --wait 120`.** It reintroduces the block this project removed and dies with the process. Rejected.
5. **Only support event delivery in front ends that own the turn loop.** This keeps core unchanged but leaves plain `cake` unable to react. Rejected as the only answer; it remains a valid channel for a front end that wants it.

## Decision Outcome

Chosen option 1.

A `Stop` hook response may set `follow_up: true` together with `additional_context`. When it does, Cake appends the context as one synthetic user message with a stable `[Stop hook follow-up]` marker and runs one more request in the same invocation. The final assistant message is the invocation result; earlier responses are intermediate results.

- The cap is one follow-up per invocation, enforced in core. The `Stop` payload gains `follow_up_index` and `max_follow_ups` so a hook can see the budget before asking.
- The follow-up is skipped when `--output-schema` is set, when the hook returns no context, when the budget is exhausted, and after any error, cut-off, limit, or interrupt.
- `Stop` runs after every successful turn; `SessionEnd` and `task_complete` run once at the end.
- Text output prints each intermediate response when it completes and a stderr boundary notice. JSON adds `intermediate_results` only when non-empty. Stream-json already carries the synthetic user message and the follow-up response as ordinary records.

### Consequences

- Good: plain `cake` can react to an event at the turn boundary without a wrapper, and every front end inherits the behavior because it is in core.
- Good: monitors, subagent completion, and background Bash share one proactive path instead of each inventing a protocol.
- Bad: a hook can spend one extra model turn without a user prompt; the cap and the explicit opt-in bound the damage, and the session records the cause.
- Bad: a resumed transcript contains a marker user message rather than hook-context records, because hook developer context is not persisted today.
- Neutral: the invocation remains one task; consumers see two assistant messages, which text and JSON already distinguish.

## More Information

The wire additions are in [Integrations](../integrations.md#hook-protocol). Implementation and verification are tracked by task 167. A future settings key could raise `max_follow_ups`; this decision deliberately ships a constant.
