---
status: accepted
date: 2026-09-18
decision-makers: Travis Ennis
informed: issue 602
---

# Evaluate TypeSafe judgments without execution authority

## Context and Problem Statement

The existing Bash safety judge adds provider latency to every command. A focused typed judgment may eventually approve clearly observational commands faster, but its eligibility accuracy and latency must be measured before it can authorize execution. Trav authorized the first PR implementing a TypeSafe client, shadow mode, and evaluation support.

## Decision Drivers

We need representative measurements without changing command authorization, a reproducible model version, bounded observation overhead, and protection of credentials and metadata-only telemetry.

## Considered Options

Replacing the judge immediately would combine integration risk with a new approval policy. A standalone evaluator would avoid runtime changes but miss the shared request context and operational behavior. An opt-in observation through the shared judge path supplies both corpus and runtime evidence while retaining the existing authority.

## Decision Outcome

Add a dedicated TypeSafe HTTP client and default-off shadow mode. Use TYPESAFE_AI_API_KEY for bearer authentication. Jev receives the command and effective judgment context and returns a probability that the whole command qualifies for immediate approval under the rubric. Advisory-only footgun warnings, including rg -rn, do not disqualify an otherwise safe observational command; hooks retain their warning responsibility. Existing primary-rubric warning behavior is unchanged and any disagreement is reported separately from unsafe approval. Shadow answers never approve or block commands. The existing judge, fail-closed behavior, allowlist, emergency bypass, and OS sandbox retain their authority. This extends observation around ADR 018 and does not supersede its execution decision.

Shadow requests run concurrently with primary evaluation under a short total deadline and without retries. Off and emergency bypass make no TypeSafe request. Missing credentials and provider failures are observation failures only. Configuration explicitly opts users into transmitting command/context to an additional provider. Local telemetry records bounded metadata rather than raw commands, credentials, reasons, or response bodies. Evaluation reports distinguish independent labels from judge disagreement and estimated sequential savings from measured parallel latency.

### Consequences

Users can evaluate a candidate fast-approval model before adopting it. Shadow mode adds provider cost and can add bounded delay when the primary judge completes first. Typed answers do not prove semantic correctness. A production cascade remains a separate decision supported by evaluation evidence.

## More Information

Issue #602 and docs/exec-plans/completed/typesafe-judge-shadow.md describe delivery. ADR 018 remains the command-approval authority, with ADR 020 governing its bounded recovery. The configuration, security, and integration documents describe the implemented contracts.
