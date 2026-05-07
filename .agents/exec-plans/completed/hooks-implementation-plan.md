# Add command hooks to cake

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agents/PLANS.md`.

## Purpose / Big Picture

After this change, a user can configure small local programs that run at important points in a `cake` session. The first useful behavior is policy and automation around tool calls: a project can block dangerous Bash commands before they run, record an audit trail after tools finish, add context when a session starts, and run cleanup or alerting hooks when a task stops or errors.

The observable outcome is that a user can add `.cake/hooks.json`, run `cake "..."`, and see the configured hook receive a JSON payload on stdin. For a `PreToolUse` hook, the script can return a denial and `cake` will not execute the tool; instead the denial reason is returned to the model as the tool result. For non-blocking lifecycle hooks, a script can write to a log file and `cake` continues normally.

## Progress

- [x] 2026-05-04 Wrote the initial ExecPlan and scoped it to command hooks, JSON configuration, stdin/stdout protocol, tool-call decisions, lifecycle hook points, tests, and documentation.
- [x] 2026-05-04 Reviewed the plan, resolved 15 ambiguities, and revised the plan to specify concurrency, observability, schema validation, role compatibility, and recovery rules.
- [x] 2026-05-04 Implemented hook configuration loading from global, project, and local JSON hook files in `src/config/hooks.rs`, including version checks, event validation, matcher rules, timeout clamping, and load-order-preserving aggregation.
- [x] 2026-05-04 Implemented the command hook runner in `src/hooks.rs`, including stdin JSON payloads, shell command execution, timeout handling, exit-code/JSON decision parsing, fail-open/fail-closed behavior, tracing events, and session JSONL `hook_event` audit records.
- [x] 2026-05-04 Integrated hooks into `Agent::send` and the CLI lifecycle in `src/clients/agent.rs` and `src/main.rs`, including `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `Stop`, and `ErrorOccurred`.
- [x] 2026-05-04 Added focused loader and runner unit tests, a wiremock-backed `Agent::send` regression test for `PreToolUse` denial, hook documentation, `.cake/hooks.json.example`, and a pointer from `.cake/settings.toml.example`.
- [x] 2026-05-04 Ran `cargo test` outside the sandbox after the sandbox blocked wiremock port binding; result after adding the agent hook regression test: 467 unit tests, 8 `exit_codes` tests, and 7 `stdin_handling` tests passed.
- [x] 2026-05-04 Ran `just ci`; result: rust toolchain check passed, `cargo fmt -- --check` passed, strict clippy passed, 467 unit tests plus integration suites passed, import lint passed, and the recipe printed `All checks passed!`.

## Surprises & Discoveries

- Observation: The research file now lives at `.agents/.research/topics/hooks.md`; during the project rename, forward-looking references were updated to `cake`.
  Evidence: `Cargo.toml` has `name = "cake"`, and the CLI entrypoint is `src/main.rs`.

- Observation: `cake` already has a single tool-dispatch boundary that can host tool hooks for Bash, Read, Edit, and Write.
  Evidence: `src/clients/tools/mod.rs` exposes `execute_tool(name, arguments) -> Result<ToolResult, String>`, and `src/clients/agent.rs` calls it indirectly through `execute_tool_with_skill_dedup` at `agent.rs:467`.

- Observation: Tool calls are currently executed concurrently, so blocking pre-hooks must be applied before spawning the tool futures.
  Evidence: `Agent::send` builds `function_calls`, then maps them to async futures and awaits `futures::future::join_all` at `agent.rs:478`.

- Observation: Settings are TOML, but the hooks research consistently recommends JSON hook files.
  Evidence: `src/config/settings.rs` models strongly typed TOML; researched systems use JSON for hooks.

- Observation: Tool failure is already typed at the boundary. No string-sniffing of `"Error:"` is required.
  Evidence: `execute_tool(...) -> Result<ToolResult, String>`. `Ok` means success, `Err(String)` means failure with a reason. The agent loop converts both into a `FunctionCallOutput`'s `output` string today; the hook layer can branch on the `Result` variant before that conversion.

- Observation: The `Role::Developer` variant already exists and is supported by both the Responses and Chat Completions backends.
  Evidence: `src/clients/types.rs` defines `Role::Developer`. `src/clients/responses.rs:290` has `extract_instructions_keeps_developer_messages_in_input` confirming developer messages survive in the Responses API input array. The Chat Completions request builder at `src/clients/chat_completions.rs` uses the same `Role` enum, so the same variant is forwarded as a `developer` message to OpenAI-compatible chat endpoints.

- Observation: `DataDir::session_path(id)` already returns the transcript path; no new helper is required.
  Evidence: `src/config/data_dir.rs:117` defines `pub fn session_path(&self, id: uuid::Uuid) -> PathBuf`. `main.rs` already knows the session UUID before constructing the agent, so it can call `data_dir.session_path(session.id)` to obtain the transcript path.

- Observation: Tool input from the model is not validated against a JSON schema before execution. Each tool implementation deserializes the JSON via serde and rejects with `Err(String)` on shape errors; Bash also calls `validate_command_safety`, and path tools call `validate_path` / `validate_path_for_write`.
  Evidence: `src/clients/tools/mod.rs:316-339` dispatches by name; `src/clients/tools/bash_safety.rs:14` declares `validate_command_safety`. There is no JSON Schema validator wired into the tool boundary, so `updated_input` from a hook does not need a separate schema check; it must only be a JSON object string and is then subject to the same per-tool serde + safety validation as model-provided args.

- Observation: The normal sandbox denies local port binding for wiremock-based tests, but the test suite passes when run outside the sandbox.
  Evidence: `cargo test` inside the sandbox failed with `Failed to bind an OS port for a mock server.: Operation not permitted`; rerunning `cargo test` with elevated permissions passed all 467 unit tests plus the integration suites.

## Decision Log

- Decision: Implement command hooks first, and defer HTTP hooks, prompt hooks, agent hooks, permission-rule syntax, and remote policy services.
  Rationale: Command hooks cover the immediate local automation and safety use cases, fit the current CLI architecture, and can be tested without introducing another LLM call path or network dependency.
  Date/Author: 2026-05-04 / Codex

