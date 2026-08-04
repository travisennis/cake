# Skills vs Runbooks: Cake's Agent-Instruction Content Model

Status: active Created: 2026-07-27 Updated: 2026-07-27 Related tasks: - Related plans: - Confidence: medium

## Summary

Most of the files under `.agents/skills/` are repeatable repository procedures, not teachable approaches. Under a three-way split --- skill, runbook, reference --- they belong in a runbook category the repository does not currently have.

Proposed distinction:

- **Skill**: teaches a decision frame, capability, or way of approaching work.
- **Runbook**: owns a repeatable repository operation, including branches, safety, evidence, and recovery.
- **Reference**: records stable formats, commands, and diagnostic interpretation.

## Notes / Evidence

  | Skill                     | Recommendation                        | Reason                                                                                                                                                                                                                                                                                  |
  | ------------------------- | ------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
  | `auditing-binary-size`    | Convert to runbook                    | Explicit prerequisites and numbered commands for a repeatable audit: `.agents/skills/auditing-binary-size/SKILL.md:10`                                                                                                                                                                  |
  | `debugging-cake`          | Convert to runbook                    | A six-step incident-triage procedure with expected interpretations and recovery actions: `.agents/skills/debugging-cake/SKILL.md:20`                                                                                                                                                    |
  | `debugging-sandbox`       | Convert to runbook                    | Operational diagnosis, tracing, source repair, and verification. Should branch explicitly between macOS Seatbelt and Linux Landlock: `.agents/skills/debugging-sandbox/SKILL.md:10`                                                                                                     |
  | `analyzing-cake-sessions` | Convert, with references              | A five-phase analysis procedure plus output contract and checklist: `.agents/skills/analyzing-cake-sessions/SKILL.md:13`. Keep the `jq` cookbook and Edit-analysis material as supporting references.                                                                                   |
  | `grooming-backlog`        | Convert upstream                      | Pure repeatable maintenance: inspect, classify, mutate, regenerate indexes. Supplied by the shared skills collection, which should own the canonical runbook rather than cake.                                                                                                          |
  | `preflight`               | Convert or split upstream             | The numbered execution procedure is a runbook; its three review lenses are useful model judgment and could remain as a much shorter skill. Also upstream-managed.                                                                                                                       |
  | `finding-improvements`    | Split upstream                        | The advisor stance, evidence standard, and leverage rubric teach an approach; the recon→audit→selection→task-creation sequence is a runbook. Upstream is its real owner.                                                                                                                |
  | `delegating-to-cake`      | Keep as a skill, distribute elsewhere | It teaches a parent agent how to use cake and establishes a capability/authority boundary — closer to a genuine skill than a cake repository workflow. More useful as a shared/user skill because an agent working another repository will not discover cake's project-local directory. |

Supporting local evidence: completed task 247 (recorded in the pre-migration task history) called these files "playbooks written for exactly the situations" agents encounter.

## Implications for this project

Cake currently defines skills more broadly than this split does. [ADR 002](../../../docs/adr/002-agent-skills.md) explicitly names debugging cake and evaluating sessions as examples of skill-worthy specialized instructions. Conversion is therefore a change to cake's content model, not cleanup, and the ADR needs an amendment recording the three-way distinction.

Migration order:

1. Move the four clearest procedures --- binary audit, cake failure triage, sandbox diagnosis, and session analysis --- to repository-owned runbooks.
2. Route them explicitly from `AGENTS.md` and the configuration, security, and integration docs.
3. Fix the upstream-supplied workflows at their owner instead of forking cake-local copies.
4. Split `preflight` and `finding-improvements` only if their judgment-bearing cores remain meaningfully useful as skills.
5. Keep `delegating-to-cake`, but distribute it where parent agents working other repositories can discover it.

Do not remove the existing skills first and add runbooks later. Cake has already measured agents skipping preflight and other operating-loop steps, and [the agent-instruction guardrail](../../../docs/guardrails/agent-instructions.md) requires a fresh-session probe for behavior-shaping changes. The migration must prove that agents retrieve the new runbook routes at least as reliably as the current skill catalog.

## Follow-ups

- The source framing for the skill/runbook distinction is external and is not reproduced here; the argument rests on the definitions above rather than on a citable authority in this repository.

## Provenance

Captured 2026-07-27 from an external agent's review of `.agents/skills/`, originally left at the repository root as `skills-research.md`. Maintainer decisions on 2026-07-27: runbooks live in `docs/runbooks/`; converted skills shrink to pointer stubs rather than being deleted; the three upstream-managed skills are out of scope for now.
