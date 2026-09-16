---
status: accepted
date: 2026-09-15
decision-makers: Travis Ennis
informed: issue 354
---

# Explicit Usage Presence In Provider-Attempt And Turn Records

## Context and Problem Statement

Cake records provider-reported token usage at three layers, and only the first two are currently distinguishable from each other:

- a **provider attempt**: one HTTP request to the provider, recorded as an `api_attempt` telemetry record and, when the provider reports usage, as one `turn_usage` session record (ADR-025);
- an **agent turn**: one logical loop iteration that may consume several attempts, counted as `turn_count` and reported as one line of conversation;
- a **task aggregate**: `task_complete.usage` in the session file and `usage` in the completion JSON, the sum of every reported attempt in the invocation.

Two ambiguities survive that layering. A provider body may omit the `usage` object entirely, or it may include one with `input_tokens`/`output_tokens`/`total_tokens` partly absent. Both cases are normalized into the same `Usage` value with zeros in the missing positions, so a reader cannot tell "the provider said zero" from "the provider did not say". Cake already logs a warning naming the missing field, but nothing durable records that the number is unknown, and a downstream script that sums `usage.total_tokens` treats an unknown as a measured zero.

Provider identity has the same gap. `AgentRunner` computes the provider response ID for a successful turn, hands it to the agent loop in `TurnResult`, and the loop drops it: `turn_usage` records cannot be tied to the provider response that produced them, and the completed `api_attempt` telemetry record carries neither the resolved provider nor the configured model, so a reader must join it to the preceding `api_attempt_in_flight` record to learn which model ran. The provider's own `model` field, which reports what actually served the response when a gateway routes by alias, is discarded at parse time.

The forces are: keep every existing aggregate field and its meaning; keep the whole path additive, because sidecars and session files are read by scripts that already tolerate new optional fields; never persist raw response bodies, prompts, credentials, or headers; and keep the change small enough to review, since telemetry is read by the session-metrics scripts, not by a library.

## Decision Drivers

- A missing provider number must never read as a measured zero.
- Provider identity must travel with the usage it explains, through the successful turn path, not only through the earlier in-flight record.
- Bounded metadata only: no response bodies, prompts, credentials, or authorization headers.
- Additive compatibility: existing aggregate fields (`task_complete.usage`, completion JSON `usage`, `turn_usage.usage`) keep their shape and meaning.
- One policy for partial usage, documented and testable, rather than per-backend improvisation.
- Provider-specific parsing stays in the backend modules; serialized shapes stay in `src/types/` and `src/session_telemetry.rs`.

## Considered Options

- **Keep `Option<Usage>` as the only presence signal (status quo).** Rejected: it distinguishes "no usage object" from "a usage object", but not a provider-reported zero from an absent counter, which is the ambiguity that misleads cost and context accounting.
- **Make each counter `Option<u64>` inside `Usage`.** Rejected: `Usage` is a sum accumulator (`accumulate_usage`) and is embedded in `turn_usage`, `task_complete`, `session_summary`, judge telemetry, and their snapshots. Per-field optionality would ripple through every consumer and every sum, for a distinction only the reporting layer needs.
- **Wrap usage in a new serialized envelope everywhere.** Rejected: it changes the shape of existing fields (`turn_usage.usage`, `task_complete.usage`) that scripts and snapshots already depend on, for no gain over an additive sibling field.
- **Add a bounded presence marker beside the unchanged `Usage`, on the main-provider attempt and turn records (chosen).** `usage_presence` is `complete`, `partial`, or `unreported`, computed where the provider body is parsed and carried on the parsed result, the parse errors that already carry usage, the settled `turn_usage` record, and the `api_attempt` telemetry record. Aggregate fields keep their exact current shape.
- **Name the absent counters in telemetry too.** Deferred, not rejected: a bounded field list would add a variable-length array to every partial record for information the parse-time warning already logs. The presence marker is the minimum a consumer needs to stop treating unknown as zero.

## Decision Outcome

Chosen option: usage presence is explicit and bounded, and provider identity is carried with the usage it explains.

The contract has three layers, and each record states which one it is:

- **Provider attempt.** `api_attempt` (telemetry, one record per HTTP request) and `turn_usage` (session, one record per attempt that reports usage) both carry the configured model, the provider response ID, and --- when the wire format supplied it and the attempt succeeded --- the provider-reported model. `api_attempt` additionally carries the resolved provider, the attempt's HTTP status, termination, and terminal class. The session record omits the resolved provider because `src/types/` may not import `src/config/` (enforced by `just lint-deps`), so its identity fields are plain strings; a reader that needs the provider takes it from the attempt's own telemetry record.
- **Agent turn.** `turn_count`, conversation records, and the stream-json conversation events describe the logical turn. One turn may hold several attempts; no usage is invented for a turn whose attempts reported none.
- **Task aggregate.** `task_complete.usage` and the completion JSON `usage` remain the sum of the `Usage` values of every attempt that reported one, unchanged in shape and meaning.

`usage_presence` is recorded on the main-provider attempt and turn records with these values:

- `complete`: the `usage` object was present and carried `input_tokens`, `output_tokens`, and `total_tokens`. The optional detail objects (`input_tokens_details`, `output_tokens_details`) never affect presence, because providers legitimately omit them.
- `partial`: the `usage` object was present but at least one of those three counters was absent. The absent counters read zero in `Usage`, the parse logs a warning naming the field, and the record marks the value as partly unknown, so a consumer that needs a known number can exclude it.
- `unreported`: no `usage` object was present, or the attempt failed before a body could report one. The attempt's `usage` is `null`.

An unreported or partial attempt contributes only what it actually reported to the task aggregate, and no attempt is ever assigned an inferred token count. Absent provider identity is omitted from the record rather than filled with a placeholder.

### Consequences

- Good, because a script can now separate measured zeros from unknown values instead of treating both as zero.
- Good, because a `turn_usage` or `api_attempt` record identifies the provider response and the serving model without joining to another record.
- Good, because every addition is an optional field or a new enum value on records that already state "consumers must tolerate added fields", so existing readers keep working.
- Bad, because `partial` is all-or-nothing across the three required counters: a consumer learns that some number is unknown but not which one, and must consult the warning log to find out.
- Bad, because the provider-reported model is captured on the successful parse path only. A terminal `response.failed` event keeps its bounded error metadata and provider response ID but no model, because the failed event's response object is not a full response.
- Bad, because one more bounded field is written on every attempt record, and the session file grows marginally.

## More Information

- Extends the settlement contract in [ADR-025](025-provider-usage-settlement.md) without changing its aggregate meaning; ADR-025's rule that an unreported attempt produces no usage record still holds.
- Complements [ADR-028](028-retry-accepted-body-transport-failures.md): a transport failure while the accepted body is read records `unreported` presence, because a partial body yields no usage that could be inferred.
- Scoped to main-provider attempts and turns. A `judge_attempt` record keeps its existing `usage` field and its own model and API-type fields; the command-safety judge is a separate diagnostic surface and the issue that produced this decision did not cover it.
- The provider-attempt versus agent-turn versus task-aggregate semantics and the presence policy are stated for consumers in [Integrations](../integrations.md).
- Issue #354 owns the implementation; the ExecPlan in `docs/exec-plans/completed/provider-attempt-usage-semantics.md` records how it was delivered.
