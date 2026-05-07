# Agent Skills Implementation Plan for cake

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `.agents/PLANS.md` from the repository root. It was migrated from the former `.agents/.plans/` location after implementation evidence showed the skills feature is present in the current codebase.

## Purpose / Big Picture

Cake should discover reusable agent skills from checked-in and user-level `SKILL.md` files, disclose those skills to the model without loading every skill body into the initial prompt, and let the model activate a relevant skill by reading it. After this work, a user can place a skill under `.agents/skills/<name>/SKILL.md`, run cake normally, and see that the skill appears in the prompt context and can be loaded on demand.

The feature is observable by running tests for skill discovery and prompts, by inspecting `docs/design-docs/skills.md`, and by seeing project skills such as `.agents/skills/debugging-cake/SKILL.md` appear in the available skill catalog.

## Progress

- [x] (2026-05-07 18:35Z) Confirmed `src/config/skills.rs` implements parsing, discovery, filtering, diagnostics, configured skill paths, XML catalog generation, and tests.
- [x] (2026-05-07 18:35Z) Confirmed `src/main.rs` wires `--no-skills`, `--skills`, configured skill paths, prompt construction, diagnostics, and skill activation recovery from session history.
- [x] (2026-05-07 18:35Z) Confirmed `src/prompts/mod.rs` emits the available skill catalog and has snapshot coverage for skills in prompt context.
- [x] (2026-05-07 18:35Z) Confirmed documentation exists in `docs/design-docs/skills.md`, `docs/adr/002-agent-skills.md`, and related settings, prompt, and sandbox docs.
- [x] (2026-05-07 18:35Z) Migrated this completed plan to `.agents/exec-plans/completed/agent-skills-implementation-plan.md` and added the required ExecPlan lifecycle sections.

## Surprises & Discoveries

- Observation: The implemented feature includes configured skill search paths in addition to project and user skill directories.
  Evidence: `src/config/skills.rs::discover_skills_with_paths` scans project paths first, configured paths second, and user paths last; `docs/design-docs/skills.md` documents `skills.path`.

- Observation: Activated skills are reconstructed from prior Read tool calls rather than persisted as a separate session field.
  Evidence: `src/main.rs::extract_activated_skills` scans conversation history and `src/clients/agent.rs` seeds `activated_skills` through `with_activated_skills`.

## Decision Log

- Decision: Classify this plan as completed during the ExecPlan migration.
  Rationale: The current repository contains implementation, CLI flags, prompt integration, docs, and tests corresponding to the plan's success criteria.
  Date/Author: 2026-05-07 / Codex

## Outcomes & Retrospective

The agent skills feature is implemented and documented. Skills can be discovered from `.agents/skills/`, configured paths, and user-level skill directories; they are listed in prompt context and lazy-loaded through the existing Read tool. The final implementation differs from the original note by reconstructing activated skills from conversation history instead of adding a standalone session field, which keeps the persisted session schema simpler while preserving deduplication after resume.

## Project Context

**cake** is a Rust CLI AI coding assistant that:
- Integrates with LLMs via OpenRouter API (Responses API and Chat Completions API)
- Executes tools (Bash, Read, Edit, Write) in a sandboxed environment
- Manages conversation sessions with continue/resume/fork capabilities
- Uses OS-level sandboxing (macOS Seatbelt, Linux Landlock)

### Relevant Architecture

The project follows a 4-layer architecture:
- **Layer 4 (CLI)**: `src/cli/` - argument parsing, command dispatch
- **Layer 3 (Clients)**: `src/clients/` - Agent loop, tool execution, API backends
- **Layer 2 (Config/Models/Prompts)**: `src/config/`, `src/models/`, `src/prompts/` - data persistence, types, prompts
- **Layer 1 (Foundation)**: External crates, logging

**Key invariants**:
- Dependencies flow downward only (no circular imports)
- All internal imports use absolute paths (`crate::module::Item`)
- ConversationItem is the single source of truth for conversation state
- Path validation before tool execution

