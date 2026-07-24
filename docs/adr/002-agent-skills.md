---
status: accepted
date: 2025-04-25
---

# Agent Skills System (Original)

## Context

As cake evolved, users needed a way to provide specialized instructions for specific tasks without bloating the system prompt. For example, debugging the cake CLI itself requires different procedures than evaluating a session or creating an EPUB. Pre-loading all possible instructions into the system prompt wastes tokens and overwhelms the model with irrelevant context.

We evaluated several approaches for making specialized knowledge available to the agent on demand.

## Decision (as amended by 2026-07-07)

We implement a skills system with the following design:

1. **Discovery**: Skills are discovered from `.agents/skills/<skill-name>/SKILL.md` directories at both project (`./.agents/skills/`) and user (`~/.agents/skills/`) levels.
2. **Parsing**: Each `SKILL.md` file contains YAML frontmatter with `name` and `description` fields, followed by markdown body content.
3. **Catalog disclosure**: Discovered skills are listed in the system prompt as an XML `<available_skills>` catalog, telling the model which skills exist and where to find them.
4. **Lazy activation**: The model uses the existing `Read` tool to load a `SKILL.md` file when its description matches the current task. The skill content is then in the conversation context. A `SkillActivated` session record is emitted once per skill per session on the first read, but the Read tool always returns the actual file contents.
5. **No deduplication**: Every `Read` of a `SKILL.md` returns the file contents like any other read. There is no interception, no state machine, and no "already active" message. The model retains full control over when and what it reads.
6. **Configuration**: Users can disable skills (`--no-skills`), filter to specific skills (`--skills name1,name2`), or configure defaults in `settings.toml`.

## Rationale

- **On-demand loading**: Skills are loaded only when needed, keeping the system prompt minimal and focused.
- **Existing tool reuse**: The model already has a `Read` tool. Skill activation uses it without adding new mechanics.
- **Familiar format**: YAML frontmatter is widely understood (Jekyll, Hugo, etc.), making skill authoring accessible.
- **No harness substitution**: The old dedup state machine withheld an information-access decision from the model. Removing it restores the model's ability to re-read skills when needed (e.g., during review workflows where skill files may have changed).
- **Session persistence**: `SkillActivated` session records are still emitted once per session per skill via path-watching, enabling activity tracking without output substitution.

## Consequences

- **Positive**: Reduced system prompt size, better model focus, easy to add new specialized knowledge
- **Positive**: Skills are plain markdown files with YAML frontmatter, no special tooling needed to author them
- **Positive**: \~300 lines of state machine and plumbing removed; simpler agent code
- **Positive**: The model can re-read a skill at any time, which is useful during review or when skill files change mid-session
- **Negative**: The model may waste tokens if it reads the same skill multiple times (mitigated by the model's own judgement)
- **Negative**: Skill discovery has a small filesystem scan cost at startup (mitigated by depth/directory limits)

## Alternatives Considered

- **Inline all skills in system prompt**: Rejected because it bloats the prompt with potentially irrelevant instructions and increases token costs.
- **Dedicated skill activation tool**: Rejected because it duplicates the existing `Read` tool. Using `Read` keeps the tool surface minimal.
- **Auto-activate based on keyword matching**: Rejected because it is brittle and could activate skills incorrectly. The model makes the activation decision based on its understanding of the task.
- **Store skill content in settings.toml**: Rejected because markdown files are easier to author and version control than embedded TOML strings.
- **Dedup with "already active" message (original)**: Replaced by the current design. Withheld the information-access decision from the model and blocked legitimate re-reads.

## Amendment Record

2026-07-07: Decision point 5 changed from "Deduplication" to "No deduplication". The Read interception state machine (`skill_dedup.rs`) and its agent plumbing were removed. `SkillActivated` records are still emitted on first read per skill per session via path-watching (no output substitution). See task 235.

## References

- `docs/design-docs/skills.md` - Full feature documentation
- `src/config/skills.rs` - Skill discovery and parsing implementation
- `src/prompts/mod.rs` - System prompt integration
- `src/clients/agent.rs` - Path-watching skill activation tracking
