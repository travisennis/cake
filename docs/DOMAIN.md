# Cake Domain Model

## What Cake Is

Cake is a CLI tool that runs an AI coding assistant loop in your terminal. You send it a prompt, it talks to an LLM via an OpenAI-compatible API, and the model can read files, edit code, run bash commands, and write files --- all inside an OS-level sandbox --- until it returns a final response.

## Core Concepts

### Agent Loop

The model receives a system prompt, conversation history, and tool definitions. It replies with either text (the final answer) or tool calls. Each tool call is executed, its result is fed back, and the loop continues. This repeats until the model produces a final text response or hits a turn limit.

### Conversation as Data

Every interaction --- user message, model response, tool call, tool result, reasoning block --- is a typed `ConversationItem` value. This unified representation lets the system serialize, persist, compare, and replay conversations regardless of the API backend.

### Session

A session is a long-lived conversation, identified by a UUID, persisted as an append-only JSONL file. Sessions support four operations:

- **New**: Generate UUID, start fresh.
- **Continue**: Load the most recent session for the working directory, append a new task.
- **Resume**: Load a specific session by UUID.
- **Fork**: Clone a session's history into a new UUID --- a divergence point without affecting the original.

Each CLI invocation within a session is a **task**: one `task_start` → conversation records → one `task_complete`.

### API Backends

Cake supports two OpenAI-compatible wire formats:

- **Chat Completions API**: The original `/v1/chat/completions` endpoint. Tool calls are embedded as `tool_calls` in assistant messages.
- **Responses API**: The newer `/v1/responses` endpoint with native item-based conversation representation.

Any provider that implements one of these protocols works --- OpenRouter, OpenAI, Anthropic via proxy, local models, etc.

### Tools

The model can invoke four tools, each a pure function from JSON arguments to a result string:

- **Bash**: Run shell commands in a sandboxed subprocess.
- **Read**: Read files from the project directory, temp directories, or allowed paths.
- **Edit**: Apply surgical text patches to existing files.
- **Write**: Create or overwrite files.

All tools validate paths before operating. The Bash tool additionally blocks known-destructive commands as a best-effort guard.

### Sandbox

Every bash command runs inside an OS-level filesystem sandbox:

- **macOS**: `sandbox-exec` with a deny-default Seatbelt profile.
- **Linux**: Landlock LSM (kernel 5.13+).

The sandbox restricts filesystem access to the project directory, temp paths, toolchain caches, and explicit overrides. It does not restrict network access. Sandboxing is opt-out via `CAKE_SANDBOX=off`.

### Skills

Skills are self-contained instructions (`SKILL.md` files) auto-discovered from configured directories. The model reads them via the Read tool, which triggers lazy activation. Each skill activates at most once per session.

### Hooks

User-defined shell commands that run before or after tool execution. Defined in `hooks.toml` files (global and project-level merged). Hooks receive tool context via stdin and can allow, deny, mutate arguments, or append context.

### Settings

Configuration is TOML-based with a layered precedence model:

1. CLI flags (highest priority)
2. Project-level `.cake/settings.toml`
3. User-level `~/.config/cake/settings.toml` (lowest priority)

Models, profiles, skills paths, hook configs, and persistent directories are all defined here.
