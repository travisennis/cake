## Add a stdio MCP client to Cake

This ExecPlan is a living document, maintained per docs/workflow/exec-plans.md. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be kept current as work proceeds.

## Purpose / Big Picture

After this change, a Cake user can put an MCP server definition in a JSON file and have that server's tools appear to the model alongside Bash, Read, Edit, Write, and toolbox tools. The motivating case is Safari 27, which exposes browser inspection and interaction as an MCP server: a user adds

```
{
  "mcpServers": {
    "safari-mcp": {
      "command": "/usr/bin/safaridriver",
      "args": ["--mcp"]
    }
  }
}
```

to `~/.config/cake/mcp.json`, runs Cake, and can ask it to open a page in Safari, read the DOM, take a screenshot, or exercise the page. The same file format works for any stdio MCP server.

Success is observable in three ways. `cake debug mcp` lists the server, its connection status, and the tools it advertises. A prompt run shows the tools in the system prompt tool list and executes an `mcp__<server>__<tool>` call through the provider. A server that is missing or broken produces a warning and does not prevent the rest of the run.

## Progress

- [x] (2026-11-08) Record ADR 055 and task 172.
- [x] (2026-11-08) Add `src/config/mcp.rs`: JSON parsing, source precedence, validation, name derivation, and unit tests.
- [x] (2026-11-08) Add the project-trust entry for `.cake/mcp.json` and carry its exact bytes through the trust snapshot.
- [x] (2026-11-08) Add `src/clients/mcp/`: process spawn, dual-era handshake, JSON-RPC correlation, pagination, timeouts, shutdown, and unit plus fixture tests.
- [x] (2026-11-08) Add `src/clients/tools/mcp.rs`: tool entry adapter, content conversion, argument parsing, output caps, and unit tests.
- [x] (2026-11-08) Wire discovery into `src/main.rs` and registration into `src/clients/agent.rs`, `src/cli/session_factory.rs`, and `src/prompts/mod.rs`.
- [x] (2026-11-08) Add `cake debug mcp` text and JSON output.
- [x] (2026-11-08) Add `tests/mcp.rs`: global and project config, both eras, dispatch, read-only exclusion, trust, and explicit-file failure.
- [x] (2026-11-08) Update configuration, integrations, security, architecture, and README documentation.
- [x] (2026-11-08) Run focused tests, `just check`, and `just check-coverage`. Complete task 172 and archive this plan. The live Safari check is not runnable from this session's sandbox; the fixture covers both eras.

## Surprises & Discoveries

- Observation: the MCP standard is dual-era. Modern revisions (`2026-07-28` and later) carry protocol version, client identity, and capabilities in `_meta` on every request and have no session. Legacy revisions (`2025-11-25` and earlier) use an `initialize` request followed by a `notifications/initialized` notification. The stdio binding requires a dual-era client to send `server/discover` first and to fall back to `initialize` only for a non-modern error or a timeout. Evidence: the MCP specification pages `basic/versioning.md` and `basic/transports/stdio.md` for `2026-07-28`.
- Observation: Apple documents the Safari MCP server with the ecosystem `mcpServers` JSON shape and the command `/usr/bin/safaridriver --mcp`. Evidence: `developer.apple.com/documentation/safari-developer-tools/connecting-an-ai-agent-to-safari`.
- Observation: Cake already has a provider-name limit convention in `config::toolbox`: 64 characters including the `tb__` prefix. MCP names must fit `mcp__<server>__<tool>` in the same budget.
- Observation: `serde_json` accepts an empty array for a fully defaulted struct, so a malformed explicit MCP config test must use invalid JSON, not `[]`. Evidence: the initial `malformed_explicit_file_is_an_error` test parsed `[]` successfully and needed a `{` fixture instead.
- Observation: `debug mcp` against the fixture shows `2025-11-25` for the legacy server and `2026-07-28` for the modern server, and the modern fixture rejects `tools/list` without `_meta`. Evidence: the fixture run in `Artifacts and Notes`.
- Observation: the real Safari smoke test cannot run inside this session's Cake sandbox. `/usr/bin/safaridriver --mcp` exits with SIGSEGV and `/System/Cryptexes/App/usr/bin` reads return `Operation not permitted` from the Bash tool. Evidence: `defaults read` reports Safari exists on macOS 27.2, but the driver is a cryptex symlink the Seatbelt profile cannot reach. The fixture server covers both protocol eras instead; a real Safari check needs a Cake run whose sandbox admits the Safari cryptex and a user who enabled remote automation.

## Decision Log