- Decision: Store hooks in JSON files named `hooks.json` rather than inside `.cake/settings.toml`.
  Rationale: The researched systems converge on JSON for hooks, hook payloads are JSON, and separate hook files keep the existing settings loader focused on model and behavior settings.
  Date/Author: 2026-05-04 / Codex

- Decision: Load hooks from `~/.config/cake/hooks.json`, `.cake/hooks.json`, and `.cake/hooks.local.json`, in that order.
  Rationale: This mirrors cake's existing global/project split while adding a high-precedence local file for machine-specific or secret-bearing hooks. The local file should be documented as gitignored by convention.
  Date/Author: 2026-05-04 / Codex

- Decision: Run hooks outside the OS tool sandbox by default.
  Rationale: Hooks are user-configured extensions of the CLI, not model-requested tools. They often need to inspect policy files, write audit logs, or call local programs that are intentionally outside the tool sandbox.
  Date/Author: 2026-05-04 / Codex

- Decision: Use a small matcher language for the first implementation: absent or `"*"` matches everything; otherwise split on `|` and match exact event source names. The `matcher` field is only meaningful for events that have a source: `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, and `SessionStart`.
  Rationale: This supports the examples in the research note without adding a regex dependency or inventing a larger permission language. For events without a source (`UserPromptSubmit`, `Stop`, `ErrorOccurred`), a matcher is not meaningful and is rejected at load time so users notice the misconfiguration immediately.
  Date/Author: 2026-05-04 / Codex / Trav

- Decision: Treat `exit 2` as a block, `exit 0` as success with optional JSON output, and all other exit codes as non-blocking hook errors unless the hook sets `fail_closed: true`.
  Rationale: This matches the common behavior across the researched tools while preserving a clear opt-in for security-sensitive hooks.
  Date/Author: 2026-05-04 / Codex

- Decision: Hooks within a single event run concurrently. Pre-tool hooks across multiple tool calls also run concurrently. Sequentially threading `updated_input` across multiple matched hooks for the same event is out of scope for the first implementation.
  Rationale: cake already executes tool calls concurrently via `futures::future::join_all`. Forcing sequential hook ordering would either degrade tool concurrency or introduce a more complex execution scheduler. Concurrent hooks keep the implementation simple and predictable. If two hooks both return `updated_input` for the same call, this is a configuration error: the first one (in load order) wins and a warning is logged. If two hooks both return a denial, any denial blocks the call. If any hook returns `additional_context`, all such strings are appended in load order.
  Date/Author: 2026-05-04 / Trav

- Decision: Keep `Stop` and `ErrorOccurred` invocation in `src/main.rs`, after `client.send(msg).await` resolves. Because cake is not a REPL and exits when the agent loop exits, the outer `send` boundary is the natural stop point.
  Rationale: Putting these hooks in `main.rs` keeps `Agent` independent from process-lifecycle concerns and matches the user-observable contract: "Stop runs once per cake invocation that completed". A future REPL mode could move this into `Agent` if needed; the current architecture does not require it.
  Date/Author: 2026-05-04 / Trav

- Decision: Use a typed tool result for hook classification rather than sniffing for the string `"Error:"`. Pre-tool hooks fire before tool execution; post-tool hooks dispatch on the `Result<ToolResult, String>` variant: `Ok` triggers `PostToolUse`, `Err` triggers `PostToolUseFailure`.
  Rationale: `execute_tool` already returns a typed `Result`. Branching on the variant is robust to future changes in tool error formatting and avoids false positives when a successful tool emits text containing the substring `"Error:"`.
  Date/Author: 2026-05-04 / Trav

- Decision: When `updated_input` is provided, validate that it parses as a JSON object and then pass it through the same per-tool deserialization and safety checks (serde shape, `validate_command_safety` for Bash, `validate_path` for path tools). If those checks fail, the resulting error is returned to the model as the tool result with a prefix indicating the hook altered the input.
  Rationale: Tool inputs are not currently schema-validated before dispatch; each tool's deserializer is the single source of truth. Reusing it for hook-supplied input avoids drift and ensures the sandbox still applies. The model is told the input was modified so that it does not interpret a downstream error as its own bug.
  Date/Author: 2026-05-04 / Trav

- Decision: When `updated_input` is applied, prepend a short notice such as `Hook updated tool input. Original arguments: {original_json}\nNew arguments: {new_json}\n---\n` to the tool result string returned to the model. Always include this notice on success or failure of the modified call.
  Rationale: The model needs to know its requested arguments were rewritten so it can interpret the tool output and any error correctly. Embedding both original and modified JSON inline is the simplest approach that keeps the existing `FunctionCallOutput` shape unchanged.
  Date/Author: 2026-05-04 / Trav

- Decision: When `fail_closed` is true and a non-tool lifecycle hook (`SessionStart`, `UserPromptSubmit`, `Stop`, `ErrorOccurred`) errors or returns invalid output, the CLI exits with a non-zero status before continuing. For `PreToolUse`, fail-closed converts to a tool-call block as before. This produces consistent "do not continue" semantics across all events.
  Rationale: A `fail_closed` hook's whole point is to prevent the next action when the policy script malfunctions. For tool calls, the next action is a single tool invocation; for lifecycle events, the next action is the rest of the cake run. Exiting with non-zero communicates this clearly to shell scripts, CI, and the user. We will revisit if real use surfaces a better behavior (for example, downgrading `Stop` failures to a warning), but consistent exit-on-fail-closed is the correct starting point.
  Date/Author: 2026-05-04 / Trav

- Decision: Inject hook-added context as `Role::Developer` messages in the conversation history.
  Rationale: Both backends already accept this role: the Responses backend keeps developer messages in the input array (`responses.rs:290`), and the Chat Completions backend forwards the same `Role` enum to OpenAI-compatible endpoints. Using `Role::Developer` keeps hook-injected context distinct from the user's prompt and from the system prompt.
  Date/Author: 2026-05-04 / Trav

- Decision: Bake the hook configuration `version` field into the loader. The first implementation accepts `version: 1`. Any other value, missing or otherwise, is a load-time error that names the file path. Unknown event names are rejected for the same reason. Unknown fields inside known events are ignored to allow forward-compatible additions.
  Rationale: Schema versioning is cheap to enforce now and prevents silent misinterpretation later when fields are added or renamed. Rejecting unknown events catches typos like `"PreToolUSe"` that would otherwise silently disable a hook. Ignoring unknown sub-fields lets later cake versions add new hook options (for example, `description`, `tags`) without breaking older binaries that still understand the core schema.
  Date/Author: 2026-05-04 / Trav

- Decision: Defer trust-on-first-use, allowlists, and per-project hook confirmation prompts. Hooks from `.cake/hooks.json` execute when present.
  Rationale: cake is currently single-user. A robust trust model is valuable but is not a blocker. This is recorded explicitly so a future plan can address it.
  Date/Author: 2026-05-04 / Trav

- Decision: Hook scripts must be executable by the OS, and hook commands always run with the cake project root as their `cwd`. cake does not support resuming a session from a different working directory than the session was started in.
  Rationale: Predictable `cwd` makes relative `command` paths safe and matches cake's existing project-rooted behavior. Resume across directories has never been supported and is documented here so hook authors can rely on a stable working directory.
  Date/Author: 2026-05-04 / Trav

- Decision: Hook activity (start, exit code, decision, duration, stderr length) is written to the session JSONL transcript and emitted as `tracing` events under the `cake::hooks` target. Hook stdout and stderr are captured to memory bounded at 64 KiB each; bytes beyond the cap are truncated with a note.
  Rationale: Without a transcript record, post-hoc debugging using the `session-investigation` skill cannot see hook activity. Writing a typed record per hook invocation lets us reuse the existing investigation tooling. Bounded capture prevents a runaway hook from consuming unbounded memory.
  Date/Author: 2026-05-04 / Trav

## Outcomes & Retrospective

Implemented a minimal but complete command hook system. Users can now configure JSON hook files, run command hooks for session and tool lifecycle events, block or rewrite tool calls before execution, add hook context to model-visible messages or tool outputs, and inspect hook activity in tracing logs and session JSONL records. The implementation preserved concurrent tool execution and passed `just ci`.

## Context and Orientation

`cake` is a Rust 2024 binary CLI. The command-line entrypoint is `src/main.rs`. The main agent orchestration type is `Agent` in `src/clients/agent.rs`. `Agent::send` accepts one user message, sends provider requests, executes model-requested tools, appends tool outputs to conversation history, and loops until the model returns a final assistant message.

A "hook" in this plan means a user-configured program that `cake` starts at a lifecycle event. A "lifecycle event" is a named point in the CLI run, such as `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Stop`, or `ErrorOccurred`. A "command hook" means a hook where `cake` starts a local process, sends JSON to the process on stdin, waits for it to exit, and optionally reads JSON from stdout. A "blocking hook" means a hook that can prevent an action, such as a tool call, from continuing.

The current tool boundary is `src/clients/tools/mod.rs`. That module defines the tool structs sent to the provider and the internal async function `execute_tool(name, arguments) -> Result<ToolResult, String>`. The tool arguments are JSON strings. Individual tools live in `src/clients/tools/bash.rs`, `read.rs`, `edit.rs`, and `write.rs`. The Bash tool is already sandboxed with macOS Seatbelt or Linux Landlock where supported; filesystem tools validate paths before reading or writing.

`ToolResult` is currently `pub struct ToolResult { pub output: String }`. Failure is signaled by the outer `Result::Err(String)` from `execute_tool`. The agent loop converts both into a `FunctionCallOutput`'s `output` string. Hook classification (`PostToolUse` vs `PostToolUseFailure`) reads the `Result` variant directly; the agent must not convert to a string before classification.

Session persistence lives in `src/config/session.rs` and is wired from `src/main.rs` through `Agent::with_persist_callback`. The transcript path is reachable via `DataDir::session_path(session.id)` (`src/config/data_dir.rs:117`). When `--no-session` is used, no session file is created, and `transcript_path` in hook payloads is `null`.

Settings are loaded from TOML by `src/config/settings.rs`. Global settings live at `~/.config/cake/settings.toml`, project settings live at `.cake/settings.toml`, and profile overlays can change some behavior. Hooks are not added to these TOML structs. Instead, add a dedicated JSON hook loader beside the existing config modules.

The CLI uses `anyhow` for application-level errors, `thiserror` for custom errors, `serde` and `serde_json` for JSON, `tokio` for async process and timeout handling, and `tracing` for logs. No new dependency is required for the initial matcher language.

cake does not support resuming a session from a different working directory than the session was started in. Hooks run with the project root as their working directory.

## Hook Configuration Format

The first implementation reads JSON files from these paths:

    ~/.config/cake/hooks.json
    <project>/.cake/hooks.json
    <project>/.cake/hooks.local.json

If a file is missing, it is ignored. If a file exists but is malformed, `cake` returns a clear configuration error before starting the model request, naming the offending file. Hooks from all files are appended in load order: global first, project second, local third. This lets project and local hooks add behavior without copying global hooks. The plan does not support deleting or overriding lower-precedence hooks yet.

The file shape is:

    {
      "version": 1,
      "hooks": {
        "SessionStart": [
          {
            "matcher": "startup|resume|fork",
            "hooks": [
              {
                "type": "command",
                "command": "./.cake/hooks/session-start.sh",
                "timeout": 10,
                "fail_closed": false,
                "status_message": "Loading session hook"
              }
            ]
          }
        ],
        "PreToolUse": [
          {
            "matcher": "Bash|Write",
            "hooks": [
              {
                "type": "command",
                "command": "./.cake/hooks/check-tool.sh",
                "timeout": 5,
                "fail_closed": true
              }
            ]
          }
        ],
        "PostToolUse": [
          {
            "matcher": "*",
            "hooks": [
              {
                "type": "command",
                "command": "./.cake/hooks/audit.sh",
                "timeout": 5
              }
            ]
          }
        ]
      }
    }

`version` must be exactly `1`; any other value is a load-time error.

Supported event names in the first implementation are `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `Stop`, and `ErrorOccurred`. Unknown event names are rejected at load time with a message naming the hook file and event. Unknown fields inside known events are ignored by serde so future config additions do not break older binaries, except for each command hook's required fields (`type`, `command`).

