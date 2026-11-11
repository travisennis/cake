## Refresh MCP tools when the server reports a list change

This ExecPlan is a living document, maintained per docs/workflow/exec-plans.md. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be kept current as work proceeds.

## Purpose / Big Picture

After this change, a Cake user can run a Model Context Protocol (MCP) server whose available tools change while Cake is running. One concrete case is a browser server that publishes page-inspection tools only after a window exists, or withdraws them when the user closes it. Today Cake reads the tool list once at startup, logs the server's `notifications/tools/list_changed` message, and never looks again, so those tools stay invisible for the rest of the run. The modern `2026-07-28` protocol makes this worse: a server sends that notification only to a client that has opened a `subscriptions/listen` stream and asked for `toolsListChanged`, and Cake never opens one.

The user-visible outcome is that the tool list the model receives on the next turn follows the server. Run Cake with a dynamic fixture server, call the fixture's `mutate` tool, and the provider request for the following turn contains the new tool and omits the removed one. `cake debug mcp` says how Cake watches for changes: `notifications` for a legacy server or `subscription` for a modern one. A server that never changes its list sees no extra traffic.

The trust story does not change. An MCP server is a trusted executable outside the Bash sandbox, whether it was configured by the user or by a trusted project. Every new tool call still passes the hook and gate path like any other tool call. A read-only sandbox still starts no server at all.

## Progress

- [x] (2026-11-10 19:05-05:00) Record task 181, ADR 060, and this plan. ADR 060 partially supersedes ADR 055's startup-snapshot bullet; 059 remains reserved for the live-sessions initiative.
- [x] (2026-11-10 19:35-05:00) Client: count accepted list-change notifications, open and correlate a modern `subscriptions/listen` stream, expose the generation, and support a re-list.
- [x] (2026-11-10 19:45-05:00) Agent: retain connected servers, refresh only after a generation change before each provider request, reconcile direct and deferred registrations, and keep the old surface on failure.
- [x] (2026-11-10 19:55-05:00) Fixture and tests: dynamic mode for both eras plus an end-to-end provider-request test.
- [x] (2026-11-10 20:05-05:00) Diagnostics and docs: `cake debug mcp` watch mode, integration, configuration, security, ADR, and task records.
- [x] (2026-11-10 21:05-05:00) Ran `cargo test clients::mcp` (17 passed), `cargo test clients::agent` (203 passed), `cargo test --test mcp` (14 passed), `just check`, and `just check-coverage` (93.86% total, CRAP and CC gates passed). Filled the retrospective and completed task 181.

## Surprises & Discoveries

- Observation: the two protocol eras deliver the same notification differently. A legacy server emits `notifications/tools/list_changed` with no subscription. A modern server MUST only send it on a `subscriptions/listen` stream, and the notification carries `io.modelcontextprotocol/subscriptionId` in `_meta`. Evidence: MCP specification `2025-11-25/server/tools.md` and `2026-07-28/basic/patterns/subscriptions.md`.
- Observation: a `subscriptions/listen` request is long-lived. Its JSON-RPC response arrives only when the subscription ends gracefully; normal delivery is through notifications. Waiting for that response like an ordinary request would hang until the per-request timeout. Evidence: the same subscription page, "Graceful Closure".
- Observation: Apple documents Safari 27's server as `/usr/bin/safaridriver --mcp`, which is a locally spawned stdio server. The local-client requirement is already satisfied by Cake's stdio transport; the missing piece is list-change support. Evidence: Apple's "Connecting an AI agent to Safari" page.
- Observation: the provider request's system prompt still contains the startup MCP tool list, so an end-to-end assertion must inspect the top-level `tools` array rather than search the whole request body. Evidence: the `tool_list_change_reaches_the_next_provider_request` test fails its removed-tool check if it searches the body.
- Observation: `safaridriver --mcp` still cannot run in this session. `/usr/bin/safaridriver --help` and the process itself produce no output and exit, matching the earlier task-172 note about the Safari cryptex and remote automation. The Python fixture remains the executable check.
- Observation: clippy rejects an `async fn` with no `.await`, so `subscribe_tools_list_changed` is a synchronous send. The response is handled by the reader task, not by the caller. Evidence: the `unused_async` lint in the first `just check` run.

## Decision Log