### Existing Patterns to Follow

1. **AGENTS.md loading** (`src/config/data_dir.rs`):
   - `DataDir::read_agents_files()` reads from user-level and project-level
   - Returns `Vec<AgentsFile>` with path and content

2. **System prompt building** (`src/prompts/mod.rs`):
   - `build_system_prompt()` receives AGENTS.md files
   - Wraps content in `<instructions>` tags

3. **Tool registration** (`src/clients/agent.rs`):
   - Tools registered in `Agent::new()`: `bash_tool()`, `edit_tool()`, `read_tool()`, `write_tool()`
   - Tool execution via `execute_tool()` in `src/clients/tools/mod.rs`

4. **Settings loading** (`src/config/settings.rs`):
   - `SettingsLoader` loads from project and user-level TOML files
   - Merge semantics: project settings override user settings

---

## Implementation Plan

### Phase 1: Skill Discovery and Parsing

**Goal**: Discover skills from filesystem and parse SKILL.md files.

#### 1.1 Create Skill Module

**New file**: `src/config/skills.rs`

```rust
use std::collections::HashSet;

// Core types
pub struct Skill {
    pub name: String,
    pub description: String,
    pub location: PathBuf,      // Absolute path to SKILL.md
    pub base_directory: PathBuf, // Parent of SKILL.md
    pub scope: SkillScope,
}

pub enum SkillScope {
    Project,
    User,
}

pub struct SkillDiagnostic {
    pub level: DiagnosticLevel,
    pub message: String,
    pub file: PathBuf,
}

pub enum DiagnosticLevel {
    Warning,
    Error,
}

pub struct SkillCatalog {
    pub skills: Vec<Skill>,
    pub diagnostics: Vec<SkillDiagnostic>,
    /// Set of skill names that have been activated in this session
    pub activated_skills: HashSet<String>,
}

impl SkillCatalog {
    /// Check if a path corresponds to a known skill location
    pub fn get_skill_by_location(&self, path: &Path) -> Option<&Skill> {
        self.skills.iter().find(|s| s.location == path)
    }
    
    /// Mark a skill as activated (returns true if newly activated)
    pub fn mark_activated(&mut self, skill_name: &str) -> bool {
        self.activated_skills.insert(skill_name.to_string())
    }
    
    /// Check if a skill has already been activated
    pub fn is_activated(&self, skill_name: &str) -> bool {
        self.activated_skills.contains(skill_name)
    }
}
```

**Tasks**:
- [ ] Create `src/config/skills.rs` with types above
- [ ] Export from `src/config/mod.rs`
- [ ] Add unit tests for types
- [ ] Implement `SkillCatalog::get_skill_by_location()`, `mark_activated()`, `is_activated()`

#### 1.2 Implement Skill Discovery

**Extend**: `src/config/data_dir.rs`

Add method similar to `read_agents_files()`:

```rust
impl DataDir {
    /// Discover skills from project-level and user-level directories.
    ///
    /// Scan paths (in order):
    /// - `<project>/.agents/skills/`
    /// - `~/.agents/skills/`
    pub fn discover_skills(&self, working_dir: &Path) -> SkillCatalog {
        // Implementation
    }
}
```

**Scan logic**:
- [ ] Iterate through scan paths in precedence order (project > user)
- [ ] For each path, find subdirectories containing `SKILL.md`
- [ ] Skip excluded directories (`.git/`, `node_modules/`, `target/`)
- [ ] Max depth: 4 levels, max directories: 2000
- [ ] Handle name collisions: first found wins within scope, project overrides user
- [ ] Log warnings for collisions

#### 1.3 Implement SKILL.md Parser

**In**: `src/config/skills.rs`

```rust
impl Skill {
    /// Parse a SKILL.md file and extract metadata.
    pub fn parse(path: &Path, scope: SkillScope) -> Result<Self, SkillDiagnostic> {
        // 1. Read file content
        // 2. Find YAML frontmatter between --- delimiters
        // 3. Parse YAML for name and description (required)
        // 4. Extract body (markdown after frontmatter)
        // 5. Handle malformed YAML with fallbacks
    }
}
```

