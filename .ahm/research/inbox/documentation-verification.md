# Behavioral Verification of Agent-Facing Instruction Changes

Agent-facing instruction edits (AGENTS.md, guardrails, skills) previously
landed with consistency checks only (panache lint, index freshness, links).
Those checks show the docs agree with each other; they do not show an agent
behaves differently. Example baseline: commit e13b401 added two
behavior-shaping operating-loop instructions with no record of the failure
motivating them and no behavioral verification.

## Decision (2026-07-19)

Added an "Agent-Facing Instruction Changes" section to
`docs/guardrails/documentation.md`: behavior-shaping edits must name the
observed failure motivating them, state the expected observable behavior
change, and run the narrowest fresh probe — or record deferred verification
with the probe defined. Consistency edits are exempt.

## Evidence

One fresh probe trajectory (isolated worktree at cake aa18b80 + the guardrail
edit; worker: Claude Code, claude-fable-5, fresh session, no knowledge of the
edit). Job: a bare maintainer request to add a behavior-shaping repository
rule (run `cargo fmt` on touched Rust files before handoff), with no
motivating failure supplied.

- The worker routed to the documentation guardrail, classified the edit as
  behavior-shaping, recorded the motivation source and expected observable
  behavior change, and created an ahm task deferring the probe with the exact
  probe defined — citing the new guardrail section as the requirement.
- Baseline comparison: the e13b401-class trajectory produced none of these
  artifacts.

Limits: a single before/after pair under one worker configuration. It shows
the section is retrievable and actionable; it does not establish a general
treatment effect, nor that deferred probes get closed.

## Follow-up / retirement condition

- Watch the next few real behavior-shaping instruction edits: if deferred
  probes accumulate without ever being run, the check is producing paperwork,
  not proof — revise (make the probe cheaper, e.g. a canned representative
  task per route) or remove the section.
- Sources on documentation practice collected earlier:
  - <https://architecture.md/>
  - <https://adr.github.io/>
  - <https://mozillascience.github.io/working-open-workshop/contributing/>
