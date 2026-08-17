---
status: accepted
date: 2026-08-14
decision-makers: Travis Ennis
informed: issue 204
---

# Bounded LLM-Judge Recovery

## Context and Problem Statement

ADR-018 made the LLM judge the only non-sandbox command gate, fail-closed, with "no retries in version 1" and whole-turn retry semantics deferred to a separate revisit. Evidence since then shows that one un-retried transient request is not reliable enough for a gate that runs before every non-empty Bash command. Persisted sessions in issue #204 record two identical `gh pr merge` calls timing out after a full 30 seconds each before the third returned an allow verdict in 14.7 seconds, and 7 of 412 judge events timed out across the ten judge-enabled sessions examined. Fail-closed behavior is correct, but a provider stall or stale pooled connection turns a recoverable transient into a repeated fail-closed block that strands the task.

## Decision Drivers

- Preserve fail-closed: a judge that cannot produce a verdict must still block the command before spawn.
- Bound total latency: recovery must not double the gate's worst-case wait (two full 30-second periods) without an explicit, documented settings contract.
- Never retry semantic outcomes: a valid `block`/`warn`/`allow`, a refusal, or a malformed verdict must stay terminal; a block must never be retried in search of an allow.
- Reuse the agent loop's established retry classification rather than inventing a parallel vocabulary.
- Keep the recovery measurable: per-attempt telemetry and the #205 SLO benchmark must show the cost and the availability effect.

## Considered Options

- **No retry (ADR-018 version-1 behavior).** Fail closed on the first timeout or transport error. Rejected because the evidence shows transient provider stalls and stale pooled connections recur often enough to strand real sessions, and the failure is visible only as repeated 30-second blocks.
- **Increase `timeout_secs`.** Rejected because it only makes the current failure mode slower; it never recovers the request that already stalled.
- **One bounded recovery attempt within a documented deadline (chosen).** A timeout or retryable transport/HTTP failure may trigger at most one recovery attempt; the complete operation is bounded by `timeout_secs + retry_budget_secs`, so the worst case stays below two full timeout periods and the settings contract documents the budget.

## Decision Outcome

Chosen option: one bounded recovery attempt within a documented deadline. The judge makes at most two provider calls per evaluation. A new `[tools.bash.judge] retry_budget_secs` setting (default 15; `0` disables recovery) adds an explicit budget beyond the unchanged per-call `timeout_secs` (default 30); the complete operation is bounded by `timeout_secs + retry_budget_secs` (45 seconds with defaults).

Recovery triggers only on error outcomes: a judge timeout, a transport error classified retryable by the agent loop's `retry::classify_transport_error`, or an HTTP failure classified retryable by `retry::classify_http_failure` (rate limit, overload, server error). Valid verdicts, refusals, malformed verdicts, and response-parse failures remain terminal. Recovery re-sends the identical request under the same settings, rubric, and allowlist; it honors the provider's `Retry-After` up to a 5-second cap, and it swaps in a fresh HTTP client (connection reuse disabled) when the failure may involve a stale connection or stalled request, mirroring the agent runner. An exhausted recovery fails closed exactly like an un-retried failure: the Bash command is blocked before spawn with the final fail-closed class.

`cake bash check` and the Bash preflight share the same observed judge driver, so both get identical retry and deadline semantics. Every provider attempt is recorded as a metadata-only `judge_attempt` telemetry record carrying attempt/retry ordinal, retry reason, backoff wait, and the effective operation deadline; no command, reason, prompt, or response text enters telemetry.

### Consequences

- Good, because transient provider stalls and stale pooled connections recover without a manual retry, and the #205 benchmark measures whether availability improves without hiding the extra latency.
- Good, because the total worst-case wait is bounded below two full timeout periods and is explicit in configuration, integration, and security documentation.
- Good, because semantic outcomes are never retried: the gate cannot be talked into an allow by retrying, and refusals/malformed verdicts still fail closed.
- Bad, because a judge operation can now consume up to 45 seconds (with defaults) before failing closed, and a failed first attempt always costs a backoff wait plus a second request.

## More Information

- Partially supersedes [ADR-018](./018-llm-judge-command-gate.md), `LLM Judge Command Gate`: the "no retries in version 1" clause is replaced by this decision; ADR-018 remains accepted for the gate itself.
- Partially superseded by [ADR-022](./022-retry-undecodable-judge-responses.md), `Retry Undecodable Judge Responses`: body-decode failures are removed from the terminal response-parse class and get one bounded recovery attempt on a fresh client; semantic backend parse failures remain terminal, and this decision remains accepted for the timeout/transport/HTTP recovery.
- Implements issue #204, `Retry transient LLM-judge failures within a bounded deadline`.
- Builds on #202 (per-attempt judge diagnostics) for the retry telemetry and on #205 (judge SLO benchmark) for measuring the recovery's effect.
- Execution plan: `docs/exec-plans/completed/judge-retry-bounded-deadline.md`.
- Current durable authorities remain `docs/security.md`, `docs/configuration.md`, `docs/integrations.md`, and `ARCHITECTURE.md`.
