# Toolboxes Feature Plan for Cake

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows [docs/workflow/exec-plans.md](../../workflow/exec-plans.md). The core toolbox feature and review are complete; the optional `cake tools` management subcommands were excluded from this plan's completed scope. The durable trust and execution boundary is recorded in [ADR-017](../../../docs/adr/017-trusted-executable-toolbox-tools.md).

## Purpose / Big Picture

Cake should support user-defined executable tools in addition to built-in Bash, Read, Edit, and Write. After this work, a user can place executable tool scripts in configured toolbox directories, run cake, and have those tools discovered, described, exposed to the model with a `tb__` prefix, and invoked through the same tool execution path as built-in tools.

The behavior is observable by creating a tiny executable toolbox script, passing its directory through `CAKE_TOOLBOX` or `--toolbox`, and seeing the model receive and execute the corresponding `tb__<name>` tool.

## Progress

- [x] (2026-05-07 18:43Z) Confirmed the current codebase has no `toolbox`, `CAKE_TOOLBOX`, `tb__`, or `TOOLBOX_ACTION` implementation.
- [x] (2026-05-07 18:43Z) Migrated this plan to `.agents/exec-plans/active/toolboxes-plan.md` and added the required ExecPlan lifecycle sections.
- [x] (2026-07-14) Re-verified plan against current codebase; toolbox support still absent, but the tool system was refactored by task 051 (`ToolRegistry`). Phase 0 and Phase 4 revised accordingly; see Surprises & Discoveries.
- [x] (2026-07-14) Implemented toolbox discovery and parsing (`src/config/toolbox.rs`: `CAKE_TOOLBOX`/`--toolbox`/default-dir resolution, file filtering, describe protocol with JSON and text formats, name validation, duplicate precedence).
- [x] (2026-07-14) Registered discovered toolbox tools with the agent and dispatched `tb__*` calls (`ToolExecutor` widened to a shared closure; `clients/tools/toolbox.rs` execute protocol; `Agent::with_toolbox_tools`; read-only exclusion in both builder orders; system-prompt tool list includes toolbox tools).
- [x] (2026-07-14) Added `--toolbox <DIR>` flag, unit tests (describe parsing, discovery filtering, execute protocol via fixture scripts), end-to-end integration tests (`tests/toolbox.rs`: discovery → prompt → dispatch → session record through the real binary against a mocked Responses API), and documentation (README, `docs/design-docs/tools.md`, ARCHITECTURE.md).
- [x] (2026-07-14) Applied review fixes: toolbox executables are never run (not even describe) under the read-only sandbox policy; stdout/stderr are read with hard caps (50KB/10KB) instead of buffering unbounded output; the execute timeout covers argument delivery, output capture, and process exit (stdin fed from a detached writer); tool names are capped at 60 characters so `tb__` + name stays within the 64-character provider limit.
- [x] (2026-07-14) Applied second review round: describe output is now read through the shared bounded reader (64KB stdout / 10KB stderr caps; a runaway tool like `yes` is skipped instead of exhausting memory), and toolbox directories are resolved in `prepare_run` against the invocation directory before any `--worktree` cwd change (relative `CAKE_TOOLBOX`/`--toolbox` entries anchored like `--add-dir`). `read_streams_bounded` moved to `config::toolbox` (config cannot import clients; clients reuses it downward).
- [x] (2026-07-14) Applied third review round: every toolbox describe and execute subprocess starts in its own Unix process group, and timeout/output-cap paths kill the entire group so descendants cannot continue consuming resources or mutating the workspace after cake reports termination. Added descendant-mutation regressions for execute timeout, execute output cap, and describe output cap.
- [x] (2026-07-14) Applied fourth review round: full `inputSchema` declarations are normalized or rejected to preserve the executor's top-level object contract, and text-format calls reject line-breaking names/values before spawn so multiline input cannot create injected protocol records.
- [x] (2026-07-14) Applied fifth review round: text-format describe parsing now applies the executor's shared argument-name validation, so tools declaring names the `key=value` protocol cannot encode are skipped instead of advertised unusably.
- [x] (2026-07-14) Applied sixth review round: duplicate text-format parameter declarations are rejected during describe parsing, preventing overwritten properties and duplicate JSON Schema `required` entries from reaching providers.
- [x] (2026-07-14) Ran an XL preflight: added ADR-017 and missing CLI/sandbox design documentation, compiled all discovered schemas as JSON Schema draft 2020-12, removed speculative `Tool::new`/`ToolboxEntry::source_dir` API, and reconciled the task and ExecPlan with the implemented state.
- [x] (2026-07-14) Completed final verification with `just ci`: 1,009 unit tests and all integration targets passed, coverage reached 91.56%, and the CRAP regression gate reported zero regressions. Optional `cake tools` subcommands remain separate follow-up scope.

