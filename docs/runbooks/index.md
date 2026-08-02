# Runbooks

Runbooks own repeatable repository operations. A runbook gives an agent the branches, safety constraints, required evidence, expected interpretations, and recovery actions needed to carry an operation through. Unlike a skill, it does not primarily teach a decision frame or capability. Unlike a reference, it does not primarily record stable facts for lookup.

`AGENTS.md` routes agents to the relevant runbook. A catalog entry may remain under `.agents/skills/` as a discovery aid, but the runbook is the canonical procedure.

## Runbook Catalog

The following runbooks are available here or planned for migration into this category:

### Auditing Binary Size

[Auditing Binary Size](auditing-binary-size.md)

Audit a release binary and identify its principal size contributors.

### Debugging Sandbox Denials

[Debugging Sandbox Denials](debugging-sandbox.md)

Diagnose sandbox failures using the platform-appropriate Seatbelt or Landlock path.

### Working on Branches and Worktrees

[Working on Branches and Worktrees](parallel-worktrees.md)

Carry a change from a branch to a merged pull request, and run several changes at once in linked worktrees.

### Analyzing Cake Sessions

[Analyzing Cake Sessions](analyzing-cake-sessions/index.md)

Analyze persisted session records, with supporting references loaded only when needed.

### Debugging Failed Cake Runs

[Debugging Failed Cake Runs](debugging-cake.md)

Triage a recent failed, interrupted, empty, or truncated cake run.

## Skill Pointer Stub

When a procedure moves from `.agents/skills/<name>/SKILL.md` into this directory, retain the catalog entry as a pointer stub:

```markdown
---
name: <existing name, unchanged>
description: <existing description, unchanged>
---

Follow the repository-owned [<title> runbook](../../../docs/runbooks/<path>)
for this procedure.
```

Copy the existing `name` and `description` frontmatter exactly, including YAML style, quoting, line breaks, and trigger wording. Replace the skill body with the single pointer sentence. The runbook then owns all procedural content; the stub exists only for catalog discovery and compatibility with skill-based entry points.
