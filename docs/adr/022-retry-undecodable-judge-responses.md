---
status: accepted
date: 2026-08-16
decision-makers: Travis Ennis
informed: issue 286
---

# Retry Undecodable Judge Responses

## Context and Problem Statement

The bounded LLM-judge recovery (ADR-020) retries timeouts and retryable transport/HTTP failures but keeps response-parse failures terminal: a 2xx whose body does not deserialize into the expected backend envelope blocks the command with the fail-closed error `safety judge transport error: error decoding response body for url (...)`. Live sessions on the Responses API backend show this failure recurring in clusters: the provider returns HTTP 200, Cake cannot decode the body, and the attempt has no provider response identifier, termination, or usage. The available telemetry cannot distinguish an empty body, non-JSON body, incompatible JSON shape, or a connection-level cause because reqwest's decode error carries the serde reason only in its source chain and the judge drops the raw-body detail.

## Decision Drivers

- Preserve fail-closed: a response that yields no verdict must still block the command before spawn; recovery may only produce a verdict or another fail-closed error.
- Never retry semantic outcomes: a valid `block`/`warn`/`allow`, a refusal, or a malformed verdict stays terminal, unchanged from ADR-020.
- A transient undecodable 2xx must not strand the session: recovery retries once within the existing `timeout_secs + retry_budget_secs` deadline, on a fresh client so connection reuse is excluded from the recovery attempt.
- Decode failures must be diagnosable: the fail-closed detail should name the serde cause and preview the raw body.

## Considered Options

- **Keep response-parse terminal (ADR-020 status quo).** Rejected: the observed failure strands sessions for minutes with no recovery path, and the error text gives no reason for the block.
- **Retry only body-decode failures once like the transport classes (chosen).** The recovery reuses the judge's at-most-one bounded retry, a fresh client with connection reuse disabled, and the identical request under the same settings, rubric, and allowlist. A second undecodable response fails closed exactly like today. Later semantic backend parse failures remain terminal because repeating a structurally valid but unusable model response is not the transient transport condition addressed here.
- **Silently tolerate malformed envelopes.** Rejected: decoding garbage as a verdict would break the verdict-code validation and the fail-closed guarantee.

## Decision Outcome

Chosen option: treat an undecodable 2xx body as a retryable transient failure in the judge's bounded recovery. A typed body-decode error schedules the same at-most-one recovery as the timeout/transport/HTTP classes: a backoff wait from the judge retry policy, a fresh HTTP client (connection reuse disabled), and the identical request. Other errors produced after successful envelope decoding remain terminal. The complete operation stays bounded by `timeout_secs + retry_budget_secs`; `retry_budget_secs = 0` still disables recovery; an exhausted recovery fails closed before spawn.

The fail-closed detail now renders the full error chain (the serde cause behind reqwest's opaque message), and both backend parse paths (`responses.rs`, `chat_completions.rs`) read the body once and attach a bounded 400-byte preview on decode failure, so an empty body, an HTML proxy page, or a wrong envelope is identifiable in the error text.

### Consequences

- Good, because a transient empty or non-JSON 2xx from a proxy recovers within the bounded deadline instead of blocking commands until the session restarts.
- Good, because decode failures are diagnosable from the fail-closed detail (serde cause plus body preview) without a raw diagnostic surface.
- Bad, because a provider that deterministically returns an undecodable body for a given request now costs a backoff wait plus a second request before failing closed.
- Bad, because this is client resilience rather than an upstream root-cause fix. Correlated invalid responses can still exhaust both attempts; the new detail is needed to determine whether the provider returned an empty body, non-JSON body, or incompatible envelope before pursuing a provider-specific correction.

## More Information

- Partially supersedes [ADR-020](./020-bounded-llm-judge-recovery.md), `Bounded LLM-Judge Recovery`: undecodable response bodies are removed from its terminal response-parse class; semantic backend parse failures remain terminal, recovery still never enters with a verdict in hand, and ADR-020 remains accepted for the timeout/transport/HTTP recovery.
- Builds on ADR-018 (`LLM Judge Command Gate`) and ADR-020: the fail-closed judge gate and its bounded recovery are unchanged in every other respect.
- Current durable authorities remain `docs/security.md`, `docs/configuration.md`, `docs/integrations.md`, and `ARCHITECTURE.md`.
