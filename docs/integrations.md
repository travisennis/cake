# Integration contracts

This document describes Cake's stable shell-facing and machine-facing contracts. Rust types and snapshots are the field-level serialization authority; this guide explains semantics, identity, ordering, and compatibility.

## Exit codes

  | Code | Meaning                                                     |
  | ---- | ----------------------------------------------------------- |
  | `0`  | successful task                                             |
  | `1`  | agent or tool execution error                               |
  | `2`  | authentication, rate-limit, or network error                |
  | `3`  | invalid input, flags, configuration, or missing credentials |

Human diagnostics go to stderr. Machine-readable stdout must contain only its declared JSON format.

`cake init` creates `.cake/settings.toml` (and, with `--hooks`, an inert `.cake/hooks.json.example`) and prints created files to stdout. An existing target is reported on stderr and exits `3` without writing anything.

For `stream-json`, validation failures before a task stream starts still use the codes above. Once streaming begins, ordinary agent, provider, and tool failures are represented by the final `task_complete` record and the process exits `0`. An unsatisfied output schema remains nonzero, and interruption exits `130`.

## Provider retries

Retries are bounded. Cake retries transport failures and HTTP `408`, `409`, `429`, `500`, `502`, `503`, `504`, and provider-overload signals such as `529` or a structured `overloaded_error`. An `x-should-retry: false` response header prevents a retry; `x-should-retry: true` additionally permits otherwise borderline `5xx` responses. Ordinary `400`, `401`, `403`, and `404` responses are not retried.

A parseable `Retry-After` value takes precedence over exponential backoff, but is capped by the active maximum backoff. Transport recovery temporarily disables idle connection reuse. A parseable context-window overflow may be retried once with reduced output and reasoning-token budgets when enough output space remains.

Cake also makes one zero-delay semantic continuation turn when a successful provider response has partial output but no final assistant message, unless the provider identifies non-retryable termination such as content filtering, failure, or refusal. The continuation stays in the same session and task, preserves usage and counters, asks only for the missing final answer, and offers no tools so completed tool work is not repeated. Text mode reports the `semantic_incomplete` retry on stderr; JSON and stream-JSON suppress it. If that turn is also incomplete, Cake emits the cut-off outcome once and includes an explicit `cake --resume <UUID> "try again"` command.

## Completion JSON

`--output-format json` emits one JSON object after the invocation completes. It contains the final result or error plus session metadata, usage, working directory, turn count, and elapsed time.

Progress and retry rendering are suppressed so stdout remains parseable. The exact serialized shape is protected by CLI tests; consumers should tolerate additional optional fields.

## Stream JSON

`--output-format stream-json` emits newline-delimited JSON records for the current invocation as events occur. It includes task boundaries, conversation items, hook events, and task completion. It does not emit session metadata, skill-activation metadata, or prior tasks.

A redirected stream is an event feed, not a resumable session file. Consumers should process records by their top-level `type`, preserve order, ignore unknown optional fields, and use `call_id` to associate tool calls, outputs, and hook events.

Malformed model tool arguments remain visible on the `function_call` record and produce a corresponding error output instead of making the stream invalid.

An automatic semantic continuation is visible as the provider's partial conversation records followed by one Cake-authored user continuation message. The invocation still emits exactly one final `task_complete`; recovery does not create another task boundary.

## Persisted sessions

Resumable sessions are flat `{session_id}.jsonl` files under `~/.local/share/cake/sessions/`, or under `{CAKE_DATA_DIR}/sessions/` when the data root is overridden.

The current format version is 4:

1. The first non-empty record is one `session_meta`.
2. Each invocation appends one `task_start`.
3. Conversation and metadata records are appended live.
4. The invocation ends with one `task_complete` when Cake can record an outcome.

Conversation records restored into model history are `message`, `function_call`, `function_call_output`, and `reasoning`. `session_meta`, `task_start`, `prompt_context`, `skill_activated`, `hook_event`, `turn_usage`, and `task_complete` are audit or lifecycle metadata and are not replayed as conversation. `turn_usage` records one normalized `usage` per completed API turn; Cake uses the most recent one to seed the agent's last-usage basis on continue, resume, and fork, so a resumed run knows its current context size before the first new provider request.