Each event entry has an optional `matcher` and a required `hooks` array. The `matcher` field is only meaningful for events with a source: `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, and `SessionStart`. Including a `matcher` for `UserPromptSubmit`, `Stop`, or `ErrorOccurred` is rejected at load time.

Each item in `hooks` must currently have `"type": "command"` and a `command` string. `timeout` is in seconds and defaults to `60`. Clamp `timeout` to a minimum of `1` and a maximum of `600` seconds. `fail_closed` defaults to `false`. `status_message` is optional and is only logged in the first implementation.

The matcher language is intentionally small. If `matcher` is missing or equals `"*"`, it matches every source for that event. Otherwise split on `|`, trim whitespace, and require exact equality. For `PreToolUse`, `PostToolUse`, and `PostToolUseFailure`, the source is the tool name such as `Bash`, `Read`, `Edit`, or `Write`. For `SessionStart`, the source is `startup` for a new session, `resume` for a continued session, and `fork` for a forked session.

Hook scripts must be executable by the OS (`chmod +x ./.cake/hooks/your-hook.sh` on Unix). cake does not chmod hook files for you. Hook commands always run with the cake project root as their working directory; use relative paths such as `./.cake/hooks/check-tool.sh` confidently, knowing that resume from another directory is not supported.

## Hook Runtime Protocol

Every command hook runs with the current project directory as its working directory. The `command` string is executed through the platform shell so users can write normal shell snippets. On Unix, use `sh -c <command>`. On Windows, use `cmd /C <command>`. This command string comes from trusted user configuration, not from model output. Do not interpolate tool arguments into the command line; send all runtime data through stdin JSON.

The common input payload sent to stdin is:

    {
      "version": 1,
      "session_id": "550e8400-e29b-41d4-a716-446655440000",
      "task_id": "550e8400-e29b-41d4-a716-446655440001",
      "transcript_path": "/Users/example/.local/share/cake/sessions/....jsonl",
      "cwd": "/path/to/project",
      "hook_event_name": "PreToolUse",
      "model": "glm-5.1",
      "timestamp": "2026-05-04T00:00:00Z"
    }

`SessionStart` adds:

    {
      "source": "startup",
      "initial_prompt": "the user prompt text"
    }

`UserPromptSubmit` adds:

    {
      "prompt": "the user prompt text"
    }

`PreToolUse` adds:

    {
      "tool_name": "Bash",
      "tool_use_id": "call_abc123",
      "tool_input": { "command": "cargo test" },
      "tool_input_json": "{\"command\":\"cargo test\"}"
    }

`PostToolUse` and `PostToolUseFailure` add:

    {
      "tool_name": "Bash",
      "tool_use_id": "call_abc123",
      "tool_input": { "command": "cargo test" },
      "tool_input_json": "{\"command\":\"cargo test\"}",
      "tool_result": {
        "result_type": "success",
        "text_result_for_llm": "... tool output ..."
      }
    }

The `result_type` field is `"success"` for `PostToolUse` and `"failure"` for `PostToolUseFailure`. The classification is read from the `Result<ToolResult, String>` variant returned by `execute_tool`: `Ok(_)` is success, `Err(_)` is failure. The plan does not sniff the output text.

`Stop` adds:

    {
      "result": "final assistant response text, if available"
    }

`ErrorOccurred` adds:

    {
      "error": {
        "message": "error text",
        "name": "Error"
      }
    }

When a hook exits with code `0`, parse stdout as JSON only if stdout is non-empty after trimming whitespace. Empty stdout means no decision. Invalid JSON is a hook error. If `fail_closed` is false, log the invalid output and continue. If `fail_closed` is true, convert to a tool-call block for `PreToolUse` and to a CLI exit with non-zero status for any other event.

When a hook exits with code `2`, block the action. For `PreToolUse`, do not execute the tool. Return a tool output string to the model that starts with `Hook blocked tool execution:` and includes stderr if present, otherwise the parsed `reason` if present. For other events, treat exit `2` as a request to stop the task and return an `anyhow` error with the hook's reason. With `fail_closed: true` on a non-tool event, exit the CLI with a non-zero status.

When a hook exits with any other code, treat it as a non-blocking hook error unless `fail_closed` is true. Log the exit code and stderr with `tracing::warn!` under the `cake::hooks` target. With `fail_closed: true`, exit the CLI with a non-zero status.

Supported JSON output fields are:

    {
      "continue": true,
      "stop_reason": "optional message",
      "decision": "allow",
      "permission": "allow",
      "reason": "optional explanation",
      "updated_input": { "command": "modified command" },
      "additional_context": "message to add to the model context",
      "suppress_output": false
    }

For compatibility with the research findings, accept either `permission` or `decision`, with values `allow`, `deny`, `block`, or `ask`. Treat `deny` and `block` the same. Treat `ask` as deny in the first implementation and include a reason saying interactive ask is not supported yet. If `continue` is explicitly false, stop the task with `stop_reason` or `reason`.

For `PreToolUse`, support `updated_input` by replacing the tool argument JSON before execution. The replacement must parse as a JSON object; if it does not, treat as a hook error governed by `fail_closed`. After substitution, the new arguments are passed through the target tool's normal serde deserialization and safety validation (`validate_command_safety` for Bash, `validate_path` / `validate_path_for_write` for path tools). If validation fails, return the validation error as the tool result. When `updated_input` is applied, prepend the following notice to the tool result string returned to the model:

    Hook updated tool input.
    Original arguments: {original_json}
    New arguments: {new_json}
    ---
    {actual tool output or error}

For `UserPromptSubmit` and `SessionStart`, support `additional_context` by appending a `Role::Developer` message to the agent history before the next provider request. For `PostToolUse` and `PostToolUseFailure`, support `additional_context` by appending it to the tool result text under a heading `Additional hook context:`. When multiple matched hooks each return `additional_context`, append all of them in load order separated by blank lines. When multiple matched hooks each return `updated_input` for the same `PreToolUse` call, the first one in load order wins and a `tracing::warn!` is emitted naming the conflicting source files. When any matched hook returns a denial, the call is denied; combined denial reasons are joined with `; `. Defer `suppress_output` behavior; parse the field but do not act on it.

## Concurrency and Ordering

Hook ordering is intentionally simple. The rules are:

1. Within a single event, all matched hooks run concurrently. Their decisions are aggregated after all complete (or time out).
2. `PreToolUse` hooks for different tool calls also run concurrently. Each call collects its own decisions independently.
3. Allowed tool calls then execute concurrently using the existing `futures::future::join_all` path.
4. After tool execution, `PostToolUse` and `PostToolUseFailure` hooks for each call run concurrently with each other and with hooks for other calls.
5. `SessionStart` and `UserPromptSubmit` run before the first provider request. `Stop` runs after `Agent::send` resolves successfully. `ErrorOccurred` runs after `Agent::send` returns an error.

Sequentially threading `updated_input` through multiple matched `PreToolUse` hooks is out of scope for the first implementation. If two hooks both modify the same tool call, the first hook in load order wins and a warning is logged. This keeps the implementation simple and predictable; sequential composition can be added later without breaking the JSON schema.

## Observability

Without observability, this feature cannot be debugged when a hook script behaves unexpectedly. Three layers exist:

1. **Tracing**. Every hook invocation emits a `tracing` event under the `cake::hooks` target with structured fields: `event` (e.g. `PreToolUse`), `source` (tool name or session source), `command` (the configured command string), `source_file` (path of the hook config that defined this hook), `exit_code`, `duration_ms`, `stderr_bytes`, `stdout_bytes`, `decision` (one of `allow`, `deny`, `error`, `timeout`, `none`), and `fail_closed`. Event level is `info` for routine outcomes, `warn` for non-blocking errors, and `error` for fail-closed errors. Operators can raise log verbosity with the existing `cake.YYYY-MM-DD.log` rotation in `~/.cache/cake/`.

2. **Session JSONL transcript**. When sessions are enabled, each hook invocation writes one record to the transcript. The record reuses the existing transcript writer (`open_session_for_append` in `src/config/data_dir.rs`) so the `session-investigation` skill can read hook activity without code changes. The record shape is:

       {
         "type": "hook_event",
         "timestamp": "2026-05-04T00:00:00Z",
         "task_id": "...",
         "event": "PreToolUse",
         "source": "Bash",
         "source_file": ".cake/hooks.json",
         "command": "./.cake/hooks/check-tool.sh",
         "exit_code": 0,
         "duration_ms": 42,
         "decision": "allow",
         "fail_closed": false,
         "stdout": "...",
         "stderr": "..."
       }

   Stdout and stderr are captured to memory bounded at 64 KiB each. Bytes beyond the cap are truncated with the suffix `... (truncated, N more bytes)`. The session writer truncates stored stdout/stderr further if necessary to stay within the existing record-size limits in `src/config/session.rs`.

3. **Failure context for the model**. When a `PreToolUse` hook denies a call, the tool result the model receives starts with `Hook blocked tool execution:` and includes the denial reason and the source file path. This makes it visible inside the conversation transcript, which helps when debugging via session investigation.

The acceptance criteria for observability are:

- Running `cake "..."` with a known-good hook produces one transcript record per hook invocation and one tracing event per invocation.
- A failing hook with `fail_closed: true` produces an `error`-level tracing event, a transcript record, and a non-zero exit status.
- The `session-investigation` skill can locate hook records in the transcript and report their decisions and durations without code changes.

## Plan of Work

The first milestone is configuration loading. Create `src/config/hooks.rs` and export the needed types from `src/config/mod.rs`. Define `HookEvent`, `HookFile`, `HookMatcherConfig`, `HookCommand`, `LoadedHooks`, and `HooksError`. Use serde to parse JSON. Add a `HooksLoader` type with `load(project_dir: &Path) -> Result<LoadedHooks, HooksError>`. Keep the loader independent from model settings so malformed hook files can produce focused hook errors. Reject `version != 1`, unknown events, and matchers on non-source events. Add unit tests that create temporary global and project directories, verify missing files are ignored, verify malformed JSON errors include the path, verify hook order is global then project then local, verify matcher behavior, and verify rejection of unsupported configurations.

The second milestone is the hook runner. Create `src/hooks.rs` (top-level so both `main.rs` and `Agent` can use it without awkward `clients` -> `config` cycles). Define a `HookRunner` that owns `LoadedHooks`, a `HookContext` (project `cwd`, transcript path, model name, session id, task id), and an optional handle to the session writer for transcript records. Define `HookInput` and event-specific payload helpers. Implement `run_event` for lifecycle events and `run_pre_tool`, `run_post_tool`, and `run_error`. Use `tokio::process::Command`, `Stdio::piped`, and `tokio::time::timeout`. Ensure the child process stdin is closed after writing the JSON payload so scripts waiting for EOF do not hang. Cap captured stdout and stderr at 64 KiB each. Emit tracing events and write transcript records on every invocation. Add unit tests using small shell commands that read stdin, emit JSON, exit `2`, time out, and emit invalid JSON.

The third milestone is integrating hooks with CLI setup. In `src/main.rs`, after settings load and before building or running the `Agent`, call the hook loader with `current_dir`. Compute the transcript path with `data_dir.session_path(session.id)` when session persistence is enabled, otherwise pass `None`. Construct `HookRunner` after the session ID and task ID are known. Run `SessionStart` before `client.emit_prompt_context_records()` and `client.emit_task_start_record()`, because session-start context must become part of the prompt context before the first provider request. Run `UserPromptSubmit` before `client.send(msg)` and after session-start context has been added.

The fourth milestone is integrating hooks into `Agent`. Add an optional `HookRunner` field to `Agent` via `Agent::with_hook_runner(runner: Arc<HookRunner>) -> Self`. Extend `Agent::send` so it can append hook-added developer messages before the first model turn. Before executing tool futures, iterate over the collected function calls in order and run `PreToolUse` for each call concurrently (one hook batch per call, all calls concurrent). This produces a list of tool execution plans: execute with original arguments, execute with updated arguments (with the prepended notice), or skip with a blocked output string. Then execute only the allowed tool calls concurrently as today. Branch on the `Result<ToolResult, String>` variant after each tool future resolves: `Ok` runs `PostToolUse`, `Err` runs `PostToolUseFailure`. Append any hook-provided additional context to the tool output before adding `ConversationItem::FunctionCallOutput` to history. Preserve result ordering by keeping the existing call order when building final outputs.

The fifth milestone is stop and error lifecycle behavior. In `src/main.rs`, after `client.send(msg).await` resolves successfully and before `emit_task_complete_record`, run `Stop` with the final assistant message text. If `Stop` returns `additional_context`, log it; do not auto-continue the conversation in the first implementation. If `client.send` returns an error, run `ErrorOccurred` before emitting the task-complete error record. If a `Stop` or `ErrorOccurred` hook itself fails open, log it and keep the original outcome. If it fails closed, exit the CLI with a non-zero status; the original error (if any) is logged but the exit reflects the hook failure because the user explicitly configured that behavior.

The sixth milestone is documentation and examples. Add a new `docs/design-docs/hooks.md` that explains config locations, event names, matcher behavior, input payloads, output decisions, exit codes, timeouts, observability (transcript records and tracing target), and security expectations. Link it from `docs/design-docs/index.md`. Add a short commented hooks section to `.cake/settings.toml.example` that points to `.cake/hooks.json`. Add an example `.cake/hooks.json.example` with one blocking Bash policy hook and one PostToolUse audit hook. Document the `chmod +x` requirement and the project-root `cwd`.

The final milestone is verification. Add unit tests for config parsing and runner semantics, then add integration-style tests around `Agent` with a fake provider. Finish with `cargo fmt` and `just ci`.

## Testing Approach

The repository's testing pattern is:

- **Unit tests** live next to the code they test in `#[cfg(test)] mod tests`. Examples include `src/clients/responses.rs` (`extract_instructions_*`), `src/clients/tools/bash_safety.rs` (`validate_command_safety` cases), and `src/clients/types.rs` (snapshot tests via `insta`).
- **Snapshot tests** use the `insta` crate. Snapshots live in `src/clients/snapshots/`. New snapshot tests must follow the same naming convention `cake__<module>__tests__<test_name>.snap`.
- **Integration tests** live in `tests/`. The current files are `tests/exit_codes.rs`, `tests/stdin_handling.rs`, and the helpers in `tests/support/`. They invoke the `cake` binary with `assert_cmd` (or equivalent in `tests/support/`) and verify exit code, stdout, and stderr.

