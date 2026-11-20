---
status: accepted
date: 2026-11-20
decision-makers: Travis Ennis
---

# Bound and retry compaction summary attempts

## Context and Problem Statement

ADR 047 bounded summary work by four logical requests and a fixed 180-second deadline for the whole operation. It also disabled further proactive compaction attempts for the rest of an invocation after one failed or no-fit attempt. In a long invocation, one transient provider stall therefore removed automatic compaction for hours of later turns, and the deadline could not be adjusted for slower summarizer models. The fixed deadline also left the first request able to consume the entire budget, so a stalled request never left room for a retry.

Provider transport already owns retrying provider errors. What compaction owns is the wall-clock bound and the decision of when, if ever, to try again after a failure.

## Decision Drivers

- One transient failure must not disable automatic compaction for the rest of a long invocation.
- A doomed attempt must not repeat on every later turn, which wastes the user's quota.
- Summary work must stay bounded in both attempts and wall-clock time.
- Users must be able to tune the summary deadline for slow providers without changing the main model.
- Requests that exceed the known context window must still fail before they are sent.

## Considered Options

- **Keep the single fixed deadline and per-invocation suppression (status quo).** Simple, but one stall disables compaction and the budget is not tunable.
- **Retry failed summaries on every later turn.** Recovers quickly from a transient failure, but repeats an unchanged doomed attempt and its cost on every turn.
- **Retry after growth, with a shared configurable deadline (chosen).** Recovers when new content makes a retry plausible and keeps total work bounded.
- **Exponential backoff on failures.** Bounds attempt frequency without new content, but spends summary requests even when nothing has changed enough to change the outcome.

## Decision Outcome

Chosen: add `[compaction] summary_timeout_secs`, default 180, and make the deadline govern retries rather than a single request.

`summary_timeout_secs` is the total wall-clock budget for one summary operation, including retries, and must be positive. Each logical summary request gets a share of the budget still remaining: the first attempt may use at most half, so a stalled request leaves room for exactly one retry within the same operation. A first timeout is retried; a second timeout fails the operation with the deadline error. Provider errors returned by the transport are not retried at this layer.

After a failed summary, a no-fit result, or another skipped attempt, another automatic attempt runs only once the projected request has grown by at least a tenth of the trigger (never less than one token) and still reaches the trigger. Every request in between is still measured, and an over-window request fails before it is sent. A policy without a resolvable trigger never retries, which matches its inability to prepare. A new invocation starts with a fresh attempt budget.

### Consequences

- Good, because a transient stall delays compaction instead of disabling it for the invocation.
- Good, because retries are limited to situations where the projection changed enough to change the outcome.
- Good, because slow summarizer providers can raise the deadline without touching the main thread.
- Bad, because a raised deadline allows a stalled summary to hold the turn longer before it fails.
- Neutral, because the default deadline remains 180 seconds and existing configurations behave the same.

## More Information

The implementation lands in `src/clients/compaction.rs` and `src/clients/agent/compaction.rs`; `docs/configuration.md` owns the user surface. This ADR partially supersedes [ADR 047](047-compact-model-context-with-self-contained-append-only-checkpoints.md): its fixed 180-second deadline without an attempt split, and its per-invocation suppression of every later proactive attempt, no longer apply. The rest of ADR 047, including checkpoint persistence, the opt-in policy, and the four-request bound, remains accepted.
