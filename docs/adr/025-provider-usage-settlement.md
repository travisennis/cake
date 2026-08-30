---
status: accepted
date: 2026-08-30
decision-makers: Cake maintainers
consulted: issue #346
informed: issue #346
---

# Settle Provider Usage Before Turn Classification

## Context and Problem Statement

Cake's persisted `turn_usage` record was written only after a provider response became a completed agent turn. Retryable attempts, terminal failures, and responses discarded during context-overflow recovery can still report billable token usage. If the agent loop classifies or discards those responses first, session totals understate provider spend and the session cannot explain the discrepancy.

The existing session format is version 4 and append-only. Live `stream-json` is a separate task event feed and must not become a copy of the session audit ledger.

## Decision Drivers

- Preserve reported provider cost even when response recovery or the agent task fails.
- Keep provider-attempt failures distinguishable without storing raw provider bodies.
- Keep old session records readable and avoid a session-format migration.
- Keep session-only audit records out of stream-json.
- Match the provider billing boundary: each request that reports usage is billable independently.

## Considered Options

- Add a new sibling session record kind for failed usage. Rejected because consumers would need a second usage-ledger branch even though the existing `turn_usage` record already represents normalized usage.
- Store only one total record after the logical turn ends. Rejected because a process can fail during retry or backoff, and a single post-classification write cannot guarantee durability for each billed attempt.
- Extend `turn_usage` and settle one record per reported provider attempt before retry or discard classification. Chosen because it matches the billing boundary, reuses the existing append-only record path, and makes each attempt independently auditable.

## Decision Outcome

Chosen option: extend `turn_usage` with optional 1-based `attempt` and bounded `terminal_class` fields, and write one record for every provider attempt that reports normalized usage before retry, terminal classification, or response discard. A first successful attempt omits the optional fields to preserve the established serialized shape. Retried, failed, and discarded attempts include them. `task_complete.usage` and telemetry totals include every reported attempt. A transport failure that reports no usage produces no usage record because Cake cannot infer unreported tokens.

### Consequences

- Good, because usage remains durable when the result is not durable.
- Good, because a retry or discarded response is attributable to its provider-attempt ordinal and terminal class.
- Good, because old records remain readable, session format version 4 remains valid, and stream-json shapes do not change.
- Bad, because a session may contain multiple `turn_usage` records for one logical turn when retries occur.
- Bad, because usage that a provider never reports, such as tokens lost in a mid-stream transport death, remains unmeasurable.

## More Information

- [Issue #346](https://github.com/travisennis/cake/issues/346)
- [Integration contracts](../integrations.md)
- [Append-only session task events](004-append-only-session-task-events.md)
- [Execution plan](../exec-plans/completed/failed-provider-turn-usage.md)
