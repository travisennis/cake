---
status: accepted
date: 2026-09-12
decision-makers: Travis Ennis
informed: issue 518
---

# Retry Accepted-Body Transport Failures

## Context and Problem Statement

The bounded provider retry added by #292 recovers a transport failure only while the request is sent: `AgentRunner` classifies an error returned by `send_request` before any HTTP response with `retry::classify_transport_error`. A failure that occurs after the provider accepted the request --- successful response headers, then a reset, truncation, or broken pipe while the response body is read --- passes through the backend's body read (`text()`/`bytes()`) and reaches the parse phase, where `handle_success_response` recorded it as `body_parse` and returned it terminal after a single attempt.

Session telemetry shows this is not a rare class. The 2026-09-06/07 Codex incident recorded `error decoding response body ... error reading a body from connection: connection reset` after `HTTP 200`, and a session audit found 19 `api_attempt` records with `terminal_class: body_parse` and `phase: reading_body`, all `status_code: 200`, all with no reported usage, and all `attempt: 1` with no follow-up attempt, across 17 sessions. Request-phase and HTTP failures in the same corpus do show `attempt: 2/3/4` records, so the retry loop runs for those phases. Every one of the 19 ended the invocation mid-task (after 17, 21, and 1 tool calls in the sampled sessions), even though the work is persisted and resumable.

The judge path had the same fault for its own bounded recovery and fixed it in #287 by classifying a parse-phase failure on its error cause chain rather than its phase. The agent path is the remaining hole.

## Decision Drivers

- The nature of the failure, not the phase in which it surfaces, decides recovery. A connection reset is the transient transport class the existing bounded retry already recovers from.
- Reuse the existing retry cap, backoff, and stale-connection no-reuse policy. A second retry mechanism beside `retry::classify_transport_error` would duplicate the cap and telemetry and could introduce a second budget.
- Do not widen recovery to a complete but malformed body, a stream that ends without a terminal event, a semantic parse failure, or a provider `response.failed` event, which keeps its own classifier.
- A retried body failure must not execute a provider tool call twice, and usage must stay settled per provider attempt: a partial body yields no usage and must not have one inferred from it.
- Keep the failure diagnosable: the terminal error and the `api_attempt` telemetry both retain the nested transport cause.

## Considered Options

- **Keep accepted-body failures terminal (status quo).** Rejected: the evidence shows a transient provider reset converting into a terminal task failure, and the invocation stops mid-task with the work only resumable.
- **Add a body-read-specific retry path with its own budget.** Rejected: it duplicates the cap, backoff, retry telemetry, and no-reuse handling, and creates a second budget to keep coherent with the whole-turn deadline (#109).
- **Classify on the error cause chain and reuse the request-phase transport path (chosen).** A parse-phase failure whose cause chain carries a `reqwest::Error` is handed to the existing classifier, which decides retryability, backoff, and the no-reuse client swap. Typed body-decode errors, semantic parse failures, and `response.failed` events keep their existing terminal paths.
- **Also retry body-decode failures, as the judge does under [ADR-022](./022-retry-undecodable-judge-responses.md).** Rejected for the agent path: a complete 2xx body that arrives but cannot be decoded is a provider, proxy, or envelope incompatibility rather than a transient connection failure, and the two known Cake-side causes of the same opaque message are already fixed (#289 typed reasoning summaries, #291 SSE body sniffing). The judge keeps its own rule because its fail-closed gate has a different cost calculus.

## Decision Outcome

Chosen option: a transport failure raised while the accepted 2xx body is read is retried under the existing bounded policy instead of being recorded as a body-parse failure.

`AgentRunner` classifies the parse-phase error as a body-read transport failure when its cause chain carries a `reqwest::Error`, the error is not a typed `ResponseDecodeError`, and it is not a timeout. Such an attempt records `phase: reading_body`, the accepted `status_code`, `terminal_class: transport`, no usage, and the full error chain. It then takes the same path as a request-phase transport failure: `retry::classify_transport_error` decides the retry against the existing cap and backoff, `should_disable_connection_reuse` swaps in a no-reuse client for the remaining attempts of the turn, and a `retry_scheduled` record names the `network` reason. Both phases share one scheduling routine, so the cap, backoff, telemetry, and no-reuse policy cannot diverge.

Unchanged behavior that this decision depends on:

- Only one phase can fail per attempt, and the accepted body is fully buffered before `complete_turn` returns, so a retried body failure cannot execute a tool call twice.
- Usage is settled per provider attempt from reported usage only. A body that never arrived reports none, the failed attempt records `usage: null`, and the successful retry settles its own usage once, so known usage is not double counted (ADR-025).
- A post-header timeout stays `timeout` and terminal: the accepted body is already bounded by the HTTP deadline, and adding a retried body-read timeout would introduce the second budget that #109 owns.
- A complete but malformed body, a stream without a terminal event, a semantic parse failure, and a provider `response.failed` event remain terminal or keep their existing classifier, and their telemetry class is unchanged.
- The classification is derived from the error, not the backend, so Responses and Chat Completions behave identically.

### Consequences

- Good, because a transient reset or truncated read after accepted headers recovers within the existing bounded retry instead of ending the invocation mid-task.
- Good, because telemetry distinguishes the response-phase transport failure from a body-parse failure through the existing `terminal_class` vocabulary, and the nested cause is retained in both the error and the attempt record.
- Good, because the transport retry has one implementation, so a future change to the cap, backoff, or no-reuse policy applies to both phases.
- Bad, because a provider that deterministically resets every body read for a request now costs four backoff waits and five requests before failing, where it previously failed on the first attempt.
- Bad, because this is client resilience rather than an upstream fix. The reset source remains the provider or the network path, and a correlated burst can still exhaust the cap.

## More Information

- Implements issue #518, `Recover from transient transport failures after accepted response headers`.
- Extends the request-phase transport retry from #292 to the accepted-body phase; the cap, backoff, and stale-connection no-reuse policy are unchanged.
- Deliberately narrower than [ADR-022](./022-retry-undecodable-judge-responses.md), which retries undecodable judge bodies: the agent path retries only the transport nature of a failed body read.
- Coordinates with #109, which owns whole-turn retry budgeting, and with #354/#439 for provider-attempt telemetry integrity. ADR-025 already records that unreported usage stays unmeasurable.
- Token-level streaming (#56) will need its own policy for failures after output has been emitted; this decision covers the current buffered body.
- Current durable authorities remain `docs/configuration.md`, `docs/integrations.md`, `docs/security.md`, and `ARCHITECTURE.md`.