- Decision: refresh between provider turns, not during a request. Rationale: a request must carry one coherent tool set, and a tool call returned by the previous turn has already run. Date/Author: 2026-11-10, Travis Ennis (ADR 060).
- Decision: re-list only servers whose notification generation advanced. Rationale: a server that never reports a change must not pay an extra `tools/list` on every turn. Date/Author: 2026-11-10, ADR 060.
- Decision: open a `subscriptions/listen` stream for every modern server after the handshake, without blocking startup on the acknowledgment. Rationale: the stream is the modern protocol's delivery mechanism, and a missing acknowledgment is not fatal; the client still accepts any notification it receives. Date/Author: 2026-11-10, ADR 060.
- Decision: keep the startup system prompt unchanged. Rationale: mutating the conversation's system message mid-run would break prompt-cache stability and replay symmetry; the request's tools array is authoritative. Date/Author: 2026-11-10, ADR 060.
- Decision: preserve the previous registry if a re-list fails. Rationale: a transient server failure must not remove tools the model can still use, and the next notification retries. Date/Author: 2026-11-10, ADR 060.

## Outcomes & Retrospective

Delivered. A server that reports `notifications/tools/list_changed` now changes the tool set the model sees on the next turn, in both protocol eras. The legacy path accepts the bare notification; the modern path opens one `subscriptions/listen` stream with `toolsListChanged: true` and correlates notifications by subscription id. The agent re-lists only changed servers, reconciles direct registrations and the deferred `ToolSearch` catalog, and keeps the previous surface when a re-list fails. `cake debug mcp` reports `tool updates: notifications` or `subscription`.

The executable proof is the `tool_list_change_reaches_the_next_provider_request` integration test: the first provider turn calls the fixture's `mutate` tool, and the second request's top-level `tools` array contains `mcp__fixture__added`, omits `mcp__fixture__fail`, and carries the changed `echo` description. Focused client and agent tests cover both eras, no-change behavior, add/remove/change reconciliation for direct and deferred exposure, and a failed re-list.

Remaining: no real Safari smoke test. `/usr/bin/safaridriver --mcp` still exits without output in this session, matching the task-172 note about the Safari cryptex and remote automation. The Python fixture is the executable evidence; a real check needs a session whose sandbox admits the Safari cryptex and a user who enabled remote automation.

Lessons: the provider request's system prompt retains the startup MCP tool list, so end-to-end assertions must inspect the `tools` array rather than the whole body. The modern subscription's acknowledgment is asynchronous; blocking startup on it would add latency for no correctness gain, because the generation counter already catches a change that lands during startup.

## Context and Orientation

Cake is a Rust CLI coding assistant. `src/clients/mcp/` is its Model Context Protocol client over stdio. `src/clients/mcp/client.rs` owns the child process, the JSON-RPC transport (one JSON object per line on stdin/stdout), the dual-era handshake, and request timeouts. `src/clients/mcp/mod.rs` owns connection setup, `tools/list` pagination, and the `McpServer` value that carries the live client plus the startup tool snapshot.

`src/clients/agent.rs` builds the agent's tool registry. `Agent::with_mcp_servers` turns each `McpTool` into a registry entry (`mcp_tool_entry` in `src/clients/tools/mcp.rs`) for `direct` exposure, or puts it in `deferred_mcp_tools` for `deferred` exposure. Deferred tools are searchable through the built-in `ToolSearch` entry, which is rebuilt by `Agent::refresh_tool_search`; selecting one adds it to the registry and records an `mcp_tools_activated` session record. `src/clients/agent/agent_loop.rs` holds `Agent::send`, the loop that currently runs `enforce_limits`, asks the provider for a turn, and executes the returned tool calls.

An MCP server declares a `tools` capability. A legacy server's `initialize` result and a modern server's `server/discover` result carry `capabilities.tools.listChanged`; the fixture will declare it in dynamic mode. A server notification has no JSON-RPC `id`; a server request does. `handle_line` in `client.rs` routes those two shapes and responses separately.

A "generation" is a count of accepted change notifications for one server. It is not a version the server sends; the client increments it, and the agent remembers the last value it acted on. Comparing those two values answers "has anything changed since I last listed this server's tools?"

ExecPlan terms: a *provider request* is one HTTP call to the model backend carrying the system prompt, conversation, and tool definitions. A *turn* is one provider response plus any tool executions it requested. `direct` exposure registers a tool before the first request; `deferred` exposure keeps it in the `ToolSearch` catalog until the model asks for it; `hidden` exposure never starts the server.