`--continue` selects the newest session whose header working directory matches the current directory. `--resume <UUID>` opens a specific session. `--fork [UUID]` creates a new session identity seeded with conversation records and prior `skill_activated` metadata from the selected parent. It does not copy parent session, task, prompt-context, hook, or completion metadata.

Session files are append-only and locked for one writer per invocation. Loading tolerates an interrupted final task without `task_complete`. Unsupported format versions fail explicitly; Cake does not silently rewrite older files.

An interrupted task can leave a `function_call` whose `function_call_output` was never written. Continue, resume, and fork close each such call by appending an ordinary `function_call_output` that records the call as not executed, so the restored history stays valid for providers. Repair appends only; prior bytes are never rewritten, and a history whose pairing is ambiguous fails with a diagnostic instead of being guessed at.

### Record semantics

- `session_meta`: version, session identity, creation context, tools, optional model/system prompt, and Git state.
- `task_start`: task identity and timestamp for one CLI invocation.
- `prompt_context`: mutable developer context used by that invocation.
- `message`: typed user, assistant, or tool text.
- `function_call` and `function_call_output`: provider tool request and result, joined by `call_id`.
- `reasoning`: provider reasoning data retained for round trips.
- `skill_activated`: first observed read of a known skill in a session.
- `hook_event`: hook execution, decision, timing, and bounded diagnostics.
- `task_complete`: outcome, duration, turns, tool-call count, result or error, usage, and optional permission denials.

Serialization snapshots under `src/types/snapshots/` provide canonical record examples.

For evidence-backed review of a persisted session, see the [Analyzing Cake Sessions runbook](runbooks/analyzing-cake-sessions/index.md). For reactive triage of a recent failed run, see the [Debugging Failed Cake Runs runbook](runbooks/debugging-cake.md).

## Telemetry sidecars

Operational telemetry lives under `~/.cache/cake/session-telemetry/{session_id}.ndjson` or the corresponding `CAKE_DATA_DIR` path. `invocation_id` separates invocations in one sidecar.

Records cover initialization, conversation and judge API attempts, retries, tools, and summaries. They omit prompts, assistant text, tool and provider response bodies, judge command/reason/cwd values, and credentials. An `api_attempt` may include provider-neutral `termination`; `retry_scheduled` reason `semantic_incomplete` marks the continuation above.

Each `judge_attempt` identifies the resolved model, API type, request controls, effective deadline, zero-tool/tool-choice state, prompt byte counts, attempt/retry ordinal, retry reason and backoff wait, terminal class, optional request identity digests (one-way SHA-256 of the provider request and originating tool call identifiers), usage, and termination. Timings separate construction, request through headers, response parsing, verdict parsing, and total time. Timeout and transport attempts retain elapsed time; absent usage is `null`. The record supplements the verdict or fail-closed compensation. One `judge_attempt` is emitted per provider call: one per evaluation, or two after a recovery.

A `compensation` carries its `kind`, optional `detail`, judge-verdict `latency_ms`, and allowlist `overridden` flag. Kinds are `json_repair`, `judge_verdict`, `judge_fail_closed`, `judge_bypass`, `same_path_serialization`, `output_truncation`, `context_overflow_retry`, and `edit_invalid_arguments`. Judge details are `block:<code>`, `warn:<code>`, or `allow`; fail-closed details name the failure class.

Consumers must tolerate added enum values and optional fields; old sidecars remain valid. Sidecars never drive continue, resume, fork, or session discovery.

## Bash tool and command-safety checks

The Bash tool's optional `reason` is the model's untrusted intent report; guidance directs agents to supply it for state-changing, destructive-looking, and remote-effect commands, stating the intended effect. A `reason` never authorizes a remote destructive command on its own, so a merge or branch delete requires an in-command guard. The judge weighs the reason against the command; it appears in transcripts, never telemetry. Each judge evaluation is stateless --- command, cwd, repository digest, and reason only --- so remediation recommends a self-contained guarded command the next request can judge alone.

