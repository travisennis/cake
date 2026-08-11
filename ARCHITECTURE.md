# Cake architecture

This document records durable system boundaries and invariants. It deliberately does not list modules, symbols, or request fields; source search and generated documentation are more accurate authorities for implementation location.

## System shape

```text
CLI and configuration
        |
        v
Agent loop -----> API backend -----> OpenAI-compatible provider
   |
   +-----> hooks (trusted control-plane commands)
   |
   +-----> tool registry
              |
              +-----> Read / Edit / Write path validation
              +-----> Bash LLM-judge preflight and OS sandbox
              +-----> toolbox executables (trusted extensions)
   |
   +-----> session transcript / stream output / telemetry
```

Cake is a binary crate. The CLI resolves configuration and run mode, builds an agent, and owns user-facing output. The agent loop owns conversation state, provider turns, tool scheduling, hooks, retries, and event fan-out. Backend adapters translate a shared conversation representation to Chat Completions or Responses wire formats. Configuration and persisted record types sit below those orchestration layers.

## Core data flow

1. The CLI resolves settings, model credentials, sandbox policy, instructions, skills, hooks, toolbox tools, and session mode.
2. Cake builds a stable system prompt plus mutable developer context.
3. A backend sends the typed conversation and registered tool definitions to the provider.
4. A final assistant message ends the loop. Tool calls are validated, hooked, scheduled, executed, recorded in issue order, and returned to the provider for another turn.
5. Conversation and lifecycle records are fanned out to the session writer, stream-json output, progress renderer, and telemetry as applicable.

## Boundaries

### CLI and agent

The CLI owns argument validation, configuration resolution, session selection, and human or machine output. The agent owns provider communication and tool orchestration. Provider and tool code must not decide how CLI errors or progress are rendered.

### Conversation and backends

`ConversationItem` is the internal conversation authority. Backends translate it at their edges; provider-specific wire shapes must not become a second conversation model. Request and response snapshots protect those translations.

### Agent and tools

The registry pairs provider-facing schemas with executors. Tools own argument and path validation plus their side effects. The agent owns hooks, concurrency, timeouts, result ordering, persistence, and feeding results back to the model.

Built-in Edit and Write calls targeting the same canonical path execute sequentially in issue order. Other calls may execute concurrently, including Bash and toolbox commands that happen to mutate the same path; results remain attributable to their original calls.

### Host and model-generated commands

Read, Edit, and Write enforce allowed paths in-process. Bash adds an LLM-judge command-safety preflight (ADR-018) and an operating-system filesystem sandbox: Seatbelt on macOS and Landlock on Linux. Every non-empty command is judged before spawn; the judge is default-on and fail-closed with no deterministic rule floor, and it replaced the compiled `bash_safety` guard. Hooks and toolbox executables are trusted control-plane extensions outside that sandbox. [Security](docs/security.md) defines the guarantees and limitations.

### Persistence and integrations

Persisted sessions are append-only, versioned JSONL logs. Stream-json is a current-task event stream, not a resumable session file. Public record shapes, configuration, exit codes, hook input/output, and toolbox protocols are compatibility contracts described in [Integrations](docs/integrations.md).

## Invariants

- Dependencies flow from `cli` toward `clients`, `config`, `prompts`, and `types`; verified by `just lint-deps`.
- Production imports use absolute `crate::` paths; verified by `just lint-imports`.
- Production code does not use `unwrap` or `expect`; verified by clippy.
- Conversation state has one typed internal representation.
- Filesystem tools validate paths before side effects.
- Sandboxing is default-on and availability failures fail closed, except for the documented recognized nested-Seatbelt fallback.
- Session files are append-only and versioned; older records are never silently rewritten.
- Machine-readable stdout contains only its declared JSON format.
- Provider-specific behavior remains at provider/backend boundaries.
- Untrusted model actions never acquire the authority of trusted hooks or toolbox executables implicitly.

## Authorities

Use `src/main.rs` and `cake --help` for CLI shape, `src/config/` for configuration and sessions, `src/types/` for internal and serialized records, `src/clients/` for providers and the agent loop, `src/clients/tools/` for tool and sandbox behavior, snapshots for wire examples, and `justfile` for repository commands. Architecture changes only when the boundaries or invariants above change.

## Related decisions

- [ADR 001](docs/adr/001-agent-loop-architecture.md), the agent loop.
- [ADR 008](docs/adr/008-structured-provider-headers.md), structured provider headers.
- [ADR 011](docs/adr/011-interrupt-handling.md), interrupt handling and graceful shutdown.
- [ADR 012](docs/adr/012-schema-constrained-final-output.md), schema-constrained final output.
- [ADR 013](docs/adr/013-per-path-serialization-of-mutating-tool-calls.md), serialization of mutating tool calls.
- [ADR 018](docs/adr/018-llm-judge-command-gate.md), the LLM judge command gate.