**Parsing rules**:
- [ ] Extract YAML between opening `---` and closing `---`
- [ ] Required fields: `name`, `description`
- [ ] Use `serde_yaml` for parsing with fallback for malformed YAML (unquoted colons in values)
- [ ] Lenient validation: warn on name mismatch, skip on missing description
- [ ] Strip frontmatter from body content
- [ ] Body is lazy-loaded (not stored at discovery time)

**Dependencies**:
- Add `serde_yaml` crate to `Cargo.toml` for YAML frontmatter parsing

---

### Phase 2: Skill Configuration and Catalog Disclosure

**Goal**: Configure skill loading via CLI/settings and include skill catalog in system prompt.

#### 2.0 Skill Configuration (CLI and Settings)

**Goal**: Allow users to disable skills or filter to specific skills via CLI flags and settings.toml.

**CLI Flags** (`src/main.rs`):

```rust
/// AI coding assistant CLI
#[derive(Parser)]
struct CodingAssistant {
    // ... existing flags ...
    
    /// Disable all skills for this session
    #[arg(long)]
    pub no_skills: bool,
    
    /// Only load specific skills (comma-separated list of skill names)
    #[arg(long, value_name = "NAMES")]
    pub skills: Option<String>,
}
```

**Settings.toml** (`~/.cache/cake/settings.toml` or `.cake/settings.toml`):

```toml
# Global skill settings
[skills]
# Set to true to disable all skills
disabled = false
# Optional: only load these named skills (comma-separated or array)
only = ["debugging-cake", "code-review"]
```

**Settings Schema** (`src/config/settings.rs`):

```rust
#[derive(Debug, Clone, Deserialize, Default)]
pub struct SkillSettings {
    /// If true, disable all skills
    #[serde(default)]
    pub disabled: bool,
    /// Optional list of skill names to load (empty = all)
    #[serde(default)]
    pub only: Vec<String>,
}

impl SettingsLoader {
    /// Load skill settings from settings.toml
    pub fn load_skill_settings(&self) -> SkillSettings {
        // Implementation
    }
}
```

**Merge Logic** (`src/main.rs`):

```rust
/// Resolve effective skill configuration from CLI and settings
fn resolve_skill_config(
    no_skills: bool,
    skills_flag: Option<&str>,
    settings: &SkillSettings,
) -> SkillConfig {
    // CLI --no-skills takes highest precedence
    if no_skills {
        return SkillConfig::Disabled;
    }
    
    // CLI --skills flag filters to named skills
    if let Some(names) = skills_flag {
        let skill_names: Vec<String> = names
            .split(',')
            .map(|s| s.trim().to_string())
            .collect();
        return SkillConfig::Only(skill_names);
    }
    
    // Settings.toml skills.only filters to named skills
    if !settings.only.is_empty() {
        return SkillConfig::Only(settings.only.clone());
    }
    
    // Settings.toml skills.disabled = true
    if settings.disabled {
        return SkillConfig::Disabled;
    }
    
    // Default: load all discovered skills
    SkillConfig::All
}

enum SkillConfig {
    All,           // Load all discovered skills
    Disabled,      // Don't load any skills
    Only(Vec<String>), // Load only these named skills
}
```

**Precedence** (highest to lowest):
1. `--no-skills` CLI flag → disables all
2. `--skills name1,name2` CLI flag → filter to named skills
3. `skills.only` in settings → filter to named skills
4. `skills.disabled = true` in settings → disable all
5. Default → load all discovered skills

**Apply Filtering** (`src/config/skills.rs`):

```rust
impl SkillCatalog {
    /// Filter catalog to only include specified skills
    pub fn filter_to(&mut self, skill_names: &[String]) {
        self.skills.retain(|s| skill_names.contains(&s.name));
    }
    
    /// Check if skills are disabled (returns empty catalog)
    pub fn disabled() -> Self {
        Self {
            skills: vec![],
            diagnostics: vec![],
            activated_skills: HashSet::new(),
        }
    }
}
```

