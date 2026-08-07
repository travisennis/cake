# Agent-facing instructions

## Scope

Read this before adding, removing, or reordering instructions in [AGENTS.md](../../AGENTS.md), the skills under `.agents/skills/`, or any other prose whose purpose is to change how an agent behaves.

This guardrail governs the evidence an instruction change requires. It does not govern user documentation, external contracts, or architecture, which are covered by their own authorities and by [CONTRIBUTING.md](../../CONTRIBUTING.md).

## Adding an instruction

Every instruction costs context on every session that loads it, and the corpus only grows if adding is cheaper than removing. So the evidence requirement falls on additions:

- Name the observed failure the instruction prevents, citing a session, commit, or GitHub issue, in the pull request or the commit message.
- State the behavior an agent will exhibit that it does not exhibit today.
- Say what the instruction displaces. `just lint-instruction-size` enforces a word budget per document; an addition that breaks the budget must cut something or raise the budget deliberately.

An instruction that restates what a linked document already says is not an addition worth making. Route to the authority instead.

## Removing an instruction

Removing an instruction is ordinary editing and needs only the normal documentation checks. Prefer removal when an instruction duplicates its authority, describes a failure a mechanical check now prevents, or has never demonstrably changed a trajectory.

Deleting a prohibition does not grant permission --- an agent falls back to its own defaults. When the intent is to allow something previously forbidden, state the permission affirmatively rather than dropping the rule.

## Provenance

This guardrail originally required a fresh-session probe before an instruction could be removed, and required nothing of additions. That was a ratchet. AGENTS.md was 454 words when the rule was restored in `72b69ed` on 2026-07-26 and 1,796 words on 2026-08-05 --- 295% growth with the rule in force for every commit of it, because each addition arrived inside a feature pull request whose motivating failure was free to name while removal cost a probe. The direction of the requirement was inverted on 2026-08-06 and the probe requirement was replaced by the word budget, which is the mechanical check this document's own retirement clause called for.

Retire this guardrail if the budget alone holds the corpus flat, which would show the prose is redundant. Record the evidence.
