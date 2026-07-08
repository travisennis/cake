# Prompts, Skills, And Hooks

## Scope

Read this before changing system prompt construction, AGENTS.md discovery, developer-context assembly, skill discovery, skill activation, skill path-watching and SkillActivated records, hook loading, or hook execution.

## Compatibility Surfaces

- AGENTS.md and system prompt search order.
- Skill catalog format, activation trigger, and once-per-session SkillActivated records (path-watching, not output substitution).
- Hook TOML shape, merge behavior, stdin payload, and tool plan actions.
- Tool result and transcript effects from hook mutation, blocking, or appended context.

## Required Checks

- Add tests for prompt assembly, skill catalog changes, or hook lifecycle behavior when touched.
- Snapshot prompt or transcript output when serialized prompt context changes.
- Keep generated indexes and `ahm context ...` workflow guidance authoritative for tasks, research, ExecPlans, and ADRs.

## Common Failure Modes

- Loading too much context into prompts instead of preserving progressive disclosure.
- Activating a skill more than once per session.
- Mutating hook arguments without preserving valid tool schemas.
- Duplicating task, plan, research, or ADR rules already owned by `ahm context task`, `ahm context plan`, `ahm context research`, or `ahm context adr`.

## Guidance Routing

Command guidance is distributed across three layers. Understanding this split prevents overloading any one layer:

- **System prompt**: Broad, high-frequency behavior rules that should shape nearly every task (tool descriptions, efficiency rules, handoff instructions). Avoid charging an always-on token tax for rare guidance.
- **Built-in Bash safety checks**: Rare, concrete command footguns detectable deterministically at execution time --- universal and low-noise (destructive git operations, `rg -rn` footgun, git commit command-substitution). More performant than hooks because they avoid spawning external processes.
- **Hooks**: Local/project/org policy, audit logging, experimentation, checks needing repo-specific state, and subjective command preferences. See [hooks.md](../design-docs/hooks.md).

Built-in checks are slated to become user-ownable policy data (task 241).

## Related Docs

- [prompts.md](../design-docs/prompts.md)
- [skills.md](../design-docs/skills.md)
- [hooks.md](../design-docs/hooks.md)