For hook tests:

- **Config loader unit tests** live in `src/config/hooks.rs::tests`. Use `tempfile::TempDir` to construct a fake `~/.config/cake` directory and a project directory; set a test override for the global config path or use a pure function that takes both paths. Verify missing files, malformed JSON, version mismatch, unknown events, and matcher rules.
- **Hook runner unit tests** live in `src/hooks.rs::tests`. Use `#[cfg(unix)]` for tests that depend on `sh -c`. Provide a Windows test variant for `cmd /C` behavior; if Windows-specific tests are difficult, document that the runner is exercised under unix only in CI and the Windows path is verified by hand.
- **Agent integration tests** use the same fake provider mechanism as existing `Agent::send` tests. Inspect `agent.rs` for the existing test harness around `complete_turn` to identify the fake transport. Tests must use a `HookRunner` constructed against an in-memory `LoadedHooks` (not a JSON file) so they do not depend on filesystem layout. Each test:
  1. Builds a tiny in-memory `LoadedHooks` with one or two hook entries that point to a temporary `sh -c` snippet.
  2. Constructs `Agent` with a fake transport that returns a fixed sequence of model responses, including tool calls.
  3. Drives `Agent::send` and inspects the `history` after completion.
  Required tests:
  - `pre_tool_hook_denies_tool_execution`: hook exits 2; verify the tool was not run (e.g., the script that the tool would execute writes to a sentinel file that must not exist) and the `FunctionCallOutput` text starts with `Hook blocked tool execution:`.
  - `pre_tool_hook_updated_input_changes_arguments`: hook returns `{"permission":"allow","updated_input":{"command":"printf safe"}}`; verify the recorded tool output contains `safe` and the `Hook updated tool input.` notice with both original and new arguments.
  - `pre_tool_hook_updated_input_invalid_returns_validation_error`: hook returns `updated_input` that the tool's serde rejects; verify the tool result is the validation error message with the `Hook updated tool input.` notice prepended.
  - `post_tool_hook_additional_context_reaches_next_turn`: hook returns `{"additional_context":"please run the formatter"}`; verify the next provider request body (captured by the fake transport) contains both the original tool output and the `Additional hook context: please run the formatter` line.
  - `post_tool_hook_failure_fires_only_on_err_variant`: a tool that returns `Err` triggers `PostToolUseFailure` with `result_type: "failure"` and does not trigger `PostToolUse`. A tool that returns `Ok` does the opposite. Verify with a hook script that records the event name to a sentinel file.
  - `hook_errors_fail_open_by_default`: hook exits 1; verify the run completes normally and emits a `tracing::warn!` event under `cake::hooks`.
  - `fail_closed_lifecycle_hook_exits_nonzero`: a `SessionStart` hook with `fail_closed: true` exits 1; verify cake exits non-zero. This is best validated as an integration test in `tests/exit_codes.rs` so the actual process exit status is observed.
  - `concurrent_pre_tool_hooks_are_aggregated`: two matched hooks run concurrently; one allows, one denies; verify the call is denied. Two matched hooks both providing `additional_context` produce both strings in load order. Two matched hooks both providing `updated_input` log a warning and use the first.
  - `transcript_contains_hook_records`: when sessions are enabled, verify a `hook_event` record appears in the JSONL transcript per invocation.

