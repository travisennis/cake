# Documentation

## Scope

Read this before auditing or updating durable documentation, generated indexes, design docs, references, ADRs, README, CONTRIBUTING, ARCHITECTURE, or agent-facing instructions.

## Compatibility Surfaces

- User-facing setup, CLI, configuration, sandbox, session, and output docs.
- Architecture maps and implementation-location references.
- Generated indexes owned by `ahm`.
- ADR decision history and status metadata.
- Progressive-disclosure routing in `AGENTS.md`.

## Required Checks

- Run `ahm context docs` before doc work.
- Do not edit generated indexes by hand.
- Prefer one authoritative home for each rule; link instead of duplicating.
- Run the narrowest useful Markdown, link, or generated-index check available.

## Agent-Facing Instruction Changes

Instructions in `AGENTS.md`, guardrails, and skills exist to change agent behavior. A consistency edit (fixing drift, links, or duplication against code or existing docs) needs only the checks above. A behavior-shaping edit --- one that adds, removes, or reorders what an agent should do --- additionally requires:

- Name the observed failure motivating the edit (session, commit, or ahm record) in the ahm task or commit message.
- State the observable behavior the edit is expected to change.
- Verify with the narrowest fresh probe that exercises the instruction: run a representative task in a fresh agent session and check that the instruction was retrieved and followed. Session files and the `analyzing-cake-sessions` skill provide the evidence.
- When a probe is not run, record that verification is deferred and which probe would establish it.

A green consistency check shows the docs agree with each other; it does not show an agent behaves differently. An instruction a trajectory never used has no evidence of effect.

## Common Failure Modes

- Letting AGENTS.md become a full manual again.
- Adding a new doc when an existing doc is the right authority.
- Updating behavior docs but missing README examples or design references.
- Rewriting ADR history instead of adding a new decision or supersession note.
- Landing a behavior-shaping instruction edit with no motivating failure or behavioral evidence recorded.

## ahm Workflows

- ahm context docs - for managing docs
- ahm context adr - for managing ADRs

## Related Docs

- [docs/design-docs/index.md](../design-docs/index.md)
- [CONTRIBUTING.md](../../CONTRIBUTING.md)