## Plan of Work

Start in `src/clients/mcp/client.rs`. Add a generation counter (`AtomicU64`) and a set of open subscription ids (a `std::sync::Mutex<HashSet<u64>>`) to `Transport`. Add a method that allocates a request id, writes a frame, and returns the id without inserting a pending-response sender; use it to send `subscriptions/listen` with `params.notifications.toolsListChanged = true` and modern `_meta`. Insert the id into the subscription set. In `handle_notification`, treat a tools-list-change notification as accepted, incrementing the counter; accept the legacy bare form and the modern form whose `params._meta["io.modelcontextprotocol/subscriptionId"]` is one of our open ids. Log an acknowledgment notification and whether it honors `toolsListChanged`. In `handle_response`, recognize a response id belonging to a subscription before looking in the pending table: remove it, log a graceful close or a rejected subscription, and do not report "unrecognized response id". Expose
`tools_generation() -> u64` and `tool_list_change_mode()` on `McpClient`, with an enum `None`/`Notifications`/`Subscription`.

In `src/clients/mcp/mod.rs`, derive `PartialEq` for `McpTool`, give `McpServer` a `reload_tools()` method that re-runs `tools/list` and replaces the snapshot, and expose `tools_generation()` and the watch mode through `McpServer`. Change the private `list_tools` helper to take a server name instead of a full configuration so `reload_tools` can call it. In `connect_server`, send the modern subscription after the handshake and before the initial `tools/list`; log, but do not fail, a subscription error. Subscribing before listing ensures a change during startup is counted and refreshed rather than lost.

In `src/clients/agent.rs`, add `mcp_servers: Vec<McpServer>` and `mcp_tool_generations: HashMap<String, u64>` to `Agent`. `with_mcp_servers` stores clones of the connected servers and records their starting generations. Add an async `refresh_changed_mcp_tools` that finds servers whose current generation differs, clones one server, re-lists it, and on success stores the new snapshot and reconciles. On failure it logs a warning, keeps the old snapshot, and records the observed generation so the loop does not retry every turn.

Reconciliation compares the old and new `McpTool` lists by registered name. A removed tool is removed from the registry, from `deferred_mcp_tools`, and from `active_mcp_tools`. A changed `direct` tool is removed and re-registered. A changed `deferred` tool replaces its catalog entry; if it was active, its registry entry is replaced. A new `deferred` tool joins the catalog. Then call `refresh_tool_search` to rebuild `ToolSearch`. Change `register_deferred_tools_direct` to iterate a clone of the catalog instead of consuming it, and skip names already registered, so the fallback mode (no `ToolSearch` allowed, or the name is taken) remains correct after a refresh. Respect the `[tools].enabled` allowlist and the server's exposure for every added or changed tool.

In `src/clients/agent/agent_loop.rs`, call `self.refresh_changed_mcp_tools().await` inside the `send` loop, after `enforce_limits` and before the provider request. This is the only mutation point, so a tool set never changes inside one request or while that turn's tool calls are executing.

Extend `src/clients/mcp/fixtures/fake_mcp_server.py` with `FAKE_MCP_DYNAMIC=1`. In that mode, declare `capabilities.tools.listChanged`, append a `mutate` tool, handle `subscriptions/listen` for the modern era by recording the id and sending `notifications/subscriptions/acknowledged`, and make `mutate` send `notifications/tools/list_changed` (with the subscription id for modern) and change the pages so `fail` disappears and `added` appears. Keep the non-dynamic tool lists identical so existing tests stay valid.

Add a test-only fixture helper in `src/clients/mcp/mod.rs` so agent tests can connect the same server. In `src/clients/mcp/tests.rs`, cover a legacy notification and a modern subscription; assert the generation advances, `reload_tools` reports the change, `fail` is gone, and `added` is present. In `src/clients/agent/agent_tests.rs`, cover the direct and deferred reconciliation through `refresh_changed_mcp_tools`, including an active deferred tool whose definition changes and a removed active tool. In `tests/mcp.rs`, add an end-to-end run with dynamic direct exposure: the first provider turn calls `mcp__fixture__mutate`, and the second provider request must contain `mcp__fixture__added` and must not contain `mcp__fixture__fail`.

