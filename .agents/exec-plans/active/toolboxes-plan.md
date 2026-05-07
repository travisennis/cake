# Toolboxes Feature Plan for Cake

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `.agents/PLANS.md` from the repository root. It was migrated from the former `.agents/.plans/` location and remains active because the current codebase does not contain toolbox discovery, `CAKE_TOOLBOX`, `tb__` tool registration, or toolbox CLI commands.

## Purpose / Big Picture

Cake should support user-defined executable tools in addition to built-in Bash, Read, Edit, and Write. After this work, a user can place executable tool scripts in configured toolbox directories, run cake, and have those tools discovered, described, exposed to the model with a `tb__` prefix, and invoked through the same tool execution path as built-in tools.

The behavior will be observable by creating a tiny executable toolbox script, running a toolbox listing or direct invocation command if implemented, and seeing the model receive and execute the corresponding `tb__<name>` tool.

## Progress

- [x] (2026-05-07 18:43Z) Confirmed the current codebase has no `toolbox`, `CAKE_TOOLBOX`, `tb__`, or `TOOLBOX_ACTION` implementation.
- [x] (2026-05-07 18:43Z) Migrated this plan to `.agents/exec-plans/active/toolboxes-plan.md` and added the required ExecPlan lifecycle sections.
- [ ] Implement toolbox discovery and parsing.
- [ ] Register discovered toolbox tools with the agent and dispatch `tb__*` calls.
- [ ] Add CLI affordances, tests, documentation, and full CI validation.

## Surprises & Discoveries

- Observation: Toolbox support appears to be unimplemented in the current tree.
  Evidence: `rg -n "toolbox|CAKE_TOOLBOX|tb__|TOOLBOX_ACTION" src docs README.md Cargo.toml` produced no matches.

## Decision Log

- Decision: Classify this plan as active during the ExecPlan migration.
  Rationale: The plan describes a feature that is not present in the current implementation and still contains actionable design detail.
  Date/Author: 2026-05-07 / Codex

## Outcomes & Retrospective

No implementation has started. At completion, update this section with the final protocol decisions, the test commands run, user-facing docs added, and any deviations from the Amp-inspired design retained below.

## What Amp Toolboxes Are

Toolboxes are a mechanism for users to extend the agent's tool set with custom, user-defined tools written in any language. Each tool is an executable file that communicates with the agent over a simple stdin/stdout protocol. The key properties:

1. **Discovery**: Directories containing executable files are scanned at startup. Amp uses an environment variable (`AMP_TOOLBOX`) with `PATH`-like colon-separated syntax. Default directory is `~/.config/amp/tools`. Earlier directories take precedence for name conflicts.

2. **Protocol**: Each executable implements two actions, determined by the `TOOLBOX_ACTION` environment variable:
   - `describe`: The executable outputs its name, description, and parameter schema (to stdout).
   - `execute`: The executable receives arguments on stdin and writes its output to stdout. Exit code 0 = success, non-zero = error.

3. **Communication formats**: Tools can use either JSON or text format. The agent auto-detects by attempting JSON parse first, then falling back to text. The detected format is remembered and used for both describe and execute.
   - **JSON format**: `{"name": "...", "description": "...", "args": {...}}` (compact) or `{"name": "...", "description": "...", "inputSchema": {...}}` (full JSON Schema draft 2020-12). On execute, stdin receives a JSON object of arguments.
   - **Text format**: Line-based `name: ...`, `description: ...`, then `param: type description` lines. Multiple `description:` lines are concatenated with newlines. On execute, stdin receives `key=value\n` pairs.

4. **Naming**: Toolbox tools are registered with a `tb__` prefix to avoid collisions with built-in tools (e.g., `run_tests` becomes `tb__run_tests`). The `name` field from the describe output is authoritative (not the filename).

