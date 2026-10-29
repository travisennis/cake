---
status: accepted
date: 2026-10-29
decision-makers: Travis Ennis
consulted: task 128
informed: tasks 128c, 128d, 128e, 128f
---

# Extract the in-process judge without parity or shadow prerequisites

## Context and Problem Statement

ADR 049 established a generic gate protocol and staged migration. Its extraction path requires exact corpus parity, mandatory comparison in shadow, and one release with the built-in judge still available. Those requirements delay deletion of the in-process policy, provider code, and evaluations even though the external gate can be checked offline against the protocol and Cake's enforcement contract.

Trav has chosen to make external wiring and removal of the built-in policy the goal. Judge-quality work remains a separate plugin task. The external judge remains explicitly disabled in Trav's settings; this decision authorizes no provider calls or live comparisons.

## Decision Drivers

- Complete the Cake-side external gate integration and remove in-process policy ownership.
- Keep no-gate behavior fail-closed: without a `pre_tool` `bash.exec` enforce gate, Cake removes Bash and `BashSession`, except for an explicit recorded bypass.
- Preserve explicit disablement and configured values during settings migration.
- Verify the boundary with offline protocol and enforcement tests that require no model provider.
- Keep generic shadow mode available without making it an extraction prerequisite.

## Considered Options

1. Keep exact oracle parity, mandatory shadow comparison, and a one-release built-in runtime. Rejected because these are judge-quality and rollout policies, not requirements for removing Cake's in-process implementation.
2. Remove the comparison and compatibility prerequisites while retaining the `cake-gate/1` protocol, explicit gate configuration, no-gate enforcement rule, and offline boundary tests. Chosen.
3. Remove comparison and also weaken the no-gate rule or silently activate a replacement judge. Rejected because it would change the security contract or override user configuration.

## Decision Outcome

Chosen option 2. This ADR partially supersedes ADR 049's migration sequencing and rollout prerequisites. It does not replace the protocol, Cake/gate ownership boundary, decision semantics, telemetry contract, or generic shadow capability.

The migration no longer waits for corpus parity, a mandatory dual-judge shadow run, or one release of built-in judge availability. The external integration and built-in deletion are accepted using offline protocol and enforcement tests. Shadow remains an optional comparison mode. Judge quality improvement, corpus parity, and any later enablement are tracked separately from this extraction.

Configuration migration must preserve every explicitly configured value, including `[tools.bash.judge] enabled = false` and `CAKE_JUDGE=off`. If a configured value cannot be represented by the external configuration, Cake must return an explicit settings error that identifies the unsupported value and migration path. It must never silently enable a judge or replace an explicit disablement with an enabled external gate. Trav's own external judge configuration remains disabled, and the extraction performs no live or provider calls.

The no-gate rule from ADR 049 remains: if there is no `pre_tool` `bash.exec` enforce gate, Cake removes Bash and `BashSession`; only an explicit bypass may override that rule, and the bypass is visible and recorded. A configured but unavailable enforce gate does not count as a usable gate.

## Consequences

- Good, because Cake can delete the built-in rubric, provider client, policy evaluations, and benchmark path without waiting on a costly comparison campaign.
- Good, because the protocol and enforcement boundary still receive deterministic offline coverage.
- Good, because user disablement and the no-gate safety floor remain explicit migration invariants.
- Bad, because extraction alone does not demonstrate that the external judge matches the former in-process policy; quality qualification must happen separately before users enable it.
- Bad, because configuration that cannot be mapped requires a clear error instead of an automatic migration.

## More Information

- Partially supersedes the migration portions of [ADR 049](./049-external-policy-gates-with-the-cake-gate-1-protocol.md): parity, mandatory shadow, and one-release built-in runtime are no longer extraction prerequisites. ADR 049 remains authoritative for `cake-gate/1` protocol and enforcement semantics.
- Delivery plan: [external policy gates ExecPlan](../exec-plans/completed/external-policy-gates.md).
- Tasks: 128c and 128d are cancelled; 128e wires the external gate and 128f removes the built-in judge.