## Concrete Steps

Run all commands from the repository root, `/Users/travisennis/Projects/cake`.

Start with a baseline:

    cargo test hooks
    cargo test agent

The first command may report that there are no hook tests yet. That is acceptable before implementation. The second command should pass before edits and gives confidence that agent loop tests are healthy.

After adding `src/config/hooks.rs`, run:

    cargo test config::hooks

Expected result:

    test config::hooks::tests::missing_hook_files_load_empty ... ok
    test config::hooks::tests::loads_global_project_and_local_in_order ... ok
    test config::hooks::tests::matcher_pipe_syntax_matches_exact_names ... ok
    test config::hooks::tests::rejects_matcher_on_non_source_events ... ok
    test config::hooks::tests::rejects_unknown_event_names ... ok
    test config::hooks::tests::rejects_version_other_than_1 ... ok
    test result: ok. N passed; 0 failed

After adding the hook runner, run:

    cargo test hooks::tests

Expected result:

    test hooks::tests::command_hook_receives_stdin_json ... ok
    test hooks::tests::exit_two_blocks_pre_tool_use ... ok
    test hooks::tests::invalid_json_fails_open_by_default ... ok
    test hooks::tests::fail_closed_invalid_json_blocks ... ok
    test hooks::tests::timeout_is_reported_as_hook_error ... ok
    test hooks::tests::stdout_is_capped_at_64_kib ... ok