5. **Environment variables**: Tools receive `TOOLBOX_ACTION`, `AGENT` (set to the agent name), and during execute: `AMP_THREAD_ID` and `AGENT_THREAD_ID` (session ID).

6. **CLI commands** (in Amp):
   - `amp tools list`: List all tools (built-in, MCP, toolbox).
   - `amp tools make [--bash|--zsh] <name>`: Scaffold a new tool.
   - `amp tools show <name>`: Show a tool's schema.
   - `amp tools use <name> [--args...]`: Invoke a tool directly from the CLI.

---

## How This Maps to Cake's Architecture

### Current Tool System

Cake has four hardcoded tools: `Bash`, `Read`, `Edit`, `Write`. They are:
- Defined as `Tool` structs (name, description, JSON schema) in `src/clients/tools/mod.rs`.
- Registered in `Agent::new()` via `tools: vec![bash_tool(), edit_tool(), read_tool(), write_tool()]`.
- Dispatched by name in `execute_tool()` via a `match` statement.
- Results are `ToolResult { output: String }`.

### Integration Points

The toolbox feature touches these layers:

| Layer | Module | Change |
|-------|--------|--------|
| Layer 2 (Config) | `config/` | New `toolbox` module for discovery and protocol |
| Layer 3 (Clients) | `clients/tools/mod.rs` | Add `Tool::new()` constructor, extend dispatch |
| Layer 3 (Clients) | `clients/tools/toolbox.rs` | Toolbox execute logic, registry |
| Layer 3 (Clients) | `clients/agent.rs` | Make `execute_tool` a method, store toolbox registry |
| Layer 4 (CLI) | `main.rs` | Discovery at startup, new `--toolbox` flag, subcommands |

---

## Implementation Plan

### Phase 0: Prerequisites

**Add `Tool::new()` constructor**: The `Tool` struct fields are `pub(super)`, so code outside `src/clients/tools/` cannot construct instances. Add a public constructor:

```rust
impl Tool {
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: serde_json::Value) -> Self {
        Self {
            type_: "function".to_string(),
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}
```

**Refactor `execute_tool` to a method on `Agent`**: Currently `execute_tool` is a free function with no access to agent state. It needs access to the toolbox registry to dispatch `tb__*` calls. Move it to an `impl Agent` method. Note: the agent loop holds `&mut self` while spawning concurrent tool futures via `join_all`. To avoid borrow-checker conflicts, extract a reference to the toolbox registry (e.g., `let registry = &self.toolbox_registry;`) before the futures block, then pass it into each future.

### Phase 1: Discovery

**New module**: `src/config/toolbox.rs`