- Decision: support both protocol eras rather than legacy only. Rationale: Safari 27 uses the modern revision, and the specification requires the probe for a dual-era client. Date/Author: 2026-11-08, ADR 055.
- Decision: use the ecosystem `mcpServers` JSON shape instead of a TOML `[mcp]` settings section. Rationale: users already have that file for other agents, and Apple documents it. Date/Author: 2026-11-08, ADR 055.
- Decision: project `.cake/mcp.json` is authority-bearing and trust-gated. Rationale: it starts arbitrary executables with Cake's ambient authority. Date/Author: 2026-11-08, ADR 055.
- Decision: capture the tool list at startup and log `notifications/tools/list_changed`. Rationale: a live registry mutation would change the model-visible tool set between turns and complicate session replay. Date/Author: 2026-11-08, ADR 055.

## Outcomes & Retrospective

Delivered. Cake now connects to stdio MCP servers from the ecosystem `mcpServers` JSON shape, negotiates both protocol eras, registers tools as `mcp__<server>__<tool>`, executes calls through one JSON-RPC connection, and reports the inventory through `cake debug mcp`. Project `.cake/mcp.json` is part of the trust snapshot, and the read-only policy starts no server.

The focused suites and the coverage gate pass. The only unverified path is a live Safari run: the Safari driver lives in a cryptex that this session's Seatbelt profile cannot read, so the driver exits with SIGSEGV before the handshake. The fixture server covers both the modern and legacy wire paths, including `_meta` enforcement and pagination.

Lessons: the modern MCP revision is not an incremental change to `initialize`; the `server/discover` probe is the only portable way to select an era. The `serde_json` acceptance of an empty array for a fully defaulted struct was a surprise worth an explicit top-level shape check. Keeping the live connection out of the prompt formatter made the prompt tests independent of processes.

## Changes after initial plan

- Added a top-level JSON-object check for config files after discovering that `serde_json` accepted `[]` for a fully defaulted struct.
- Filtered explicit `--mcp-config` paths inside a checkout's `.cake/` tree under `--no-project-config`, mirroring the toolbox protection.
- Bounded MCP image count and size, and skipped cross-server registered-name collisions.
- Extracted `run_mcp` helpers to satisfy the CRAP gate without adding unit tests that would print to stdout.

## Context and Orientation

Cake is a Rust binary crate. Its tools live in `src/clients/tools/mod.rs` in a `ToolRegistry`. Each `ToolEntry` pairs a model-facing `Tool` definition with an async executor closure. Capabilities declared at registration decide whether a tool is read-safe, whether its arguments get conservative JSON repair, whether it is replay-safe, and whether it needs image support. The default registry is `default_tool_registry()`.

Toolbox tools are the closest existing analogue. `src/config/toolbox.rs` discovers trusted executables and parses their `describe` output. `src/clients/tools/toolbox.rs` turns one parsed tool into a `ToolEntry`. `Agent::with_toolbox_tools` in `src/clients/agent.rs` adds those entries after the session id is known. `load_run_resources` in `src/main.rs` performs discovery and stores the parsed tools on `RunResources`.

The system prompt lists tools through `format_tool_list_section` in `src/clients/tools/mod.rs`. It receives the toolbox tools so their names and descriptions appear next to built-ins. `[tools].enabled` filters the registry and the prompt by exact registered name.

Project `.cake/` configuration is trust-gated by `src/config/trust.rs`. `scan_project_cake` walks the tree, hashes every regular file into a manifest, and classifies authority-bearing files. `ProjectAuthority` in `src/config/settings.rs` names authority-bearing settings keys. Adding a new authority-bearing file means adding it to the trust scan, the authority report, and the symlink rejection list.

An MCP server is an external process that speaks JSON-RPC 2.0 over stdin and stdout. One line is one JSON object. A request has `jsonrpc`, `id`, `method`, and optional `params`; a response has the same `id` plus `result` or `error`. A notification has no `id`. `tools/list` returns tool descriptors with `name`, `description`, `inputSchema`, and optional `outputSchema`. `tools/call` takes a tool name and an argument object and returns `content` blocks plus optional `structuredContent` and `isError`. Image content blocks carry base64 `data` and a `mimeType`.

## Plan of Work

Create `src/config/mcp.rs`. It defines the on-disk shape, a resolved server definition, and a loader. The loader reads the global file `<config>/cake/mcp.json`, then the trusted project file `.cake/mcp.json`, then each `--mcp-config` path, merging by server name with later sources winning. It returns warnings for unreadable or malformed auto-discovered files and a typed error for an explicit path that is missing or malformed. It validates server names, commands, arguments, environment names and values, and the per-server timeout. It computes `mcp__<server>__<tool>` names with provider-safe characters and a hash suffix when truncation is required.

Add `.cake/mcp.json` to `src/config/trust.rs` as an authority-bearing file, including the symlink rejection and the authority report. Carry its exact bytes through the snapshot so the file that is hashed is the file that is parsed, matching the settings and hook behavior.