**Tasks**:
- [ ] Add `--no-skills` and `--skills` flags to CLI struct in `src/main.rs`
- [ ] Add `SkillSettings` struct to `src/config/settings.rs`
- [ ] Add `skills` section parsing to `SettingsLoader`
- [ ] Implement `resolve_skill_config()` function in `src/main.rs`
- [ ] Add `filter_to()` and `disabled()` methods to `SkillCatalog`
- [ ] Apply configuration after skill discovery
- [ ] Add unit tests for configuration parsing and precedence
- [ ] Add integration tests for flag behavior

#### 2.1 Build Skill Catalog

**In**: `src/config/skills.rs`

```rust
impl SkillCatalog {
    /// Generate XML catalog for system prompt.
    pub fn to_prompt_xml(&self) -> String {
        // <available_skills>
        //   <skill>
        //     <name>debugging-cake</name>
        //     <description>How to investigate and debug...</description>
        //     <location>/path/to/SKILL.md</location>
        //   </skill>
        // </available_skills>
    }
}
```

**Tasks**:
- [ ] Implement `to_prompt_xml()` method
- [ ] Filter out skills with error-level diagnostics
- [ ] Return empty string if no valid skills

#### 2.2 Extend System Prompt

**Modify**: `src/prompts/mod.rs`

```rust
pub fn build_system_prompt(
    _working_dir: &Path,
    agents_files: &[AgentsFile],
    skill_catalog: &SkillCatalog,  // NEW
) -> String {
    let mut prompt = String::from(
        "You are cake. You are running as a coding agent in a CLI on the user's computer."
    );

    // ... existing AGENTS.md context ...

    // Add skill catalog section
    if !skill_catalog.skills.is_empty() {
        prompt.push_str("\n\n## Skills\n\n");
        prompt.push_str(SKILL_USAGE_INSTRUCTIONS);
        prompt.push_str(&skill_catalog.to_prompt_xml());
        prompt.push_str("\n\n");
    }

    prompt
}

const SKILL_USAGE_INSTRUCTIONS: &str = indoc::indoc! {"
    <skill_instructions>
    The following skills provide specialized instructions for specific tasks.
    When a task matches a skill's description, use your file-read tool to load
    the SKILL.md at the listed location before proceeding.
    When a skill references relative paths, resolve them against the skill's
    directory (the parent of SKILL.md) and use absolute paths in tool calls.
    </skill_instructions>
"};
```

**Tasks**:
- [ ] Add `skill_catalog` parameter to `build_system_prompt()`
- [ ] Add skill catalog section to prompt
- [ ] Add behavioral instructions for skill usage
- [ ] Update tests

#### 2.3 Update Call Sites

**Modify**: `src/main.rs`

In `CodingAssistant::build_client_and_session()`:
```rust
let skill_catalog = data_dir.discover_skills(&current_dir);
let system_prompt = build_system_prompt(&current_dir, &agents_files, &skill_catalog);
```

**Tasks**:
- [ ] Call `discover_skills()` in main.rs
- [ ] Pass catalog to `build_system_prompt()`
- [ ] Log diagnostics (warnings/errors) to log file

---

### Phase 3: Skill Activation (File-Read)

**Goal**: Enable model to activate skills via existing Read tool.

#### 3.1 Verify Read Tool Works with SKILL.md

The existing `Read` tool (`src/clients/tools/read.rs`) should work without modification:
- Model sees `location` path in catalog
- Model calls `Read` with the SKILL.md path
- Read tool returns content (including frontmatter)

**Tasks**:
- [ ] Test that existing Read tool works with skill paths
- [ ] Verify path validation allows skill directories

#### 3.2 Allowlist Skill Directories

**Modify**: `src/clients/tools/mod.rs` or `src/clients/tools/read.rs`