**Environment variable**: `CAKE_TOOLBOX` (following cake's `CAKE_` prefix convention).

**Default directory**: `~/.config/cake/tools` (under cake's config directory).

**Discovery logic**:
1. If `CAKE_TOOLBOX` is set and non-empty, split on `:` and scan those directories.
2. If `CAKE_TOOLBOX` is unset, scan the default directory (`~/.config/cake/tools`).
3. If `CAKE_TOOLBOX` is set to empty string, skip toolbox scanning entirely.
4. For each directory, enumerate files, applying these filters:
   - **Skip** hidden files (dot-prefix).
   - **Skip** files with `.md` or `.txt` extensions.
   - **Skip** non-executable files (check execute bit via `std::os::unix::fs::PermissionsExt`).
   - **Skip** directories.
5. Earlier directories take precedence for name conflicts.

> **Note on Amp compatibility**: The Amp documentation is ambiguous on the unset case (one sentence says it uses the default directory, the next says no scanning). Our behavior (scan default directory when unset) is the more user-friendly interpretation and likely matches Amp's actual behavior.

**Output**: A `Vec<ToolboxEntry>` where each entry contains:
```rust
struct ToolboxEntry {
    /// Name as discovered (filename)
    filename: String,
    /// Full path to the executable
    path: PathBuf,
    /// Source directory (for display/debugging)
    source_dir: PathBuf,
}
```

### Phase 2: Describe Protocol

**New module**: `src/config/toolbox.rs` (continued) or `src/clients/tools/toolbox.rs`

For each discovered `ToolboxEntry`, run the executable with `TOOLBOX_ACTION=describe` and `AGENT=cake`, capture stdout, and parse the schema.

**Format detection**:
1. Attempt JSON parse of stdout.
2. If JSON fails, parse as text format.
3. Store the detected format alongside the tool definition.

**JSON format parsing**:
- Support both compact `args` format and full `inputSchema` (JSON Schema draft 2020-12).
- `args` format: `{"param": ["type", "description"]}` is converted internally to a JSON Schema object.
- `inputSchema` format: used as-is for tools needing nested objects, arrays, and `required` fields.
- The `name` field from the JSON output is the authoritative tool name.

**Text format parsing**:
- Multiple `description:` lines are concatenated with newlines.
- Parameter lines must include an explicit type (no defaulting to string).
- Optional parameters are marked with a `?` suffix on the type: `param: string? description`.
- Empty lines are ignored.

**Error handling**: If a toolbox executable fails during `describe` (non-zero exit, invalid output, timeout), **skip it with a warning log** and continue startup with the remaining tools. One broken tool should not block the session.

**Data types**:
```rust
enum ToolboxFormat {
    Json,
    Text,
}

struct ToolboxTool {
    /// Registered name with prefix: tb__<name>
    registered_name: String,
    /// Original name from the describe output (authoritative, not the filename)
    original_name: String,
    /// Path to the executable
    path: PathBuf,
    /// Description from describe action
    description: String,
    /// Parameter schema as JSON Schema (converted from either format)
    parameters: serde_json::Value,
    /// Communication format detected during describe
    format: ToolboxFormat,
}
```

The `ToolboxTool` can be converted to a `Tool` struct (via `Tool::new()`) for registration with the API.

### Phase 3: Execute Protocol

**In `Agent`**: `execute_tool` is now a method on `Agent`. Add a fallback branch for names starting with `tb__`.

```rust
// In execute_tool match:
name if name.starts_with("tb__") => {
    self.execute_toolbox_tool(name, arguments).await
}
```

**Execution logic** (`execute_toolbox_tool`):
1. Look up the `ToolboxTool` by registered name in the toolbox registry.
2. Spawn the executable with:
   - `TOOLBOX_ACTION=execute`
   - `AGENT=cake`
   - `CAKE_THREAD_ID=<session_id>`
   - `AGENT_THREAD_ID=<session_id>`
3. Write arguments to stdin:
   - JSON format: write the arguments JSON object directly.
   - Text format: convert JSON args to `key=value\n` pairs (using `=` as the separator).
4. Capture stdout as the tool output.
5. Check exit code: 0 = success, non-zero = error.
6. Return `ToolResult { output }`.

**Timeout**: Apply the same timeout as the Bash tool (configurable, default 60s).

**Concurrency**: No limit on concurrent toolbox processes. In practice, models rarely request more than 2-3 tool calls per turn, and the OS handles a handful of child processes trivially.

**Sandboxing consideration**: Toolbox tools run as separate processes. They are NOT sandboxed by cake's Seatbelt/Landlock profiles (unlike Bash). This is intentional: toolbox tools are user-provided and trusted. Document this clearly.

### Phase 4: Agent Integration

**In `Agent`**: Store a `ToolboxRegistry` (e.g., `HashMap<String, ToolboxTool>`) as a field on the agent. Accept toolbox tools in the constructor.

```rust
pub fn new(config: ResolvedModelConfig, system_prompt: &str, toolbox_tools: Vec<ToolboxTool>) -> Self {
    let mut tools = vec![bash_tool(), edit_tool(), read_tool(), write_tool()];
    let mut toolbox_registry = HashMap::new();
    for tb_tool in toolbox_tools {
        tools.push(Tool::new(&tb_tool.registered_name, &tb_tool.description, tb_tool.parameters.clone()));
        toolbox_registry.insert(tb_tool.registered_name.clone(), tb_tool);
    }
    Self {
        tools,
        toolbox_registry,
        // ...
    }
}
```

**Borrow-checker note**: In the agent loop, extract `let registry = &self.toolbox_registry;` before the `join_all` futures block so the closures can reference it without conflicting with `&mut self`.

### Phase 5: CLI Integration

**Startup** (in `main.rs`):
1. After parsing args, discover toolbox entries.
2. Run `describe` on each, build `ToolboxTool` structs (skipping failures with warnings).
3. Pass the `Vec<ToolboxTool>` to `Agent::new()`.

**New CLI flag**: `--toolbox <DIR>` to add extra toolbox directories (appended to `CAKE_TOOLBOX` dirs).

**New subcommand** (optional, lower priority): `cake tools` with subcommands:
- `cake tools list`: Show all tools (built-in + toolbox) with source labels.
- `cake tools show <name>`: Display a tool's schema.
- `cake tools use <name> [--arg key=value...]`: Invoke a tool directly.
- `cake tools make [--bash|--python|--node] <name>`: Scaffold a new tool in the default directory. Defaults to bash.

### Phase 6: Summarization and Progress

**In `summarize_tool_args()`**: Add a branch for `tb__*` tools that shows the tool name and a truncated JSON dump of arguments.

**In progress/spinner display**: Toolbox tool calls should display like built-in tools (e.g., `tb__run_tests: running...`).

---

## Design Decisions

### Prefix: `tb__`
Matches Amp's convention. Prevents collisions with built-in tool names. The model sees these as distinct tools.

### Environment variable naming
`CAKE_TOOLBOX` follows the project's `CAKE_` prefix convention (like `CAKE_DATA_DIR`, `CAKE_SANDBOX`).

### Default directory
`~/.config/cake/tools` keeps everything under cake's config directory. Project-level tools can go in `.cake/tools/`.

### No sandbox for toolbox tools
Toolbox tools are user-authored executables. Sandboxing them would be restrictive and complex. The user takes responsibility for what their tools do.

### Format compatibility
Support both JSON and text formats for Amp compatibility. Users can write tools that work with both Amp and cake. Both `args` (compact) and `inputSchema` (full JSON Schema) are supported in JSON format.

### Tool naming: describe output is authoritative
The `name` field from the describe output determines the registered tool name, not the filename. The filename is only used for discovery.

### Optional parameter syntax
Only the type suffix form (`string?`) is supported for marking parameters optional. This is the simplest to parse and the most visually clear.

### Text format: explicit types required
Parameter lines in text format must include an explicit type. No implicit defaulting to string.

### Text format execute input separator
Uses `=` as the key-value separator (`key=value\n`), matching the Amp spec text.

### Session ID exposure
Pass `session_id` as both `CAKE_THREAD_ID` and `AGENT_THREAD_ID` so tools can correlate with sessions. The dual naming provides compatibility with Amp (`AGENT_THREAD_ID`) while following cake's naming convention (`CAKE_THREAD_ID`).

### Describe failure handling
Failures during describe are logged as warnings and the tool is skipped. One broken tool does not block startup.

### execute_tool as an Agent method
Moved from a free function to a method on `Agent` to provide access to the toolbox registry. This is the idiomatic Rust approach and avoids globals or thread-locals.

### Tool::new() constructor
Added to `Tool` to allow construction from outside `src/clients/tools/` without exposing struct fields. Fields remain `pub(super)` for existing internal use.

### Scaffolding defaults
`cake tools make` defaults to bash scaffolding, with `--python` and `--node` options available.

---

## Module Placement

Following cake's layering rules (dependencies flow downward only):

```
Layer 4 (CLI)
  main.rs              - startup discovery, --toolbox flag, tools subcommand
  
Layer 3 (Clients)  
  clients/tools/mod.rs - Tool::new() constructor, execute_tool dispatch for tb__* tools
  clients/tools/toolbox.rs - execute_toolbox_tool(), ToolboxTool registry
  clients/agent.rs     - accept toolbox tools in constructor, store registry

Layer 2 (Config)
  config/toolbox.rs    - discovery (scan dirs, find executables, filter)
                       - describe protocol (run executable, parse output)
                       - ToolboxEntry, ToolboxTool, ToolboxFormat types
```

Discovery and describe parsing belong in Layer 2 (config) because they deal with filesystem scanning and configuration. Execution belongs in Layer 3 (clients/tools) because it's tool execution logic alongside the existing tools.

---

## Implementation Order

1. Phase 0: Add `Tool::new()` constructor, refactor `execute_tool` to Agent method
2. `config/toolbox.rs`: Discovery + describe protocol + types
3. `clients/tools/toolbox.rs`: Execute protocol
4. `clients/tools/mod.rs`: Extend `execute_tool()` dispatch
5. `clients/agent.rs`: Accept toolbox tools in constructor, store registry
6. `main.rs`: Wire up discovery at startup
7. Tests for each layer
8. CLI subcommands (lower priority, can be a follow-up)

---

## Testing Strategy

### Unit Tests
- **Text format parsing**: Single and multiple `description:` lines, parameters with explicit types, optional markers (`string?`), empty lines, edge cases (special characters in descriptions, parameter names with underscores).
- **JSON format parsing**: Compact `args` format, full `inputSchema` format, missing fields, invalid JSON.
- **`args` to JSON Schema conversion**: Verify the compact format correctly converts to a valid JSON Schema object.
- **Name extraction**: Verify the `name` field from describe output is used, not the filename.
- **File filtering**: Hidden files skipped, `.md`/`.txt` skipped, non-executable skipped, directories skipped.
- **Discovery precedence**: Earlier directories win for name conflicts.

### Integration Tests
- **Fixture executables**: Bash scripts in a temp directory that implement the describe/execute protocol. Test the full discover → describe → execute cycle.
- **Both formats**: Fixture tools using JSON format and text format.
- **Error cases**: Non-zero exit on describe, invalid output, timeout, missing name field.

### Property-Based Tests (using `proptest`)
- **Text format parsing roundtrip**: Generate arbitrary tool names, descriptions, and parameter lists → serialize to text format → parse → verify all fields preserved.
- **`key=value` serialization roundtrip**: Generate random `HashMap<String, String>` → serialize to `key=value\n` → deserialize → verify equality. Surfaces edge cases like values containing `=`, empty values, unicode.
- **JSON `args` → `inputSchema` conversion**: Generate arbitrary `args` maps → convert to JSON Schema → verify the schema validates the expected inputs.

---

## Resolved Questions

1. **Project-level tools (`.cake/tools/`)**: Only scanned when explicitly listed in `CAKE_TOOLBOX`. No automatic scanning of project-level directories. This avoids the security risk of cloned repos injecting tools.

2. **Timeout configuration**: Per-tool timeout, specified in the describe schema. Tools can declare a `timeout` field (in seconds). If omitted, falls back to a default (e.g., 60s, matching the Bash tool default).

3. **Stderr handling**: Stderr from toolbox tools is written to cake's log file. This gives tool authors a way to add diagnostic logging that can be used for debugging without polluting the model's output.

4. **Tool count limits**: Deferred to phase 2. Selectively enabling/disabling tools is preferable to a hard limit, but adds complexity. For now, all discovered tools are registered.

## Revision Notes

- 2026-05-07 / Codex: Migrated this historical plan into the new active ExecPlan directory and added lifecycle sections required by `.agents/PLANS.md`. The original feature design above remains as the starting implementation context.
