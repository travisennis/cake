# Cake Architecture

Cake is an AI coding assistant CLI that integrates with language models through OpenAI-compatible Chat Completions and Responses API backends. It provides a conversation-based interface with tool execution capabilities for file manipulation, code editing, and shell command execution. OpenRouter is one supported provider (selected automatically when the configured `base_url` is an `openrouter.ai` host, or via the `provider` setting), but any OpenAI-compatible endpoint works.

## Overview

The core problem cake solves: provide a safe, sandboxed environment for AI agents to interact with the local filesystem and execute commands, while maintaining conversation context across sessions.

Key design decisions:

- **Agent loop**: The model can request tool executions; results are fed back, and the loop continues until the model returns a final response
- **Conversation as data**: All interaction history is represented as typed `ConversationItem` values
- **Session persistence**: Conversations can be saved, resumed, continued, or forked
- **OS-level sandboxing**: Bash commands run within platform-specific sandboxes (Seatbelt on macOS, Landlock on Linux)
- **Strict layering**: Dependencies flow in one direction only, preventing circular imports

## Codemap

### Layer 4: CLI (`cli` module) and entry point (`main.rs`)

Entry point for user interaction. `main.rs` defines the top-level clap struct `CodingAssistant`, parses arguments, validates flag combinations, classifies clap errors into structured exit codes, and dispatches to a `CmdRunner` implementation.