## Surprises & Discoveries

- Historical observation (2026-05-07): Toolbox support was unimplemented before this plan was executed. Evidence: `rg -n "toolbox|CAKE_TOOLBOX|tb__|TOOLBOX_ACTION" src docs README.md Cargo.toml` produced no matches.
- Observation (2026-07-14): Task 051 (completed) already replaced the free `execute_tool()` function with a `ToolRegistry` of `ToolEntry { definition, execute }` in `src/clients/tools/mod.rs`, and `Agent` already stores `tools: ToolRegistry`. The original Phase 0 refactor ("move `execute_tool` to an Agent method") and its borrow-checker notes are obsolete. Evidence: `ToolRegistry` at `src/clients/tools/mod.rs:551`, `ToolRegistry::execute()` at `mod.rs:603`, `Agent.tools` at `src/clients/agent.rs:54`.
- Historical observation (2026-07-14): `ToolExecutor` was a plain function pointer, so it was widened to a shared closure that can capture each toolbox tool's path, format, and timeout.
- Observation (2026-07-14): `Agent::new(config, initial_messages)` no longer matches the signature quoted in Phase 4; the codebase uses `with_*` builder methods (`with_tool_context`, etc.) for optional agent configuration.
- Observation (2026-07-14): `Agent::with_tool_context` calls `ToolRegistry::retain_read_safe_tools()` under `SandboxPolicy::ReadOnly` because Edit/Write bypass the OS sandbox. Toolbox tools are unsandboxed external processes and must likewise be excluded under `ReadOnly`, or the policy's no-mutation guarantee breaks. Not covered by the original plan.
- Observation (2026-07-14): `format_tool_list_section()` (`src/clients/tools/mod.rs:826`) derives the system-prompt "Available tools" section from `default_tool_registry()` and asserts "Only these tools are available." It must be fed the actual registry (including toolbox tools) or toolbox tools will contradict the prompt.
- Observation (2026-07-14): Tool calls now flow through PreToolUse hooks (`ToolHookPlan`, `src/hooks.rs`) and per-turn scheduling (`schedule_tool_calls`, `src/clients/tools/scheduling.rs`, ADR-013). Toolbox calls will get singleton scheduling groups because `mutating_target()` only understands Edit/Write --- acceptable, but mutations by toolbox tools cannot be serialized by path.
- Observation (2026-07-14): Phase 6's `summarize_tool_args()` no longer exists; tool progress/display now goes through `AgentObserver` and `CliOutputSink` (`src/cli/output.rs`).
- Observation (2026-07-14): CLI subcommands live in the `Commands` enum in `src/cli/mod.rs` (currently `Debug`, `Sessions`) implementing the `CmdRunner` trait; a `cake tools` subcommand belongs there, not directly in `main.rs`.

## Decision Log

