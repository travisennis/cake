# Prompts Module

The `prompts` module builds the stable system prompt and the mutable context messages derived from `AGENTS.md` files, discovered skills, and environment context.

## Overview

The initial prompt is sent as multiple conversation messages:

1. **System**: Stable identity, "You are cake. You are running as a coding agent..."
2. **Developer context**: Project-specific instructions from `AGENTS.md` files
3. **Developer context**: Available skills catalog with activation instructions
4. **Developer context**: Environment context such as working directory and date
5. **Capabilities**: Implicitly defined by the available tools

For the Responses API, mutable context is sent as individual `developer` role messages in the input array. For Chat Completions, mutable context is folded into the first user message for compatibility with OpenAI-compatible providers that do not support developer role messages consistently.

Each invocation also appends `prompt_context` audit records to the session file
for the mutable context it used. Those records are not replayed on
continue/resume/fork; fresh context is rebuilt and appended for the new
invocation.

The module provides these public functions:

```rust
pub fn resolve_system_prompt(working_dir: &Path, config_dir: &Path) -> String

pub fn build_initial_prompt_messages(
    working_dir: &Path,
    config_dir: &Path,
    agents_files: &[AgentsFile],
    skill_catalog: &SkillCatalog,
) -> Vec<(Role, String)>
```

## System Prompt Resolution

The system prompt is resolved from three sources in precedence order (highest to lowest):

1. **Project-level override**: `.cake/system.md` in the working directory
2. **User-level override**: `system.md` in the user config directory (typically `~/.config/cake/system.md`)
3. **Built-in default**: `system.md` embedded at compile time via `include_str!`

The first readable file found wins. Override files **replace** the default prompt entirely; they do not append to it. Empty files are valid (intentional blank prompt). Unreadable files are skipped with a warning, and resolution falls through to the next source.

The built-in default prompt is stored in `src/prompts/system.md` as proper Markdown. It is embedded into the binary at compile time, so cake never depends on an external file for normal operation.

### Resolution Behavior

| Source | Path | Behavior |
|--------|------|----------|
| Project-level | `.cake/system.md` | Used if present and readable. Takes precedence over all other sources. |
| User-level | `~/.config/cake/system.md` | Used if present and readable, and no project-level file exists. |
| Built-in default | Embedded at compile time | Always available as fallback. |

Edge cases:

- **Empty file**: Valid — the model receives no system prompt content.
- **Whitespace-only file**: Trimmed to empty, same as empty file.
- **Unreadable file**: Skipped with a warning. Resolution continues to the next source.
- **Missing file**: Not an error. Resolution continues to the next source.
- **Session resume/continue**: The system prompt is resolved once at session creation and stored in session metadata. It is not re-resolved on continue or resume.

### Logging

Resolution is logged at debug level:

- `Using project-level system prompt: .cake/system.md`
- `Using user-level system prompt: ~/.config/cake/system.md`
- `Using built-in system prompt (no override found)`

Unreadable files produce a warning:

- `Skipping unreadable system prompt file at <path>: <error>`

## AGENTS.md Files

Cake reads instructions from three locations:

1. **User-level**: `~/.cake/AGENTS.md` — Personal preferences applicable to all projects
2. **XDG config**: `~/.config/AGENTS.md` — XDG-standard location for global instructions
3. **Project-level**: `./AGENTS.md` — Project-specific instructions

All files are optional. If present and non-empty, their contents are injected into a developer context message.

### AgentsFile Struct

```rust
pub struct AgentsFile {
    pub path: String,    // Display path (e.g., "~/.cake/AGENTS.md")
    pub content: String, // File contents
}
```

This struct is defined in the `config` module and populated by `DataDir::read_agents_files()`.

## Prompt Construction

### Base Prompt

The base system prompt establishes the AI's identity and behavioral rules. It is stored in `src/prompts/system.md` as Markdown and embedded at compile time:

```rust
const BUILTIN_SYSTEM_PROMPT: &str = include_str!("system.md");
```

The prompt covers:

1. **Identity**: "You are cake. You are running as a coding agent in a CLI on the user's computer."
2. **Available tools**: Bash, Read, Edit, Write
3. **Efficiency rules**: Prefer targeted edits, batch tool calls, skip unnecessary exploration
4. **Self-reflection notes**: Record mistakes, desires, and learnings in `~/.cake/` files