Create `src/clients/mcp/mod.rs` and `src/clients/mcp/client.rs`. The process spawns the configured command with the session working directory, `AGENT=cake`, `AI_AGENT=cake`, inherited environment plus configured `env`, piped stdio, its own process group, and `kill_on_drop`. A writer task serializes frames. A reader task correlates responses by id, answers `ping` and `roots/list`, and rejects unsupported server requests. The handshake probes `server/discover` with modern `_meta`, selects `2026-07-28` when advertised, and otherwise falls back to `initialize` with `2025-11-25`. Tool listing follows `nextCursor` with a bounded page count.

Create `src/clients/tools/mcp.rs`. It turns one discovered tool into a `ToolEntry` whose executor forwards an argument object and converts content blocks. Text is joined; images become inline `ImagePart`s when the model and size permit; resources become readable text; structured content is serialized when there is no text; `isError` becomes a `ToolError`; output is capped with a truncation compensation event.

Wire discovery into `load_run_resources`, carry the servers on `RunResources`, pass them to the prompt builder and session factory, and register them with a new `Agent::with_mcp_servers`. Skip discovery entirely under the read-only sandbox. Add `cake debug mcp` to `src/cli/debug.rs`, with a diagnostic document when `--json` is set.

## Concrete Steps

All commands run from `/Users/travisennis/Projects/cake/cake`.

Focused tests:

```
cargo test config::mcp
cargo test clients::mcp
cargo test debug_mcp
```

The full gate:

```
just check
```

Manual inspection with a fixture server:

```
cake debug mcp
```

A real Safari check on a machine with Safari 27 and remote automation enabled:

```
mkdir -p ~/.config/cake
cat > ~/.config/cake/mcp.json <<'JSON'
{
  "mcpServers": {
    "safari-mcp": {
      "command": "/usr/bin/safaridriver",
      "args": ["--mcp"]
    }
  }
}
JSON
cake debug mcp
```

Expected: a `safari-mcp` entry with a ready status and the tool names from Apple's table, including `list_tabs`, `navigate_to_url`, and `screenshot`.

## Validation and Acceptance

`cargo test config::mcp` proves parsing, precedence, validation, and name generation. `cargo test clients::mcp` proves the handshake, JSON-RPC correlation, pagination, content conversion, timeouts, and shutdown against a fixture server. `cargo test debug_mcp` proves the diagnostic document.

Behavioral acceptance: with a configured fixture server, `cake debug mcp` prints its tools; a prompt run includes the MCP tool names in the system prompt and can call one; removing the server executable produces a warning and the run still starts with the remaining tools; `.cake/mcp.json` in an untrusted checkout exits 3 before any server spawns; `--sandbox read-only` starts no server.

## Idempotence and Recovery

Reading and connecting are repeatable. Each run starts fresh server processes and kills them on drop; a leftover process after a crash is bounded by its own process group and the operating system. `--mcp-config` files are read-only inputs. Editing `mcp.json` is always safe; the next run picks it up. An invalid project file changes the trust snapshot and requires `cake trust` again, which is the intended fail-closed behavior.

## Artifacts and Notes

`cargo test mcp` passes 48 focused tests (config, client, adapter, prompt listing, and debug report), and `cargo test --test mcp` passes the eight drivable end-to-end cases. A fixture `debug mcp` run produced:

```
MCP servers

fixture-legacy (ready)
  protocol: 2025-11-25
  server: fake-legacy
  tools (4):
    - echo -> mcp__fixture-legacy__echo
    ...

fixture-modern (ready)
  protocol: 2026-07-28
  server: fake-modern
  tools (4):
    ...

broken (failed)
  error: MCP server 'broken' failed to start '/nonexistent/server': ...
```

The end-to-end test session records `mcp__fixture__echo`, the prompt line `- **mcp__fixture__echo**: MCP server 'fixture': Echo a message.`, and the tool output `echo: hello`, proving registration, prompt listing, and dispatch.

## Interfaces and Dependencies

- `crate::config::mcp::{McpServerConfig, McpConfigLoad, discover_mcp_servers, registered_tool_name}` owns configuration discovery and naming.
- `crate::clients::mcp::{McpServer, McpTool, connect_server}` owns the process and JSON-RPC client.
- `crate::clients::tools::mcp::mcp_tool_entry` owns the `ToolEntry` adapter.
- `crate::clients::agent::Agent::with_mcp_servers` registers discovered tools.
- `crate::cli::debug` owns `cake debug mcp`.
- No new crate dependencies. The client uses `tokio`, `serde_json`, `sha2`, `hex`, and `base64`, all already present.

## Revision Notes

- 2026-11-08: Initial plan, created with task 172 and ADR 055.