After integrating with `Agent`, run focused tests:

    cargo test pre_tool_hook_denies_tool_execution
    cargo test pre_tool_hook_updated_input_changes_arguments
    cargo test pre_tool_hook_updated_input_invalid_returns_validation_error
    cargo test post_tool_hook_additional_context_reaches_next_turn
    cargo test post_tool_hook_failure_fires_only_on_err_variant
    cargo test hook_errors_fail_open_by_default
    cargo test concurrent_pre_tool_hooks_are_aggregated
    cargo test transcript_contains_hook_records

After integration tests are added, run:

    cargo test --test exit_codes fail_closed_lifecycle_hook_exits_nonzero

After documentation and examples are added, run:

    cargo fmt
    just ci

The expected final result is that formatting, clippy, and the full test suite complete successfully. Record the exact `just ci` result in `Progress` and `Outcomes & Retrospective`.

## Validation and Acceptance

The implementation is acceptable when all of the following behaviors are demonstrable.

A project can create `.cake/hooks.json` with a `PreToolUse` hook matching `Bash` and a command script that exits `2` when the payload's `tool_input.command` contains `rm -rf`. When a model requests that Bash command, the Bash tool is not invoked. The tool result added to the conversation says the hook blocked execution and includes the hook reason.

A `PreToolUse` hook can return:

    {
      "permission": "allow",
      "updated_input": { "command": "printf safe" }
    }