For details on overriding this prompt, see [System Prompt Resolution](#system-prompt-resolution) above.

### Skills Section

If any skills were discovered, a "Skills" section is emitted as a developer context message:

```markdown
## Skills

<skill_instructions>
The following skills provide specialized instructions for specific tasks.
When a task matches a skill's description, use your file-read tool to load
the SKILL.md at the listed location before proceeding.
When a skill references relative paths, resolve them against the skill's
directory (the parent of SKILL.md) and use absolute paths in tool calls.
</skill_instructions>

<available_skills>
  <skill>
    <name>debugging-cake</name>
    <description>How to investigate and debug issues with the cake CLI tool...</description>
    <location>/path/to/SKILL.md</location>
  </skill>
</available_skills>
```

Skills are lazy-loaded: the model reads the `SKILL.md` file via the Read tool when it determines the skill is relevant. Once activated, the skill is deduplicated (subsequent reads return a lightweight "already active" message).

For full details on the skills system, see [skills.md](./skills.md).

### Additional Context Section

If any `AGENTS.md` files have non-empty content, an additional context section is emitted as a developer context message:

```markdown
## Additional Context

### ~/.cake/AGENTS.md

<instructions>
User-level instructions here...
</instructions>

### ~/.config/AGENTS.md

<instructions>
XDG config instructions here...
</instructions>

### ./AGENTS.md

<instructions>
Project-level instructions here...
</instructions>
```

Empty or whitespace-only files are skipped.

### Example Output

With both files present, prompt construction returns separate messages:

```markdown
system:
You are cake. You are running as a coding agent in a CLI on the user's computer.

---

developer:
## Additional Context

### ~/.cake/AGENTS.md

<instructions>
Always format code with rustfmt before returning it.
Prefer anyhow for error handling.
</instructions>

### ./AGENTS.md

<instructions>
This project uses snake_case for all identifiers.
Run `cargo test` after making changes.
</instructions>
```

Without AGENTS.md files:

```markdown
system:
You are cake. You are running as a coding agent in a CLI on the user's computer.

---

developer:
Current working directory: /project
Today's date: 2026-05-03
```

## Design Decisions

### XML-style Tags

Instructions are wrapped in `<instructions>` tags to:
- Clearly delimit user instructions from system text
- Help the model distinguish context from conversation
- Allow for future nested structure if needed

### File Path Display

The `path` field uses display paths like `~/.cake/AGENTS.md` rather than absolute paths:
- More readable for users
- Consistent across different machines
- Indicates the source (user vs. project level)

### Empty File Filtering

Files with only whitespace are filtered out to:
- Avoid empty additional context sections
- Reduce token usage
- Keep the prompt clean

## Related Documentation

- [cli.md](./cli.md): CLI layer triggers prompt construction via `build_initial_prompt_messages()`
- [session-management.md](./session-management.md): AGENTS.md files are read during session initialization
- [tools.md](./tools.md): Tool definitions are included alongside prompts in API requests

## Integration

The prompt construction flow:

1. **`main.rs`** calls `data_dir.read_agents_files(&current_dir)`
2. **`config::DataDir`** reads and parses `~/.cake/AGENTS.md`, `~/.config/AGENTS.md`, and `./AGENTS.md`
3. **`main.rs`** calls `discover_skills(&current_dir)` to find available skills
4. **`main.rs`** computes the config directory from `dirs::home_dir()`
5. **`main.rs`** passes `current_dir`, `config_dir`, `agents_files`, and `skill_catalog` to `build_initial_prompt_messages()`
6. **`prompts`** resolves the system prompt via `resolve_system_prompt(working_dir, config_dir)`
7. **`prompts`** constructs a stable system message plus separate mutable context messages
8. **`clients::responses`** sends mutable context as developer messages; **`clients::chat_completions`** folds mutable context into the first user message

## Use Cases

### User-Level Instructions

Common patterns for `~/.cake/AGENTS.md`:

- **Code style preferences**: "Prefer functional programming style"
- **Default tools**: "Always run tests after editing code"
- **Error handling**: "Use anyhow for errors, thiserror for libraries"
- **Documentation**: "Add doc comments to all public items"

### XDG Config Instructions

Common patterns for `~/.config/AGENTS.md`:

- **Cross-tool preferences**: Instructions shared with other tools that read `~/.config/AGENTS.md`
- **Global defaults**: Same purpose as `~/.cake/AGENTS.md` but following the XDG Base Directory convention

### Project-Level Instructions

Common patterns for `./AGENTS.md`:

- **Architecture rules**: "Follow the layered architecture in ARCHITECTURE.md"
- **Testing requirements**: "All changes must include tests"
- **Build commands**: "Use `just build` instead of `cargo build`"
- **Project conventions**: "Use `crate::` for imports, never relative paths"

### Combined Context

Both files work together:

- User preferences apply everywhere
- Project rules override or extend for specific projects
- The AI sees both and applies them appropriately

## Testing

The module includes tests for:

- **System prompt resolution**: Override precedence, empty files, whitespace trimming, unreadable files, built-in fallback
- **Empty agents files**: No additional context section added
- **With agents files**: Correct formatting and inclusion
- **Only user file**: Single file in context section
- **Empty content skipped**: Whitespace-only files ignored
- **Snapshot tests**: Full prompt composition for various configurations

Example tests:

```rust
#[test]
fn resolve_uses_project_level_override() {
    let working_dir = TempDir::new().unwrap();
    let config_dir = TempDir::new().unwrap();

    let cake_dir = working_dir.path().join(".cake");
    std::fs::create_dir_all(&cake_dir).unwrap();
    std::fs::write(cake_dir.join("system.md"), "Project prompt").unwrap();

    let prompt = resolve_system_prompt(working_dir.path(), config_dir.path());
    assert_eq!(prompt, "Project prompt");
}

#[test]
fn resolve_uses_builtin_when_no_override() {
    let dir = TempDir::new().unwrap();
    let config_dir = TempDir::new().unwrap();
    let prompt = resolve_system_prompt(dir.path(), config_dir.path());
    assert!(prompt.starts_with("You are cake."));
}
```

## Future Enhancements

Potential improvements:

- **Dynamic prompts**: Include current git status, recent files
- **Template system**: Allow variable substitution in AGENTS.md
- **Conditional rules**: Different instructions based on file type
- **Validation**: Lint AGENTS.md and SKILL.md for common issues
- **Skill dependencies**: Allow skills to declare dependencies on other skills
- **Settings-based system prompt**: Add `system_prompt` key to `settings.toml` and profiles for per-model prompt configuration (see task 151)
- **CLI flag**: Add `--system-prompt <path>` flag for one-off prompt overrides (see task 151)

These would be additions to the current simple, reliable approach rather than replacements.
