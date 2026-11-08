---
status: accepted
date: 2026-11-08
decision-makers: Travis Ennis
informed: task 172
---

# Use a dual-era stdio MCP client configured by mcp.json

## Context and Problem Statement

Cake can extend its tool surface only through compiled built-ins or the toolbox protocol. The Model Context Protocol has become the common way for external tools --- including Safari 27's developer automation --- to describe and invoke operations. Cake needs to act as an MCP client so a user can add a server without writing a wrapper executable per tool.

The protocol now has two eras. Legacy revisions, up to `2025-11-25`, establish a session with an `initialize` handshake. Modern revisions, starting with `2026-07-28`, carry the protocol version, client identity, and client capabilities in `_meta` on every request and have no session state. The stdio binding specifies how a dual-era client must probe with `server/discover` and fall back to `initialize` only when the probe returns a non-modern error or times out.

The configuration surface also has an ecosystem convention: a JSON file whose `mcpServers` object maps a server name to `command`, `args`, and `env`. Apple, Claude Code, Codex, and other clients document that shape.

## Decision Drivers

- Let a user add a server in the format the ecosystem already documents.
- Work with both the stable legacy servers in use today and Safari 27's modern server.
- Run servers as trusted executables with the same trust story as hooks, toolbox tools, and gates.
- Keep the model-visible tool surface predictable: names, schemas, errors, and read-only behavior must match Cake's existing tool contracts.
- Fail closed for project configuration and for the read-only sandbox.
- Avoid a second protocol implementation per front end; the client belongs in core.

## Considered Options

1. **A dual-era stdio client in Cake with `mcp.json` configuration.** Chosen. It matches the protocol's compatibility guidance and the configuration shape users already have.
2. **Legacy `initialize` only.** Simpler, but a modern-only server such as a future Safari release would fail, and the spec already requires a probe for dual-era clients.
3. **An external toolbox gateway.** The toolbox protocol is one executable per tool, so a gateway would expose a single opaque `mcp` tool or need its own dynamic schema mechanism. It also duplicates trust and discovery. Rejected.
4. **HTTP transport first.** Broader reach, but it adds network trust, OAuth, and session management before the local stdio case works. Deferred.

## Decision Outcome

Chosen option 1.

Cake is a dual-era MCP client over stdio. At startup it spawns each configured server, probes `server/discover`, selects the modern `2026-07-28` protocol when the server advertises it, and otherwise falls back to the legacy `initialize` handshake with `2025-11-25`. It then lists tools, registering each as `mcp__<server>__<tool>`.

Server definitions are the ecosystem `mcpServers` JSON shape. Cake reads `~/.config/cake/mcp.json`, a trusted project `.cake/mcp.json`, and repeatable `--mcp-config <path>` files; later sources override earlier servers by name. `cake debug mcp` connects and reports the discovered tools without running an agent turn.

- Project `.cake/mcp.json` is authority-bearing. An untrusted project fails closed before any server starts, and `--no-project-config` skips the file.
- MCP servers are trusted executables outside the Bash sandbox, like toolbox tools. The read-only sandbox starts none of them.
- A server that fails to start or handshake is skipped with a warning; other servers and tools remain available.
- The tool list is captured at startup. `notifications/tools/list_changed` is logged but does not mutate a live registry.
- Tool names are sanitized to the provider-safe set and truncated with a deterministic hash suffix to stay within the 64-character function-name limit.
- Text, image, audio, resource, resource-link, and structured content are mapped to Cake's tool result shape; `isError` results become model-visible tool errors.
- The protocol surface in this decision is tools only. Resources, prompts, sampling, elicitation, roots, tasks, extensions, HTTP transports, and OAuth are not implemented.

### Consequences

- Good: a user can add Safari, or any stdio server, with a JSON entry and no Cake-specific wrapper.
- Good: the compatibility probe means one client implementation covers current legacy servers and the modern revision.
- Good: trust, sandbox filtering, hook payloads, and session records reuse the existing tool paths instead of adding a second execution surface.
- Bad: every configured server is started at the beginning of every invocation, even when no MCP tool is called. A slow `npx` server delays startup.
- Bad: the tool list is static for the invocation; a server that changes its tools requires a new run.
- Bad: a server process runs with Cake's ambient environment and authority. Only trusted configuration should be loaded.
- Neutral: MCP tool calls are ordinary tool calls in the transcript and telemetry, so existing analysis works without a new record type.

## More Information

The wire and failure semantics are in [Integrations](../integrations.md#mcp-client-protocol), configuration in [Configuration](../configuration.md#mcp-servers), and the trust boundary in [Security](../security.md#trusted-extensions). Implementation and verification are tracked by the MCP client task.