When the model requested `printf unsafe`, the tool output proves that `printf safe` ran instead, and the `FunctionCallOutput` text begins with the `Hook updated tool input.` notice including both the original and new JSON.

A `PostToolUse` hook can return:

    {
      "additional_context": "The command produced a generated file that should be inspected."
    }

The next provider request includes the original tool output plus an `Additional hook context:` section containing that text.

A malformed hook script exits `1` by default and does not stop the task. The same hook with `fail_closed: true` causes cake to exit non-zero for any non-tool event, and to block the call for `PreToolUse`.

`SessionStart` and `UserPromptSubmit` hooks can add `Role::Developer` context before the first provider request. This is proven by a test that inspects the mock provider request body and finds the hook-added developer message.

`Stop` hooks run after a final assistant message. `ErrorOccurred` hooks run when the agent call returns an error. Both are proven with scripts that append a line to a temporary log file.

When sessions are enabled, the JSONL transcript contains one `hook_event` record per hook invocation, including event, source, source_file, exit_code, duration_ms, decision, and stdout/stderr (truncated as needed).

All new tests pass, existing tests pass, and `just ci` passes.

## Idempotence and Recovery

This work is additive. If hook loading is implemented but the runner is incomplete, hook files can remain absent and `cake` should behave exactly as before. If a hook file is malformed, the user can remove or fix only that hook file; no session or settings migration is involved.

Hook scripts run with bounded timeouts. If a test hangs, first inspect whether the runner closes child stdin after writing the payload. If shell-specific tests fail on a platform, keep the core runner tests platform-gated with `#[cfg(unix)]` or provide a Windows `cmd /C` equivalent.

Do not make hook execution part of the tool sandbox in this plan. If a later security review requires sandboxed hooks, add it as a separate feature with explicit configuration and tests.

If `updated_input` creates invalid tool arguments, return the per-tool validation error as the tool result rather than panicking or falling back to the original arguments. The hook explicitly changed the arguments and the model must see the result of that change, prepended with the `Hook updated tool input.` notice so the change is visible.

Hook scripts must be executable. cake does not chmod hook files; if a script is not executable, the platform shell will report an error and the hook is treated as a non-blocking error (or fail-closed, per configuration). cake does not support resuming a session from a different working directory than the session was started in; hook commands therefore can rely on the project root as a stable `cwd`.

## Artifacts and Notes

The current tool execution flow in `src/clients/agent.rs` (around line 458) is the primary insertion point:

    let futures = function_calls
        .iter()
        .map(|(_id, call_id, name, arguments)| {
            ...
            async move {
                let result = execute_tool_with_skill_dedup(
                    &name,
                    &arguments,
                    &skill_locations,
                    &activated_skills,
                )
                .await;
                (call_id, result)
            }
        });

    let results = futures::future::join_all(futures).await;

