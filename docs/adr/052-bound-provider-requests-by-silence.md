---
status: accepted
date: 2026-10-30
decision-makers: Travis Ennis
---

# Bound provider requests by silence instead of total time

## Context and Problem Statement

Provider requests were bounded by a five-minute reqwest total timeout that ran from connect through the end of the body. It measured elapsed work rather than progress, so a healthy turn that kept producing reasoning or tool-argument tokens was killed at five minutes even while data was arriving. Reading the body as one buffered operation also discarded every byte received before the timeout, leaving only a generic request error.

The timeout was the last wall-clock bound on a provider attempt. Removing it entirely would let a silent connection hold a turn open indefinitely, while keeping any fixed total limit preserves the class of false failure this change exists to fix. The bound has to follow progress.

## Decision Drivers

- Progress, not elapsed time, decides whether a provider attempt is healthy.
- A silent connection must still fail so a turn cannot hang forever.
- Reuse the existing retry cap, backoff, no-reuse policy, and exit classification rather than adding a second budget.
- Keep the bytes already received usable for parsing and diagnostics.

## Considered Options

- Keep the total timeout and raise it. Rejected: a larger fixed number still kills a progressing response, only later.
- Remove the timeout entirely. Rejected: a provider or proxy that stops sending would hold the invocation forever.
- Bound the wait for headers and each body chunk independently, resetting on every received chunk (chosen). A response may run as long as it keeps sending data, and only a silent gap fails it.

## Decision Outcome

Chosen: a ten-minute no-received-data stall deadline.

Concretely:

- `AgentRunner` wraps the wait for response headers in the stall window. `build_http_client` no longer sets a total timeout; the existing ten-second connect timeout remains.
- Both backends read the accepted body with `Response::chunk()`. Each chunk resets the window, and bytes are accumulated for the existing JSON and SSE parsers. An SSE comment or keep-alive line counts as received data, so the contract is silence, not semantic progress.
- The window is a constant with no settings key. A value an operator must tune before the default is useful would be a second contract to keep coherent with the retry policy.
- On expiry, `ResponseStall` names the model, phase (`connect`, `first byte`, or `mid-body`), window, and bytes received. The user-facing error keeps a bounded preview of the partial body; telemetry uses the same detail without body bytes.
- A stall before response headers is a request-phase transport failure and takes the existing bounded retry, backoff, and no-reuse policy. A stall after a `2xx` response is terminal `timeout`, never retried, and exits `2`.
- A non-2xx body that stalls keeps its HTTP status class. The stall detail is appended to the terminal error and telemetry so an empty or partial provider diagnostic is still explainable.
- Bytes received before a stall go through the normal parser first. A complete JSON body or an SSE stream that already delivered a terminal event is kept; a terminal `response.failed` event keeps its typed provider metadata and retry classification.

### Security and Compatibility Impact

No new filesystem, network, command-execution, dependency, or credential surface. Requests may now run longer than five minutes, which is the intended behavior change. Exit classification is unchanged for a known status: a non-2xx response keeps its status class, and a post-2xx stall exits `2` like other client-side request timeouts. Session record shapes are unchanged; the attempt records `terminal_class: timeout`. Telemetry still omits provider response bodies: the stall's accepted-body preview is available to the human diagnostic but not to telemetry sinks.

### Consequences

- Good, because a long but progressing turn is no longer killed by elapsed time.
- Good, because a stall after accepted headers keeps its diagnostic instead of collapsing into a generic body-read error.
- Bad, because a peer that sends keep-alive bytes forever can keep a turn open indefinitely; the timeout is now a silence detector, not a total work budget.
- Bad, because buffering still grows with the body until the stream ends. Token-level streaming remains a separate policy problem.

## More Information

- Task 134, from fx `v0.0.15` parity.
- Partially supersedes [ADR 028](028-retry-accepted-body-transport-failures.md): a post-header timeout remains terminal `timeout`, but the accepted body is no longer bounded by the HTTP total deadline; it is bounded per received chunk. ADR 028's retry classification and no-reuse policy otherwise stand.
- [ADR 045](045-classify-request-timeouts-as-api-errors.md) still owns the exit class for provider timeouts.
- [Integrations](../integrations.md) is the living contract for exit codes and retry behavior.