In `src/cli/debug.rs`, add a `tool_updates` field to `McpServerReport` and print it in the text report. In `docs/integrations.md`, replace the fixed-list paragraph and the fixed-list sentence with the two-era delivery and refresh timing. In `docs/configuration.md`, replace the restart note. In `docs/security.md`, state that a trusted server can change the model-visible tool surface mid-run and that hooks and gates still apply to every call. Mention the plan and ADR 060 in the task.

## Concrete Steps

All commands run from `/Users/travisennis/Projects/cake/cake`.

First confirm the current fixed-list behavior and the new tests failing:

```
cargo test clients::mcp -- --nocapture
```

Then implement the client and agent changes and rerun:

```
cargo test clients::mcp
cargo test clients::agent
cargo test --test mcp
```

Format and run the full gate:

```
cargo fmt
just check
```

The expected end-to-end evidence is a test transcript in which the dynamic fixture is called and the second recorded provider request body contains `mcp__fixture__added` and lacks `mcp__fixture__fail`. A manual check is:

```
cake debug mcp
```

with a dynamic server configured. The server's row should include `tool updates: subscription` (modern) or `tool updates: notifications` (legacy).

## Validation and Acceptance

Behavioral checks:

1. Configure the fixture with `"FAKE_MCP_ERA": "legacy"` and `"FAKE_MCP_DYNAMIC": "1"` in `mcp.json`, `exposure: direct`. Ask Cake to call `mcp__fixture__mutate`. The next turn's request carries `mcp__fixture__added` and not `mcp__fixture__fail`.
2. Repeat with `"FAKE_MCP_ERA": "modern"`. The fixture's stderr or a test assertion shows it received `subscriptions/listen` with `toolsListChanged: true` and answered with `notifications/subscriptions/acknowledged`; the same next-turn tool set appears.
3. With `exposure: deferred`, the prompt still advertises `ToolSearch`, and a search after the mutation finds `mcp__fixture__added` while `mcp__fixture__fail` is absent.
4. A server with `FAKE_MCP_DYNAMIC` unset performs exactly the startup `tools/list` pagination and no more.
5. Stop the fixture process after it sends a change but before Cake re-lists. Cake keeps the old tools and the run continues; the warning names the server and `tools/list`.

`cargo test clients::mcp`, `cargo test clients::agent`, `cargo test --test mcp`, and `just check` must pass. The real Safari check remains conditional on a sandbox and Safari configuration that can run `/usr/bin/safaridriver --mcp`.

## Idempotence and Recovery

The tests write only temporary directories and the fixture script under them; rerunning is safe. `cargo fmt` and `just check` are repeatable. If a server's re-list hangs, each request keeps the configured per-request timeout; the run logs and continues with the old surface. If a subscription is rejected, the client removes the id and still accepts any notification the server sends; no state is corrupted, and a new run tries again. If the diff is wrong, revert the agent refresh call first; the client changes are then inert.

## Artifacts and Notes

Expected test evidence, to be filled with actual transcript lines:

```
running 2 tests
test clients::mcp::tests::legacy_list_change_refreshes_tools ... ok
test clients::mcp::tests::modern_list_change_refreshes_tools ... ok

test mcp::dynamic_tool_list_change_reaches_the_next_request ... ok
```

The end-to-end assertion reads the second provider request body and checks for the added and removed registered names.

## Interfaces and Dependencies

By the end, these must exist:

- `crate::clients::mcp::client::McpClient::tools_generation(&self) -> u64`.
- `crate::clients::mcp::client::McpClient::tool_list_change_mode(&self) -> ToolListChangeMode`, with `ToolListChangeMode` re-exported from `crate::clients::mcp`.
- `crate::clients::mcp::McpServer::reload_tools(&mut self) -> impl Future<Output = Result<bool, String>>` (async method).
- `crate::clients::mcp::McpServer::tools_generation(&self) -> u64` and `tool_list_change_mode(&self) -> ToolListChangeMode`.
- `crate::clients::agent::Agent::refresh_changed_mcp_tools(&mut self) -> impl Future<Output = ()>` (async, task-internal).
- A test fixture mode selected by `FAKE_MCP_DYNAMIC=1`.

The implementation uses `serde_json`, `tokio`, and the existing `std::sync` atomics and mutexes; no new dependency is needed. The MCP subscription messages are plain JSON-RPC and reuse the existing writer loop and line reader.