- Decision: Classify this plan as active during the ExecPlan migration. Rationale: The plan describes a feature that is not present in the current implementation and still contains actionable design detail. Date/Author: 2026-05-07 / Codex
- Decision: Register toolbox tools as ordinary `ToolEntry` values by widening `ToolExecutor` to a boxed closure, instead of a separate toolbox registry with `tb__*` fallback dispatch. Rationale: Task 051's `ToolRegistry` already unifies definition and execution; a single dispatch path keeps hooks, scheduling, and read-only exclusion uniform. Date/Author: 2026-07-14 / Claude
- Decision: Exclude toolbox tools from the registry under `SandboxPolicy::ReadOnly`. Rationale: Toolbox tools are unsandboxed external processes; offering them under read-only would break the policy's no-mutation guarantee, same rationale as `retain_read_safe_tools()` for Edit/Write. Date/Author: 2026-07-14 / Claude
- Decision: Support the describe `timeout` field in the JSON format only, and truncate toolbox execute output at 50KB (matching the Bash tool's inline cap). Rationale: In the text format a `timeout: 30` line is ambiguous with a parameter declaration; the output cap protects the session from runaway tool output. Date/Author: 2026-07-14 / Claude

## Outcomes & Retrospective

The core feature now discovers trusted executable tools, validates their provider-facing schemas, registers them through the existing tool path, executes both supported protocols with bounded resources, and excludes the trust boundary entirely under `read-only`. Focused unit and real-binary integration tests cover the contract. Review exposed several malformed-input and subprocess-tree edge cases; validating at discovery and owning each invocation's process group keeps those failures local and truthful to providers.

The optional `cake tools` list/show/use/make commands were explicitly deferred. They are not required to discover or invoke toolbox tools during an agent run and should be tracked as separate follow-up work rather than keeping the core implementation open-ended.

Final verification passed the repository's complete `just ci` gate: formatting, both clippy configurations, 1,009 unit tests, every integration target, 91.56% line coverage, zero CRAP regressions, import lint, and module-size lint. The implementation therefore meets the plan's observable behavior and quality gates with no known remaining core risk.

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

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## How This Maps to Cake's Architecture

### Current Tool System (as of 2026-07-14)

Cake has four built-in tools: `Bash`, `Read`, `Edit`, `Write`. They are: - Defined as `Tool` structs (name, description, JSON schema) in `src/clients/tools/mod.rs`. Child tool modules construct definitions through `pub(super)` fields. - Registered as `ToolEntry { definition, execute }` values in a `ToolRegistry` built by `default_tool_registry()` (`src/clients/tools/mod.rs:847`); `Agent` stores `tools: ToolRegistry`. - Dispatched by name via `ToolRegistry::execute()`; executors are shared closures so toolbox entries can capture per-tool state. - Gated by PreToolUse hooks (`ToolHookPlan`) and grouped per turn by `schedule_tool_calls()` (ADR-013) before executing concurrently. - Under `SandboxPolicy::ReadOnly`, `retain_read_safe_tools()` removes Edit/Write from the registry. - Results are `ToolResult { output: String }`.

### Integration Points

The toolbox feature touches these layers:

  | Layer             | Module                     | Change                                                         |
  | ----------------- | -------------------------- | -------------------------------------------------------------- |
  | Layer 2 (Config)  | `config/`                  | New `toolbox` module for discovery and protocol                |
  | Layer 3 (Clients) | `clients/tools/mod.rs`     | Widen `ToolExecutor` to hold per-tool state                    |
  | Layer 3 (Clients) | `clients/tools/toolbox.rs` | Toolbox execute logic (builds `ToolEntry` values)              |
  | Layer 3 (Clients) | `clients/agent.rs`         | `with_*` builder to inject toolbox entries into `ToolRegistry` |
  | Layer 4 (CLI)     | `cli/mod.rs`, `main.rs`    | Discovery at startup and repeatable `--toolbox` flag           |

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## Implementation Plan

### Phase 0: Prerequisites

**Widen `ToolExecutor` to carry per-tool state**: (Revised 2026-07-14; replaces the obsolete "refactor `execute_tool` to an Agent method" step --- task 051 already introduced `ToolRegistry` and `Agent` already dispatches through it.) `type ToolExecutor = fn(Arc<ToolContext>, String) -> ToolFuture` is a plain fn pointer, so a `ToolEntry` cannot capture the toolbox executable's path, format, or timeout. Change the executor type to a boxed closure, e.g. `Arc<dyn Fn(Arc<ToolContext>, String) -> ToolFuture + Send + Sync>`. Built-in tools keep working (fn pointers coerce into the closure type); toolbox entries capture their `ToolboxTool` in the closure. This removes the need for a separate toolbox registry or `tb__*` fallback dispatch: toolbox tools become ordinary `ToolEntry` values registered under their `tb__<name>`.

### Phase 1: Discovery

**New module**: `src/config/toolbox.rs`

**Environment variable**: `CAKE_TOOLBOX` (following cake's `CAKE_` prefix convention).

**Default directory**: `~/.config/cake/tools` (under cake's config directory).

**Discovery logic**: 1. If `CAKE_TOOLBOX` is set and non-empty, split on `:` and scan those directories. 2. If `CAKE_TOOLBOX` is unset, scan the default directory (`~/.config/cake/tools`). 3. If `CAKE_TOOLBOX` is set to empty string, skip toolbox scanning entirely. 4. For each directory, enumerate files, applying these filters: - **Skip** hidden files (dot-prefix). - **Skip** files with `.md` or `.txt` extensions. - **Skip** non-executable files (check execute bit via `std::os::unix::fs::PermissionsExt`). - **Skip** directories. 5. Earlier directories take precedence for name conflicts.

> **Note on Amp compatibility**: The Amp documentation is ambiguous on the unset case (one sentence says it uses the default directory, the next says no scanning). Our behavior (scan default directory when unset) is the more user-friendly interpretation and likely matches Amp's actual behavior.

**Output**: A `Vec<ToolboxEntry>` where each entry contains:

```rust
struct ToolboxEntry {
    /// Name as discovered (filename)
    filename: String,
    /// Full path to the executable
    path: PathBuf,
}
```

### Phase 2: Describe Protocol

**New module**: `src/config/toolbox.rs` (continued) or `src/clients/tools/toolbox.rs`

For each discovered `ToolboxEntry`, run the executable with `TOOLBOX_ACTION=describe` and `AGENT=cake`, capture stdout, and parse the schema.

**Format detection**: 1. Attempt JSON parse of stdout. 2. If JSON fails, parse as text format. 3. Store the detected format alongside the tool definition.

**JSON format parsing**: - Support both compact `args` format and full `inputSchema` (JSON Schema draft 2020-12). - `args` format: `{"param": ["type", "description"]}` is converted internally to a JSON Schema object. - `inputSchema` format: normalize a missing top-level `type` to `object`, reject explicit non-object roots, and compile the result as JSON Schema draft 2020-12 before registration. - The `name` field from the JSON output is the authoritative tool name.

**Text format parsing**: - Multiple `description:` lines are concatenated with newlines. - Parameter lines must include an explicit type (no defaulting to string). - Optional parameters are marked with a `?` suffix on the type: `param: string? description`. - Empty lines are ignored.

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

The toolbox child module constructs a `Tool` definition directly for registration with the API; no public constructor is required.

### Phase 3: Execute Protocol

**Dispatch**: (Revised 2026-07-14.) No special `tb__*` fallback branch is needed --- each toolbox tool is registered as a `ToolEntry` whose executor closure captures its `ToolboxTool`, and `ToolRegistry::execute()` finds it by name like any built-in.

**Execution logic** (executor closure in `clients/tools/toolbox.rs`): 1. Use the captured `ToolboxTool` (path, format, timeout). 2. Spawn the executable with: - `TOOLBOX_ACTION=execute` - `AGENT=cake` - `CAKE_THREAD_ID=<session_id>` - `AGENT_THREAD_ID=<session_id>` 3. Write arguments to stdin: - JSON format: write the arguments JSON object directly. - Text format: convert JSON args to `key=value\n` pairs (using `=` as the separator). 4. Capture stdout as the tool output. 5. Check exit code: 0 = success, non-zero = error. 6. Return `ToolResult { output }`.

**Timeout**: Apply the same timeout as the Bash tool (configurable, default 60s).

**Concurrency**: No limit on concurrent toolbox processes. In practice, models rarely request more than 2-3 tool calls per turn, and the OS handles a handful of child processes trivially.

**Sandboxing consideration**: Toolbox tools run as separate processes. They are NOT sandboxed by cake's Seatbelt/Landlock profiles (unlike Bash). This is intentional: toolbox tools are user-provided and trusted. Document this clearly.

### Phase 4: Agent Integration

(Revised 2026-07-14 to match the post-task-051 architecture.)

**In `Agent`**: `Agent::new(config, initial_messages)` already builds `tools: default_tool_registry()`. Add a builder method following the existing `with_*` pattern:

```rust
pub fn with_toolbox_tools(mut self, toolbox_tools: Vec<ToolboxTool>) -> Self {
    for tb_tool in toolbox_tools {
        self.tools.push(toolbox_entry(tb_tool)); // ToolEntry with capturing executor
    }
    self
}
```

**Read-only policy**: `with_tool_context` strips Edit/Write under `SandboxPolicy::ReadOnly` because they bypass the OS sandbox. Toolbox tools are unsandboxed external processes, so they must also be excluded under `ReadOnly` regardless of the order in which `with_toolbox_tools` and `with_tool_context` are called.

**System prompt**: `format_tool_list_section()` currently derives the "Available tools" section from `default_tool_registry()` and states "Only these tools are available." It must reflect registered toolbox tools (e.g., accept the actual registry or the toolbox list) so the prompt does not contradict the tool set.

### Phase 5: CLI Integration

**Startup** (in `main.rs`): 1. After parsing args, discover toolbox entries. 2. Run `describe` on each, build `ToolboxTool` structs (skipping failures with warnings). 3. Pass the `Vec<ToolboxTool>` to `Agent::new()`.

**New CLI flag**: `--toolbox <DIR>` to add extra toolbox directories (appended to `CAKE_TOOLBOX` dirs).

**New subcommand** (optional, lower priority): `cake tools` with subcommands: - `cake tools list`: Show all tools (built-in + toolbox) with source labels. - `cake tools show <name>`: Display a tool's schema. - `cake tools use <name> [--arg key=value...]`: Invoke a tool directly. - `cake tools make [--bash|--python|--node] <name>`: Scaffold a new tool in the default directory. Defaults to bash.

### Phase 6: Summarization and Progress

(Revised 2026-07-14: `summarize_tool_args()` no longer exists.)

**Display**: Tool progress and results now flow through `AgentObserver` (`src/clients/agent_observer.rs`) and `CliOutputSink` (`src/cli/output.rs`). Verify `tb__*` calls display like built-in tools (e.g., `tb__run_tests: running...`); add a `tb__*` argument-summary branch there if per-tool formatting exists.

**Machine-readable output**: Confirm `tb__*` tool calls serialize cleanly through the `stream-json` output and session transcript records (they should, since they use the standard tool-call path --- verify with a fixture tool).

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

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

### Toolbox tools as ordinary ToolRegistry entries (supersedes "execute_tool as an Agent method")

The original plan predated task 051's `ToolRegistry`. Instead of a separate toolbox registry with `tb__*` fallback dispatch, widen `ToolExecutor` from a fn pointer to a boxed closure so toolbox tools register as ordinary `ToolEntry` values. One dispatch path; hooks, scheduling, and the read-only exclusion mechanism apply uniformly.

### Scaffolding defaults

`cake tools make` defaults to bash scaffolding, with `--python` and `--node` options available.

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## Module Placement

Following cake's layering rules (dependencies flow downward only):

```
Layer 4 (CLI)
  main.rs              - startup discovery, --toolbox flag
  cli/mod.rs           - repeatable --toolbox flag

Layer 3 (Clients)
  clients/tools/mod.rs - widen ToolExecutor to boxed closure
  clients/tools/toolbox.rs - toolbox executor closure, ToolEntry construction
  clients/agent.rs     - with_toolbox_tools() builder, ReadOnly exclusion

Layer 2 (Config)
  config/toolbox.rs    - discovery (scan dirs, find executables, filter)
                       - describe protocol (run executable, parse output)
                       - ToolboxEntry, ToolboxTool, ToolboxFormat types
```

Discovery and describe parsing belong in Layer 2 (config) because they deal with filesystem scanning and configuration. Execution belongs in Layer 3 (clients/tools) because it's tool execution logic alongside the existing tools.

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## Implementation Order

1. Phase 0: Widen `ToolExecutor` to a boxed closure
2. `config/toolbox.rs`: Discovery + describe protocol + types
3. `clients/tools/toolbox.rs`: Execute protocol as capturing `ToolEntry` executors
4. `clients/agent.rs`: `with_toolbox_tools()` builder, `ReadOnly` exclusion
5. `format_tool_list_section()`: reflect toolbox tools in the system-prompt tool list
6. `main.rs`: Wire up discovery at startup
7. Tests for each layer
8. CLI subcommands in `cli/mod.rs` (lower priority, can be a follow-up)

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

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

### Deferred Property-Based Follow-up

No new dependency was added for the core implementation. If protocol surface expands, consider `proptest` coverage for:

- **Text format parsing roundtrip**: Generate arbitrary tool names, descriptions, and parameter lists → serialize to text format → parse → verify all fields preserved.
- **`key=value` serialization roundtrip**: Generate random `HashMap<String, String>` → serialize to `key=value\n` → deserialize → verify equality. Surfaces edge cases like values containing `=`, empty values, unicode.
- **JSON `args` → `inputSchema` conversion**: Generate arbitrary `args` maps → convert to JSON Schema → verify the schema validates the expected inputs.

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## Resolved Questions

1. **Project-level tools (`.cake/tools/`)**: Only scanned when explicitly listed in `CAKE_TOOLBOX`. No automatic scanning of project-level directories. This avoids the security risk of cloned repos injecting tools.

2. **Timeout configuration**: Per-tool timeout, specified in the describe schema. Tools can declare a `timeout` field (in seconds). If omitted, falls back to a default (e.g., 60s, matching the Bash tool default).

3. **Stderr handling**: Stderr from toolbox tools is written to cake's log file. This gives tool authors a way to add diagnostic logging that can be used for debugging without polluting the model's output.

4. **Tool count limits**: Deferred to phase 2. Selectively enabling/disabling tools is preferable to a hard limit, but adds complexity. For now, all discovered tools are registered.

## Revision Notes

- 2026-05-07 / Codex: Migrated this historical plan into the new active ExecPlan directory and added lifecycle sections required by `.agents/PLANS.md`. The original feature design above remains as the starting implementation context.
- 2026-07-14 / Claude: Re-verified against the current codebase before starting task 121. Toolbox support is still absent, but task 051's `ToolRegistry` refactor invalidated Phase 0 (no free `execute_tool` to move), the Phase 4 constructor sketch, and Phase 6 (`summarize_tool_args` removed). Revised those phases, the integration table, module placement, and implementation order; added decisions on registry integration and `ReadOnly` exclusion; recorded new integration points (system-prompt tool list, PreToolUse hooks, ADR-013 scheduling, `cli/mod.rs` Commands enum).
- 2026-07-14 / Codex: Completed the XL preflight, added ADR-017 and missing design documentation, strengthened schema validation and subprocess tests, reduced execution-path complexity to satisfy the CRAP gate, recorded final `just ci` evidence, and closed the plan with optional management subcommands explicitly deferred.