Current path validation (`validate_path_in_cwd`) only allows:
- Current working directory
- Temp directories

**Need to add**:
- User-level skill directories: `~/.agents/skills/`
- Project-level skill directories (already in CWD)

**Tasks**:
- [ ] Extend `validate_path_in_cwd()` to allow known skill directories
- [ ] Or add skill directory paths to allowed paths
- [ ] Add tests for reading from skill directories

#### 3.3 Deduplicate Skill Activations

**Problem**: When the model reads a SKILL.md file, the content is added to conversation history. If the same skill is read again (or the session is resumed), the instructions appear multiple times, wasting tokens and potentially confusing the model.

**Solution**: Track activated skills in the session and return a lightweight message instead of re-reading the file.

**Modify**: `src/config/session.rs`

```rust
use std::collections::HashSet;

pub struct Session {
    // ... existing fields ...
    /// Names of skills that have been activated in this session
    pub activated_skills: HashSet<String>,
}
```

**Modify**: `src/clients/tools/read.rs`

Pass skill catalog to the Read tool execution so it can check if a path is a skill:

```rust
pub fn execute_read_with_skills(
    args: &str,
    skill_catalog: &SkillCatalog,
) -> Result<ToolResult, String> {
    // Parse path from args
    let path = parse_path(args)?;
    
    // Check if this path is a skill location
    if let Some(skill) = skill_catalog.get_skill_by_location(&path) {
        if skill_catalog.is_activated(&skill.name) {
            // Already activated - return lightweight message
            return Ok(ToolResult {
                output: format!(
                    "Skill '{}' is already active in this session. \
                     Its instructions are already in the conversation context.",
                    skill.name
                ),
            });
        }
        // Mark as activated and proceed with read
        // Note: need mutable access to catalog for marking
    }
    
    // Normal read logic...
}
```

**Architecture challenge**: The tool execution is stateless - it receives arguments and returns output. We need to share the activated_skills state between:
1. The Session (for persistence)
2. The Agent (for passing to tool execution)
3. The tools (for checking activation)

**Approach**: Use `Arc<Mutex<HashSet<String>>>` for shared state:

```rust
// In main.rs
let activated_skills = Arc::new(Mutex::new(HashSet::new()));

// Pass to Agent
let agent = Agent::new(resolved, &system_prompt)
    .with_activated_skills(activated_skills.clone());

// Pass to Session for persistence
session.activated_skills = activated_skills;
```

**Tasks**:
- [ ] Add `activated_skills: HashSet<String>` to `Session` struct
- [ ] Add `activated_skills: Arc<Mutex<HashSet<String>>>` to `Agent` struct
- [ ] Initialize activated_skills from session when resuming/continuing
- [ ] Modify `execute_read()` to check skill activation status
- [ ] Return "already activated" message instead of re-reading
- [ ] Save activated_skills to session on save
- [ ] Add tests for deduplication logic

---

## File Changes Summary

### New Files
- `src/config/skills.rs` - Skill types, parser, catalog

### Modified Files
- `src/config/mod.rs` - Export skills module
- `src/config/data_dir.rs` - Add `discover_skills()` method
- `src/config/session.rs` - Add `activated_skills` field for persistence
- `src/config/settings.rs` - Add `SkillSettings` struct and parsing
- `src/prompts/mod.rs` - Add skill catalog to system prompt
- `src/main.rs` - Add `--no-skills` and `--skills` flags, call skill discovery, pass catalog to prompt builder, manage activated_skills state
- `src/clients/tools/mod.rs` - Allowlist skill directories, pass skill catalog to read tool
- `src/clients/tools/read.rs` - Check skill activation status before reading
- `src/clients/agent.rs` - Add `activated_skills` state, pass to tool execution
- `Cargo.toml` - Add `serde_yaml` dependency

---

## Dependencies

### New Dependencies
- `serde_yaml` - For parsing YAML frontmatter