The hook implementation changes this in two stages. First, run pre-tool hooks (concurrently across calls, concurrently within each call) and build an execution plan per call: execute, execute-with-updated-input, or skip-with-blocked-output. Second, run the allowed executions concurrently and merge blocked outputs back into the same ordered result list. Branch on the `Result<ToolResult, String>` variant to drive `PostToolUse` vs `PostToolUseFailure`.

An example blocking script for Unix tests can be as small as:

    #!/bin/sh
    payload="$(cat)"
    case "$payload" in
      *"rm -rf"*)
        echo "destructive command blocked" >&2
        exit 2
        ;;
      *)
        printf '{"permission":"allow"}'
        ;;
    esac

An example hook output that injects context is:

    {"additional_context":"Prefer running the formatter after this edit."}

## Interfaces and Dependencies

In `src/config/hooks.rs`, define these public types and functions:

    pub struct LoadedHooks {
        pub groups: Vec<HookGroup>,
    }

    pub struct HookGroup {
        pub source_path: PathBuf,
        pub event: HookEvent,
        pub matcher: HookMatcher,
        pub hooks: Vec<HookCommand>,
    }

    pub enum HookEvent {
        SessionStart,
        UserPromptSubmit,
        PreToolUse,
        PostToolUse,
        PostToolUseFailure,
        Stop,
        ErrorOccurred,
    }

    pub struct HookCommand {
        pub command: String,
        pub timeout: Duration,
        pub fail_closed: bool,
        pub status_message: Option<String>,
        pub source_dir: PathBuf,
    }

    pub struct HooksLoader;

    impl HooksLoader {
        pub fn load(project_dir: &Path) -> Result<LoadedHooks, HooksError>;
    }

In `src/hooks.rs`, define the runtime API:

    pub struct HookRunner { ... }

    pub struct HookContext {
        pub session_id: uuid::Uuid,
        pub task_id: uuid::Uuid,
        pub transcript_path: Option<PathBuf>,
        pub cwd: PathBuf,
        pub model: String,
    }

    pub enum ToolHookPlan {
        Execute { arguments: String, prefix_notice: Option<String>, additional_context: Vec<String> },
        Block { reason: String, additional_context: Vec<String> },
    }

    pub enum HookPermission {
        Allow,
        Deny,
    }

    impl HookRunner {
        pub fn new(loaded: LoadedHooks, context: HookContext) -> Self;
        pub async fn session_start(&self, source: &str, initial_prompt: &str) -> anyhow::Result<Vec<String>>;
        pub async fn user_prompt_submit(&self, prompt: &str) -> anyhow::Result<Vec<String>>;
        pub async fn pre_tool_use(&self, tool_name: &str, tool_use_id: &str, arguments: &str) -> anyhow::Result<ToolHookPlan>;
        pub async fn post_tool_use(&self, tool_name: &str, tool_use_id: &str, arguments: &str, result: &Result<String, String>) -> anyhow::Result<Option<String>>;
        pub async fn stop(&self, result: Option<&str>) -> anyhow::Result<Option<String>>;
        pub async fn error_occurred(&self, error: &anyhow::Error) -> anyhow::Result<()>;
    }

`session_start` and `user_prompt_submit` return developer-context strings that `Agent` should append before the first provider request. `pre_tool_use` returns a plan that distinguishes execute (with optional updated arguments and a notice to prepend) from block (with a reason). `post_tool_use` accepts the typed `Result` so it can dispatch on success vs failure without string sniffing, and returns optional extra context to append to the tool output. `stop` and `error_occurred` are lifecycle side effects in the first implementation.

No new crate is required for the initial design. Use existing `serde`, `serde_json`, `tokio`, `uuid`, `chrono`, `anyhow`, `thiserror`, and `tracing`.

## Revision Notes

- 2026-05-04 / Codex: Created the initial self-contained plan after reading the hook research and current cake architecture. The plan deliberately chooses command hooks, JSON hook files, simple exact-match filters, and direct integration with `Agent::send` because those choices provide a useful first implementation with limited architectural risk.
- 2026-05-04 / Trav (review pass): Revised the plan to address 15 areas of weakness identified in review. Specifically: (1) added a Concurrency and Ordering section pinning down hook execution as fully concurrent within and across events; (2) confirmed `Stop` and `ErrorOccurred` belong in `main.rs` because cake is not a REPL and recorded the rationale; (3) removed the "if necessary" hedge around transcript path plumbing by noting that `DataDir::session_path(id)` already returns it; (4) clarified that `matcher` is only meaningful for source-bearing events (`PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `SessionStart`) and is rejected at load time on others; (5) deferred trust model with explicit decision; (6) made `fail_closed` exit cake non-zero for non-tool events so behavior is uniform; (7) replaced string-sniffing of `"Error:"` with a typed branch on `Result<ToolResult, String>`; (8) defined `updated_input` validation as the same per-tool serde + safety checks the model's input goes through, and added a model-visible `Hook updated tool input.` notice; (9) added an Observability section requiring transcript records and a structured tracing target so the `session-investigation` skill can debug hooks; (10) deferred CLI discoverability subcommand; (11) filled in the testing approach using cake's existing `tests/`, `insta` snapshots, and inline `#[cfg(test)]` patterns; (12) recorded a `version`-handling rationale (reject other versions, reject unknown events, ignore unknown sub-fields); (13) confirmed `Role::Developer` works in both backends and chose it for hook context injection; (14) cleaned up the suspicious midnight UTC progress timestamps and tidied the `Outcomes & Retrospective` placeholder; (15) documented script executability, project-root `cwd`, and the no-cross-directory-resume constraint.
- 2026-05-04 / Codex (implementation): Executed the plan and updated the status, surprises, and retrospective with the concrete files changed and validation results. The only validation caveat is that sandboxed `cargo test` cannot bind wiremock ports, so the full test suite was rerun with approved elevated permissions before `just ci`.
- 2026-05-07 / Codex: Moved this completed ExecPlan from `.agents/.plans/` to `.agents/exec-plans/completed/` during the ExecPlan directory migration.