Submodules under [`src/cli/`](file:///Users/travisennis/Projects/cake/src/cli/):

- **`cmd_runner`**: The `CmdRunner` trait — the single interface every command implements: `async fn run(&self, data_dir: &DataDir) -> anyhow::Result<()>`.
- **`output`**: `CliOutputSink`, formatting helpers for spinner messages, retry notices, and the done-summary line. Centralizes user-facing output formatting.
- **`run_mode`**: `RunMode` and `SessionStorage` enums describing whether a run is `New`/`Continue`/`Resume`/`Fork` and where its session is persisted.
- **`session_factory`**: Constructs the appropriate `Session` for the resolved `RunMode`.

The main `CodingAssistant` command lives in `src/main.rs` (not in a separate `instruct` module). The CLI layer is intentionally thin — it delegates all business logic to lower layers.

### Layer 3: Clients (`clients` module)

The bridge to external AI services and the orchestration layer for tool execution. The current module layout (see [src/clients/mod.rs](file:///Users/travisennis/Projects/cake/src/clients/mod.rs)):

**`agent`**: The public-facing `Agent` struct. Owns the user-visible API (`Agent::new`, `with_*` builders, `send`, telemetry emitters) and the high-level conversation loop. Delegates the per-turn HTTP work to `AgentRunner`, the rolling state to `ConversationState`, and the callback fan-out to `AgentObserver`. Exported as `clients::Agent`.

**`agent_runner`**: `AgentRunner` performs one API turn — building the HTTP client, calling the chosen `Backend`, handling retries via `retry::RetryPolicy`, and emitting per-attempt telemetry events.

**`agent_state`**: `ConversationState` (the running `Vec<ConversationItem>` history plus developer-context append helpers) and `accumulate_usage` for combining `Usage` across turns.

**`agent_observer`**: `AgentObserver` holds the optional streaming-JSON, persist, progress, and retry callbacks supplied by the CLI, and fans events out to them.

**`backend`**: The `Backend` enum (`Responses` | `ChatCompletions`) abstracts the wire-format choice. `Backend::from_api_type` maps `ApiType` to a backend; `send_request` and `parse_response` dispatch to the matching module. This is what replaces the older "Agent dispatches by `ApiType`" wiring.

**`responses`**: Backend for the Responses API. Provides `send_request()` and `parse_response()` functions that handle the Responses API wire format.

**`responses_types`**: Request/response DTOs for the Responses API, including `ProviderConfig`.

**`chat_completions`**: Backend for the Chat Completions API. Provides `send_request()` and `parse_response()` functions. Translates `ConversationItem` to/from chat completions format: groups consecutive `FunctionCall` items into assistant messages with `tool_calls`, maps `System` role to `"system"`, and skips `Reasoning` items.

**`chat_types`**: Request/response DTOs for the Chat Completions API (`ChatRequest`, `ChatResponse`, `ChatMessage`, `ChatTool`, `ChatToolCall`, etc.).

**`provider_strategy`**: `ProviderStrategy` applies provider-specific request shaping. Infers the provider from `base_url` (e.g. OpenRouter for `openrouter.ai` hosts), applies provider-specific HTTP headers (the OpenRouter `HTTP-Referer`/`X-Title` headers by default), and patches outbound messages where needed (e.g. the Kimi reasoning-content placeholder).

**`retry`**: `RetryPolicy`, `RequestOverrides`, `RetryStatus`, `RetryReason`, and `HttpFailure` types implementing the backoff/jitter logic, context-overflow recovery via `max_output_tokens` shrinking, and `Retry-After` / `x-should-retry` header handling.

**`skill_dedup`**: Wraps tool execution to detect when a Read of a `SKILL.md` activates a skill, deduplicates activations within a session, and tracks pending vs. active skills.

**`tools`**: Tool definitions and execution. Each tool (Bash, Read, Edit, Write) defines its JSON schema for the API and its execution logic. The `execute_tool` function dispatches to the appropriate implementation. Tools validate that paths are within the working directory or allowed temp directories before operating. The Bash tool also performs pre-execution command safety checks that block known-destructive commands (destructive git operations and dangerous `rm -rf`) before they reach the shell. Exported helpers: `ToolContext`, `summarize_tool_args`, `read_extract_path`.

**`tools::sandbox`**: Cross-platform sandboxing abstraction. Provides `SandboxConfig` and `SandboxStrategy` for restricting filesystem and network access. Platform-specific implementations use sandbox-exec (macOS) or Landlock LSM (Linux).

### Layer 2: Config, Types, Prompts

Foundation modules that provide data persistence, core types, and prompt generation.

**`config`**:

- `data_dir` (`DataDir`, `AgentsFile`): Manages cache directory (`~/.cache/cake/`), session directory (`~/.local/share/cake/sessions/`), both overridable via `CAKE_DATA_DIR`, plus AGENTS.md discovery.
- `session` (`Session`, `SessionWriter`): In-memory session state with JSONL serialization, plus the writer that performs atomic append/rename.
- `worktree`: Git worktree utilities for isolated execution environments.
- `model`: Contains `ApiType` enum (`Responses`/`ChatCompletions`), `ModelConfig` struct (model, api_type, base_url, api_key_env, temperature, top_p, max_output_tokens, reasoning_effort, reasoning_summary, reasoning_max_tokens, provider/providers), `ModelProvider`, `ProviderHeaders`, `ReasoningEffort`, and `ResolvedModelConfig` (resolves API key from env var). Defaults for model, base URL, API key env var, and providers live alongside these types — there is no separate `defaults` module.
- `settings` (`SettingsLoader`, `ModelDefinition`): TOML-based configuration loading from `settings.toml` files. Supports XDG-style global (`~/.config/cake/settings.toml`) and project-level (`.cake/settings.toml`) locations, with project settings overriding global settings for the same model name. Includes a `[skills]` section for controlling skill discovery and configured skill paths, plus a `directories` key for declaring additional read-write directories (merged across global and project files).
- `skills` (`Skill`, catalog builder): Skill discovery, parsing, and catalog management. Discovers `SKILL.md` files from project, configured, and user skill directories, parses YAML frontmatter, and builds an XML catalog for the system prompt. Skills are activated lazily via the Read tool and deduplicated within a session.
- `hooks` (`HooksLoader`, `HookSource`, `LoadedHooks`, `HookEvent`, `HookCommand`): TOML loading of user-defined hook commands from global and project `hooks.toml` files. The runtime that executes them lives in the top-level [`hooks`](file:///Users/travisennis/Projects/cake/src/hooks.rs) module (Layer 1).

Sessions are stored as flat `{uuid}.jsonl` files under `~/.local/share/cake/sessions/`. Each file's header contains the working directory, so `--continue` filters by matching the current directory.

**`types`**:

- `conversation`: `Role` enum (System, Developer, Assistant, User, Tool), `ConversationItem` enum, and `ReasoningContent`/`ReasoningContentKind` for round-tripping reasoning items.
- `session`: `SessionRecord` and `StreamRecord` enums plus the shared `*Data` structs that back their variants, `GitState`, and `TaskOutcome`/`TaskCompleteSubtype`.
- `usage`: `Usage`, `InputTokensDetails`, `OutputTokensDetails` — the backend-agnostic token usage shape both Chat Completions and Responses normalize into.

API wire-format DTOs live next to the backend that owns them: `clients::chat_types` for Chat Completions and `clients::responses_types` for the Responses API.

**`prompts`**: System prompt construction with AGENTS.md and skill catalog integration. Reads user-level (`~/.cake/AGENTS.md`), XDG config (`~/.config/AGENTS.md`), and project-level (`./AGENTS.md`) instruction files, plus discovered skills from `.agents/skills/`, and injects them into the system prompt.

### Layer 1: Foundation

Top-level modules in [`src/`](file:///Users/travisennis/Projects/cake/src/) that sit beneath the other layers:

**`exit_code`**: Classifies `anyhow::Error` values into structured exit codes (0=success, 1=agent error, 2=API error, 3=input error). The `main()` function uses this to return `std::process::ExitCode` instead of relying on the default Rust behavior of exiting 1 on any `Err`.

**`logger`**: File-only logging using tracing with daily rotation and 7-day retention. Defaults to INFO level, with debug/trace available via `RUST_LOG` environment variable.

**`hooks`**: The hook execution runtime. `HookRunner` and `HookContext` wrap the `LoadedHooks` value produced by `config::hooks` and execute matching user commands at well-defined points (e.g. before/after tool calls), forwarding stdin payloads and capturing stdout/stderr with a `HOOK_OUTPUT_LIMIT`. `ToolHookPlan` describes the action to take for a scheduled tool call (execute, mutate args, append context, or block).

**`session_telemetry`**: Optional structured per-run telemetry. Defines `SessionTelemetryWriter`, `SessionTelemetryContext`, `SessionTelemetrySettings`, `SessionTelemetryRecord`, `SessionTelemetryRunMode`, and the per-attempt event types (`AgentRunnerTelemetryEvent`, `ApiAttemptTelemetry`, `RequestOverridesSnapshot`, `RetryScheduledTelemetry`, `ToolCallTelemetry`) written by the agent.

**`time_format`**: Small formatting helpers (`format_seconds_tenths`, `format_duration_tenths`) shared by CLI output and telemetry so timing strings stay consistent.

External crates: `anyhow` for error handling, `tokio` for async runtime, `serde` for serialization, `reqwest` for HTTP, `tracing` for logging, `clap` for CLI parsing.

## Architectural Invariants

These constraints guide the design and are unlikely to change:

1. **Dependencies flow downward only**: `cli` → `clients` → `config/types/prompts`. Cross-layer imports are prohibited (e.g., `config` cannot import from `clients`).

2. **All internal imports use absolute paths**: `crate::module::Item`, never relative paths like `super::` or `self::`.

3. **ConversationItem is the single source of truth**: All conversation state flows through this enum. There is no parallel representation.

4. **Tools validate paths before execution**: Every filesystem operation checks that the target path is within the working directory, an allowed temp directory (`/tmp`, `/var/folders`, etc.), or a registered skill directory.

5. **Bash tool blocks destructive commands**: Known-destructive commands (e.g. `git reset --hard`, `git push --force`, `rm -rf` outside temp dirs) are rejected before execution as a best-effort safety guard.

6. **Session writes are atomic**: Sessions are written to a temp file, then renamed to the final path. The most recent session for a directory is determined by file modification time among files whose header matches the working directory.

7. **Sandboxing is opt-out, not opt-in**: Sandboxing applies to all bash commands unless explicitly disabled via `CAKE_SANDBOX=0`.

8. **No unwrap/expect in production code**: The clippy configuration denies these, enforced at compile time.

## System Boundaries

### CLI ↔ Client Boundary

The CLI layer owns argument parsing and user-facing error messages. The client layer owns all network communication and tool execution. The boundary is the `CmdRunner` trait: `async fn run(&self, data_dir: &DataDir) -> anyhow::Result<()>`. The CLI resolves a `ModelConfig` from settings.toml (or the `--model` flag) via `ModelDefinition::to_model_config()` → `ResolvedModelConfig::resolve()` → `Agent::new()`.

### Client ↔ Tool Boundary

Tools are pure functions from JSON arguments to `ToolResult`. The client owns the orchestration (concurrent execution, timeout handling, result aggregation). Tools own the validation and side effects.

### Config ↔ Filesystem Boundary

All filesystem access for configuration and sessions goes through `DataDir`. No other module constructs paths into `~/.cache/cake/` or `~/.local/share/cake/` directly.

### Host ↔ Sandbox Boundary

The sandbox configuration defines a strict boundary between what the host process can access and what the sandboxed subprocess can access. Read-only paths, read-write paths, and executable paths are explicitly enumerated.

## Cross-Cutting Concerns

### Error Handling

- Library code uses `thiserror` for typed errors
- Application code uses `anyhow` for context propagation
- Tool errors are stringified and returned to the model as function call output (the model can decide how to proceed)
- Exit codes are classified by the `exit_code` module based on error chain inspection:
  - `0` — success
  - `1` — agent/tool error (default for unclassified errors)
  - `2` — API error (HTTP 401/403/429, connection failure, timeout)
  - `3` — input error (missing API key, no prompt, invalid model name, bad flags)

### Logging

- File appender with daily rotation: `~/.cache/cake/cake.YYYY-MM-DD.log`
- Maximum 7 files retained (older files automatically deleted)
- Default level: INFO (debug/trace require `RUST_LOG=cake=debug` or `RUST_LOG=cake=trace`)
- Pattern includes timestamps, levels, file:line for debugging
- Non-blocking writes (async-safe)
- Session lifecycle events are logged at INFO level: session creation, continuation, resumption, forking, saving, and loading

### Session Management

Sessions enable conversation persistence across separate cake invocations:

- **New**: Generate UUID, create fresh session
- **Continue**: Load the most recent session for the working directory, append new messages
- **Resume**: Load a specific session by UUID, continue from there
- **Fork**: Copy history from existing session, generate new UUID (diverges without affecting original)

Storage format is JSONL (JSON Lines): each line is a complete JSON object, allowing append-only writes and partial recovery.

### Machine-Readable Output

When `--output-format stream-json` is specified, the system emits machine-readable JSON events:

- `ConversationItem` values as they are created
- Final usage statistics
- Error events

When `--output-format json` is specified, a single JSON summary object is printed at completion containing the result, session metadata, token usage, working directory, session file path, turn count, and elapsed time. Both modes suppress console progress reporting to avoid polluting stdout.

## Finding Things

Use symbol search to locate specific implementations:

- **Agent public API**: `Agent` in `clients/agent.rs`
- **Agent loop entry**: `Agent::send` in `clients/agent/agent_loop.rs`
- **Per-turn HTTP + retry**: `AgentRunner::complete_turn` in `clients/agent_runner.rs` (the high-level wrapper is `Agent::complete_turn` in `clients/agent/agent_loop.rs`)
- **API dispatch**: `Backend::send_request` / `Backend::parse_response` in `clients/backend.rs`
- **Conversation state**: `ConversationState` and `accumulate_usage` in `clients/agent_state.rs`
- **Provider-specific headers/shaping**: `ProviderStrategy` in `clients/provider_strategy.rs`
- **Retry policy and overrides**: `RetryPolicy`, `RequestOverrides`, `RetryStatus` in `clients/retry.rs`
- **Skill activation dedup**: `execute_tool_with_skill_dedup` in `clients/skill_dedup.rs`
- **Tool execution**: `execute_tool` function in `clients/tools/mod.rs`
- **Chat Completions translation**: `build_messages` in `clients/chat_completions.rs`
- **Bash safety checks**: `clients/tools/bash_safety/`
- **Model configuration**: `ModelConfig`, `ApiType`, `ResolvedModelConfig` in `config/model.rs`
- **Session loading**: `Session::load` or `DataDir::load_latest_session`
- **Path validation**: `validate_path_in_cwd`
- **Sandbox profiles**: `SandboxConfig` and platform-specific implementations in `clients/tools/sandbox/`
- **Conversation types**: `ConversationItem` in `types/conversation.rs`
- **Session record types**: `SessionRecord`, `StreamRecord` in `types/session.rs`
- **Hook runtime**: `HookRunner`, `ToolHookPlan` in `src/hooks.rs`; loading in `config/hooks.rs`
- **Telemetry**: `SessionTelemetryWriter` and friends in `src/session_telemetry.rs`
- **Exit code classification**: `classify` function in `exit_code.rs`

## Reading List

For deeper understanding of specific subsystems (see [docs/design-docs/index.md](file:///Users/travisennis/Projects/cake/docs/design-docs/index.md) for the full list):

- `docs/design-docs/cli.md` - Command-line interface and command dispatch
- `docs/design-docs/conversation-types.md` - ConversationItem enum and API types
- `docs/design-docs/prompts.md` - System prompt construction and AGENTS.md integration
- `docs/design-docs/session-management.md` - Session lifecycle and storage format
- `docs/design-docs/sandbox.md` - Sandbox implementation details
- `docs/design-docs/streaming-json-output.md` - Machine-readable output format
- `docs/design-docs/logging.md` - Logging configuration and troubleshooting
- `docs/design-docs/tools.md` - Tool framework (Bash, Read, Edit, Write)
- `docs/design-docs/settings.md` - `settings.toml` loading and precedence
- `docs/design-docs/skills.md` - Skill discovery, catalog, and activation
- `docs/design-docs/hooks.md` - User-defined hook commands and lifecycle
- `docs/design-docs/api-retry-strategy.md` - Retry policy, backoff, and context-overflow recovery
- `docs/design-docs/edit-tool-session-analysis.md` - Analysis of edit-tool behavior across sessions
- `docs/references/responses-api.md` - Responses API integration notes
- `docs/references/chat-completions-api.md` - Chat Completions API integration notes
