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

## Common Failure Modes

- Letting AGENTS.md become a full manual again.
- Adding a new doc when an existing doc is the right authority.
- Updating behavior docs but missing README examples or design references.
- Rewriting ADR history instead of adding a new decision or supersession note.

## ahm Workflows

- ahm context docs - for managing docs
- ahm context adr - for managing ADRs

## Related Docs

- [docs/design-docs/index.md](../design-docs/index.md)
- [CONTRIBUTING.md](../../CONTRIBUTING.md)