Every non-empty Bash command runs through the LLM judge (ADR-018) before spawn. [Security](security.md) defines the gate semantics and [Configuration](configuration.md) the `[tools.bash.judge]` settings. A timeout or retryable transport/HTTP failure triggers at most one recovery within `timeout_secs + retry_budget_secs`; verdicts, refusals, and malformed verdicts are never retried, and exhausted recovery fails closed.

`cake bash check -- <command>` uses the preflight path and prompt without executing. Verdicts, allowlist overrides, and bypass exit `0`; timeout, network, auth, and rate-limit errors exit `2`; other transport errors, malformed verdicts, and refusals exit `1`; unknown judge models exit `3`.

`--diagnostic` also prints effective prompts, transformed request JSON, non-secret controls, zero-tool configuration, parsed response, usage, termination, timing, and verdict. Treat it as sensitive: prompts contain commands, paths, repository state, reasons, and possibly embedded secrets. Cake excludes its API key, authorization headers, and provider headers. It remains inspection-only.

## Hook protocol

Hook commands receive one versioned JSON object on stdin. Common fields include `session_id`, `task_id`, `transcript_path`, `cwd`, `hook_event_name`, `model`, and `timestamp`. Tool events also carry tool identity and input; post-tool events carry the result.

For tool events, `tool_input` is the parsed arguments object a `PreToolUse` decision gates: after Cake's conservative JSON repair pass for Edit, Write, and `tb__*` tools (the input those executors act on), strict otherwise. `tool_input_json` is the raw argument string as issued. `tool_input` is `{}` only when parsing fails; such a call fails rather than executing.

Exit status and stdout determine behavior:

- status `0` with empty output continues;
- status `0` with output parses one JSON decision;
- status `2` blocks;
- other failures are logged and ignored unless `fail_closed` applies before a result exists.

Decision JSON may use:

```json
{
  "continue": true,
  "permission": "allow",
  "reason": "optional explanation",
  "updated_input": {},
  "additional_context": "optional model context"
}
```

`continue: false` stops and has highest priority. `permission` takes priority over `decision`; `deny`, `block`, and `ask` block because Cake has no interactive ask flow. `PreToolUse` may return one `updated_input` object, which is revalidated by the tool. Any event may return `additional_context`.

Pre-request failures may abort the invocation. Post-result hooks are best-effort so they cannot replace an existing model or tool outcome. Hook stdout and stderr stored in events are bounded.

## Toolbox protocol

A toolbox tool is one executable implementing two actions selected with the `TOOLBOX_ACTION` environment variable.

### Describe

With `TOOLBOX_ACTION=describe`, the executable prints either JSON or a line-based text description. JSON may use a compact `args` map or a draft 2020-12 `inputSchema` whose top level is an object. Text uses:

```text
name: run_tests
description: Run the project test suite
path: string? Optional test path
```

Names use letters, numbers, `_`, or `-` and are registered as `tb__<name>`. Descriptions run with bounded time and output. Invalid, duplicate, timed-out, or unencodable tools are skipped.

### Execute

With `TOOLBOX_ACTION=execute`, JSON-described tools receive the raw JSON argument object on stdin. Text-described tools receive `key=value` lines and therefore cannot accept names or string values containing line breaks; Cake rejects those calls rather than changing their meaning.

The process runs in the session working directory with `AGENT=cake` and the session id in `CAKE_THREAD_ID` and `AGENT_THREAD_ID`. Stdout becomes the model tool result. Non-zero status is an error. Execution time, stdout, and stderr are bounded, and timeout or output-cap handling terminates the tool's process group.

Toolbox executables are trusted and run outside the Bash sandbox. Their calls may execute concurrently, and Cake cannot infer mutation targets for same-path serialization.

## Compatibility changes

Changes to session versioning, record names or required fields, stream ordering, exit-code meaning, hook decisions, toolbox framing, or machine-readable stdout are compatibility changes. They require focused serialization or integration tests, migration analysis where applicable, and an update to this document's semantics.

## Related decisions

- [ADR 004](adr/004-append-only-session-task-events.md), append-only task events.
- [ADR 005](adr/005-command-hooks.md), command hooks.
- [ADR 007](adr/007-per-session-telemetry-sidecar.md), the telemetry sidecar.
- [ADR 017](adr/017-trusted-executable-toolbox-tools.md), trusted toolbox executables.
