---
status: accepted
date: 2026-11-10
decision-makers: Travis Ennis
informed: task 181
---

# Refresh MCP tools when the server reports a change

## Context and Problem Statement

ADR 055 chose a startup tool snapshot for the stdio MCP client and logged `notifications/tools/list_changed` without acting on it. That was correct for servers with a fixed surface: the model-visible tool set could not change between turns, and every run started from one deterministic inventory.

Some servers do not have a fixed surface. A browser automation server may publish page tools only after a window exists, or withdraw them when the user closes it. The modern `2026-07-28` protocol also changed the delivery rule: servers send list-change notifications only to clients that opened a `subscriptions/listen` stream with `toolsListChanged: true`. A restart-only client misses the change in both eras and cannot use the server for the current run. Logging the notification is not client support.

The decision must preserve three existing contracts: provider requests never change mid-request, hooks and gates still see every tool call, and the session format does not grow a new record just to describe a tool inventory change.

## Decision Drivers

- Let a user run a stateful MCP server without restarting Cake after each browser window or page transition.
- Implement the protocol's delivery rules for both eras rather than only the legacy bare notification.
- Keep the model-visible tool set deterministic within one provider request.
- Preserve the existing trust boundary: an MCP server is already a trusted, unsandboxed executable, and every call still passes the normal tool path.
- Avoid new persisted state and avoid changing existing session records.

## Considered Options

1. **Refresh between turns.** Chosen. The client counts change notifications; the agent re-lists only servers whose count advanced, reconciles the registry and deferred catalog, and sends the new surface on the next provider request.
2. **Keep the startup snapshot.** Simpler, and it is the current behavior, but it does not satisfy servers that require list-change handling and cannot expose a state-dependent tool.
3. **Restart the server process on a notification.** Rejected. A restart loses the server's state, which is usually the reason its tool list changed, and the handshake cost lands in the middle of a task.
4. **Change the tool list during a provider request.** Rejected. A request must describe one coherent tool set; requests already in flight cannot gain or lose definitions.

## Decision Outcome

Chosen option 1.

The client records each accepted `notifications/tools/list_changed` in a generation counter. A legacy server's bare notification increments it directly. A modern server is sent one `subscriptions/listen` request carrying `toolsListChanged: true` after the handshake; notifications carrying that subscription id increment the counter, and an omitted acknowledgment or a rejected subscription is logged without failing the connection.

Before each provider request, the agent compares each server's generation with the value it last observed. A server whose value advanced is re-listed through every `tools/list` page. The agent then reconciles the registry: added tools are registered, removed tools are removed, changed definitions are replaced, and the deferred `ToolSearch` catalog is rebuilt. The request carries the new surface; tools already requested in the previous turn have already run. A failed re-list keeps the previous registry and logs a warning.

The system prompt keeps the startup server summary and direct-tool list. The request's tool array is the authority for what the model can call, and a `deferred` server's row counts remain startup data until the next run.

### Consequences

- Good: a server can add or remove tools mid-run and Cake uses them on the next turn, including via `ToolSearch` for deferred exposure.
- Good: both protocol eras are supported; a modern server sees the required subscription, and a legacy server keeps working without one.
- Good: no new session record, configuration key, or wire message shape. The change is additive to the existing MCP contract.
- Bad: a trusted server can change the model-visible tool surface without a restart. The server is already trusted, hooks and gates still govern calls, and the `read-only` policy still starts no server, but a prompt-cache prefix can miss when definitions change.
- Bad: the system prompt's MCP server row and direct-tool descriptions can lag the live request surface until the next invocation.
- Neutral: a server that never reports a change performs no extra `tools/list` calls.

## More Information

This decision partially supersedes ADR 055's "tool list is captured at startup" bullet and its "a server that changes its tools requires a new run" consequence. ADR 055's transport, configuration, trust, and naming decisions remain in force. The wire and failure semantics are in [Integrations](../integrations.md#mcp-client-protocol); configuration is in [Configuration](../configuration.md#mcp-servers).
