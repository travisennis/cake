# Agent-facing instructions

## Scope

Read this before adding, removing, or reordering instructions in [AGENTS.md](../../AGENTS.md), the skills under `.agents/skills/`, or any other prose whose purpose is to change how an agent behaves.

This guardrail governs the evidence an instruction change requires. It does not govern user documentation, external contracts, or architecture, which are covered by their own authorities and by [CONTRIBUTING.md](../../CONTRIBUTING.md).

## Required evidence

A consistency edit --- one that fixes drift, links, or duplication against code or existing documentation --- needs only the normal documentation checks.

A behavior-shaping edit --- one that adds, removes, or reorders what an agent should do --- additionally requires:

- Name the observed failure motivating the edit, citing a session, commit, or `ahm` record, in the managed task or the commit message.
- State the observable behavior the edit is expected to change.
- Verify with the narrowest fresh probe that exercises the instruction: run a representative task in a fresh agent session and check that the instruction was retrieved and followed. Session files and the [Analyzing Cake Sessions runbook](../runbooks/analyzing-cake-sessions/index.md) provide the evidence.
- When a probe is not run, record that verification is deferred and which probe would establish it.

A green consistency check shows that the documents agree with each other; it does not show that an agent behaves differently. An instruction no trajectory ever used has no evidence of effect.

## Provenance and retirement

Motivating failure: commit `e13b401` added two behavior-shaping operating-loop instructions with no record of the failure motivating them and no behavioral verification. The rule was first established on 2026-07-19 in `a708105`, verified by one fresh-session probe recorded in `.ahm/research/inbox/documentation-verification.md`, removed on 2026-07-24 by the documentation reset in `c16b0e0`, and restored here.

Retire this guardrail when deferred probes accumulate without ever being run, which would show the requirement is producing paperwork rather than evidence, or when a mechanical check can establish the same thing. Removing it for document volume alone is not a reason; record the evidence that it stopped working.
