---
status: accepted
date: 2026-10-15
decision-makers: Travis Ennis
---

# Classify provider request timeouts as API errors

## Context and Problem Statement

Cake exits `2` for a provider failure it observed as a network condition and `1` for a provider failure it observed as an HTTP status. Timeout-adjacent failures straddle that line: a `reqwest::Error::is_timeout()`, a connect failure, and the typed judge timeout all map to `2`, while an HTTP `408` from the same provider mapped to `1`, because the status list in `classify_api_error_status` covered only `401`, `403`, and `429`.

The exit class is a compatibility surface: scripts and CI branch on `$?`. Cake therefore needs one stated policy for which provider failures are class `2`, not an accident of which layer noticed the failure.

## Decision Drivers

- The same provider failure should exit the same way whichever layer observed it (transport, status code, or typed timeout).
- Retry behavior and exit classification answer different questions. Retrying asks whether re-sending might succeed; classifying asks who failed.
- The class-2 set must stay small and explainable, because every addition is a breaking change for `$?` consumers.
- The tool boundary must stay explicit: a Bash tool timeout is not a provider failure.

## Considered Options

- Add `408` only (chosen).
- Also move `409` (lock timeout) and `5xx`/`529` to class `2`, grouping everything retryable. Rejected: those are answers from a reachable provider, and the retry policy already covers them; tying the class to retryability would make the class change whenever the retry policy does.
- Also move the gateway timeout `504`, because the name says timeout. Rejected: `504` is an upstream response the provider relayed, not a failure of cake's own request, and it arrives with the rest of `5xx`. A gateway that answers has answered.
- Leave the divergence and document "statuses win". Rejected: it is not a decision, only a description of the bug.

## Decision Outcome

Chosen: HTTP `408` joins class `2`; every other status mapping is unchanged.

Concretely:

- `classify_api_error_status` in `src/exit_code.rs` maps `401 | 403 | 408 | 429` to `code::API_ERROR` and everything else to `code::AGENT_ERROR`.
- The reqwest status path (`is_reqwest_api_error`) delegates to that function instead of repeating the status list, so the two status sites cannot drift again.
- Retryability is documented as a separate transport-recovery concern. `409`, `5xx`, and `529` remain retried (`src/clients/retry.rs` is untouched) and remain class `1`.
- A tool-level timeout is a tool error, class `1`. This decision covers provider and API status classification only.

### Security and Compatibility Impact

The change adds no filesystem, network, command-execution, or dependency surface; it is a two-branch change in a pure classification function. It is breaking for scripts that branch on `$?`: an exhausted `408` now exits `2` instead of `1`. The classification depends on provider-supplied status codes, so a provider that reports a timeout as a non-`408` status is classified by that status instead. Every other status keeps its documented class, so no other exit behavior moves.

### Consequences

- Good, because a provider request timeout exits like the client-side timeouts it is indistinguishable from in practice.
- Good, because the status policy now lives in one function, with the boundary pinned by tests for `408`, `409`, `5xx`, and `504`.
- Bad, because class `2` becomes one status wider for automation that treats `2` as "credentials or quota". Accepted: the class is documented as the provider-failure class, and a timeout is a provider-side outcome.
- Accepted limitation: a tool-level timeout stays class `1`, so a consumer cannot read "process exited `2`" as "everything timed out".

## More Information

- Task 055, from issue [#458](https://github.com/travisennis/cake/issues/458), finding C9.
- [Integrations](../integrations.md) states the class composition and the compatibility change.