### Existing Dependencies Used
- `serde`, `serde_json` - Serialization
- `anyhow`, `thiserror` - Error handling
- `log` - Logging diagnostics

---

## Testing Strategy

### Unit Tests
- [ ] Skill parsing (valid, malformed, missing fields)
- [ ] Skill discovery (collision resolution, filtering)
- [ ] Catalog generation (XML format, filtering)
- [ ] Skill configuration parsing (CLI flags, settings.toml)
- [ ] Configuration precedence (CLI overrides settings)

### Integration Tests
- [ ] Full discovery-to-activation flow
- [ ] Reading SKILL.md via Read tool
- [ ] Multiple scopes (project + user)
- [ ] Skill activation deduplication (same skill read twice)
- [ ] Session resume preserves activated skills (no re-reading)
- [ ] `--no-skills` flag disables all skills
- [ ] `--skills` flag filters to named skills
- [ ] Settings.toml `skills.disabled` disables skills
- [ ] Settings.toml `skills.only` filters skills

### Test Fixtures
Create test skill directories in `tests/fixtures/skills/`:
- `valid-skill/SKILL.md` - Valid skill
- `malformed-yaml/SKILL.md` - Invalid YAML
- `missing-description/SKILL.md` - Missing required field
- `name-mismatch/SKILL.md` - Name doesn't match directory

---

## Implementation Order

### Sprint 1: Core Discovery
1. Create `src/config/skills.rs` with types
2. Implement SKILL.md parser
3. Implement `DataDir::discover_skills()`
4. Unit tests for parsing and discovery

### Sprint 2: Configuration & Prompt Integration
5. Add `--no-skills` and `--skills` CLI flags
6. Add `SkillSettings` to settings module
7. Implement configuration precedence logic
8. Implement `SkillCatalog::to_prompt_xml()`
9. Modify `build_system_prompt()` to include catalog
10. Update `main.rs` call sites
11. Test prompt generation and configuration

### Sprint 3: Activation & Polish
12. Verify Read tool works with skill paths
13. Allowlist skill directories
14. Implement activation deduplication (Session + Agent state)
15. Add diagnostic logging
16. Test skill activation
17. Test deduplication (same session and resume)
18. Update documentation

---

## Design Decisions

1. **YAML Parser**: Use `serde_yaml` crate with fallback handling for malformed YAML
2. **Body Loading**: Lazy loading - body is read at activation time, not stored at discovery
3. **Activation Mechanism**: File-read only - model uses existing Read tool to load SKILL.md
4. **Trust System**: Deferred to future security hardening

---

## Existing Skill Example

The project already has a skill at `.agents/skills/debugging-cake/SKILL.md`:

```yaml
---
name: debugging-cake
description: |
  How to investigate and debug issues with the cake CLI tool. Use this skill whenever:
  - The user reports the CLI returned "None" or an empty response
  - The user mentions truncated, incomplete, or cut-off responses
  ...
---

# Debugging cake CLI
...
```

This should be discovered and included in the catalog once implemented.

---

## Success Criteria

- [ ] Skills discovered from `.agents/skills/` (project and user level)
- [ ] SKILL.md files parsed with frontmatter extraction using `serde_yaml`
- [ ] Skill catalog appears in system prompt
- [ ] Model can activate skills via Read tool
- [ ] Skills are not re-loaded when already activated in the same session
- [ ] Activated skills persist across session resume/continue (no re-reading)
- [ ] `--no-skills` flag disables all skills
- [ ] `--skills name1,name2` flag filters to named skills
- [ ] Settings.toml `skills.disabled` disables skills by default
- [ ] Settings.toml `skills.only` filters to named skills
- [ ] CLI flags override settings.toml configuration
- [ ] Diagnostics logged for malformed skills
- [ ] Existing `debugging-cake` skill appears in catalog

## Revision Notes

- 2026-05-07 / Codex: Migrated this historical plan into the new completed ExecPlan directory and added lifecycle sections required by `.agents/PLANS.md`. The original implementation detail above was retained for context and auditability.
