# Integration contracts

This document describes Cake's stable shell-facing and machine-facing contracts. Rust types and snapshots are the field-level serialization authority; this guide explains semantics, identity, ordering, and compatibility.

## Exit codes

  | Code | Meaning                                                       |
  | ---- | ------------------------------------------------------------- |
  | `0`  | successful task                                               |
  | `1`  | agent or tool execution error                                 |
  | `2`  | authentication, rate-limit, network, or request-timeout error |
  | `3`  | invalid input, flags, configuration, or missing credentials   |

Class `2` is the provider-failure class: authentication (`401`, `403`), rate limiting (`429`), request timeouts (`408`), and transport failures such as connect errors or client timeouts. Retryability is a separate transport-recovery concern and does not decide the class: `409` (lock timeout), `5xx`, and `529` are retried but exit `1`, because the provider answered the request. A tool-level timeout --- a toolbox tool or a hook exceeding its timeout --- is a tool error, `1`; the Bash tool's own wait is a yield window, not a timeout failure. See [ADR 045](adr/045-classify-request-timeouts-as-api-errors.md).

Human diagnostics go to stderr. Machine-readable stdout must contain only its declared JSON format.

Root agent-run inputs, including a prompt and run-only flags such as `--output-schema`, cannot be combined with a selected subcommand. Cake reports that invalid usage on stderr and exits `3` before dispatching the subcommand. Options explicitly shared with a command, such as `--output-format stream-json replay <uuid>`, remain valid.

`cake init` creates `.cake/settings.toml` (and, with `--hooks`, an inert `.cake/hooks.json.example`) and prints created files to stdout. An existing target is reported on stderr and exits `3` without writing anything.

For `stream-json`, validation failures before a task stream starts still use the codes above. Once streaming begins, ordinary agent, provider, and tool failures are represented by the final `task_complete` record and the process exits `0`. An unsatisfied output schema remains nonzero, and interruption exits `130`. A hit `max_turns` or `max_tool_calls` limit is an in-stream outcome: stream-json exits `0`, text and JSON modes exit `1`. If a consumer closes stdout early (for example, `head -1`), Cake treats the resulting broken pipe as normal consumer cancellation in every output mode, emits no broken-pipe diagnostic, and exits `0`.

`cake --fail-on-error` opts a `stream-json` run into a strict version of that contract, so a pipeline can act on `$?` alone ([ADR 046](adr/046-opt-in-strict-exit-codes-for-stream-json.md)). Without the flag nothing changes. With it, the run's own outcome decides the status:

- A failed (`error`/`limit`) `task_complete` exits with the class of that outcome: `1` for an agent-class failure (including a fatal tool error), a cut-off, or a `max_turns`/`max_tool_calls` limit outcome, and for a provider failure that is none of the class-2 kinds (`5xx`, for example); `2` for an authentication, rate-limit, or network failure. Input and configuration failures are rejected before the stream starts, so an in-stream `3` does not exist. An unsatisfied output schema stays `1` with or without the flag.
- A broken pipe from a consumer that closes stdout still exits `0`, and an interruption still exits `130`.
- The stream itself is unchanged: the final `task_complete` record remains the outcome channel.

The flag is a root agent-run option, like `--output-schema`: combining it with a subcommand is invalid usage and exits `3`. It is accepted but inert with `--output-format text` and `--output-format json`, which already exit nonzero on a failed run.

Interruption follows one graceful path for both Ctrl-C (SIGINT) and, on Unix, SIGTERM --- the signal process supervisors use to cancel a run. The active task closes with an interrupted `task_complete` record and the telemetry summary is flushed before the process exits `130`.

## Terminal status reports (OSC 7501)

A prompt run reports its own state with the [Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status), so a terminal or an agent inbox can show what cake is doing without window-title heuristics or a per-tool integration. Reports go to stderr, only when stderr is a terminal, and only from a prompt run: a piped, redirected, or captured stderr never carries a sequence, and stdout, session records, stream-json events, hook payloads, and the model-visible conversation never carry one at all. Reporting needs no setting, because a terminal that does not know the sequence ignores it.

Each report replaces cake's root record: no `id`, `app=cake` on every report. `working` starts an agent turn, `done` ends a turn whose result is ready to be seen, `error` ends a turn that failed and carries the run's error text as `msg`, `idle` reports a user interruption, and `blocked` with `kind=question` reports the wait for a prompt on piped stdin. A failure before the agent turn --- input validation, configuration, or worktree setup --- reports nothing, and the terminal drops the transient record it was showing when the process exits. `msg` values are single-line, control characters collapsed to spaces, and shortened to the protocol's decoded limit. A failed write is ignored, and no report changes an exit code. Cake never sends `clear`: a process exit already drops a `working` or `blocked` record, and a `done` or `error` record is left behind for the user to find. See [ADR 042](adr/042-report-run-state-with-the-osc-7501-program-status-protocol.md).

## Provider tool schemas

For Responses requests to `api.openai.com` and the ChatGPT Codex backend, Cake sends function tools with explicit `strict: false`. This opts out of automatic strict-schema normalization and preserves the original parameter schemas and optional fields. Tool calling remains best-effort; executors still validate arguments. Other Responses providers and Chat Completions keep their existing tool serialization. This setting does not change strict final-output constraints from `--output-schema`.

## Provider retries

Retries are bounded. Cake retries transport failures --- a connect or stale-connection failure while the request is sent, and a reset, broken pipe, or truncated read of an accepted 2xx body --- and HTTP `408`, `409`, `429`, `500`, `502`, `503`, `504`, and provider-overload signals such as `529` or a structured `overloaded_error`. A complete 2xx body that cannot be decoded, a stream that ends without a terminal event, and a semantic parse failure are not treated as transport failures and stay terminal. An `x-should-retry: false` response header prevents a retry; `x-should-retry: true` additionally permits otherwise borderline `5xx` responses. Ordinary `400`, `401`, `403`, and `404` responses are not retried.

A parseable `Retry-After` value takes precedence over exponential backoff, but is capped by the active maximum backoff. Transport recovery temporarily disables idle connection reuse. A parseable context-window overflow may be retried once with reduced output and reasoning-token budgets when enough output space remains.

Cake also makes one zero-delay semantic continuation turn when a successful provider response has partial output but no final assistant message, unless the provider identifies non-retryable termination such as content filtering, failure, or refusal, or the response is classified as context pressure. The continuation stays in the same session and task, preserves usage and counters, asks only for the missing final answer, and offers no tools so completed tool work is not repeated. Text mode reports the `semantic_incomplete` retry on stderr; JSON and stream-JSON suppress it. If that turn is also incomplete, Cake emits the cut-off outcome once and includes an explicit `cake --resume <UUID> "try again"` command.

A response is discarded as context pressure only when independent evidence establishes it: a typed provider input-overflow rejection whose input cannot fit, or a response that reported complete input usage above the configured `context_window`, whatever ended the turn. Missing, partial, or unreported usage, a plain length stop, and provider-side truncation never establish it; a length stop at or above the intended output cap is an output-limit stop instead, and a refusal, a content filter, or a provider-reported failure is never pressure. A measured discard's items are not streamed, persisted, or executed, its reported usage still counts once, and it never also receives a semantic continuation; the run ends with the cut-off outcome. A typed input-overflow rejection is not a cut-off: it remains the terminal provider error, with its provider status and body, until automatic compaction can retry the request.

For streamed Responses API calls, either `response.completed` or the terminal `response.incomplete` event finalizes the provider turn. Cake preserves the incomplete response's status, reason, usage, output, and provider response ID, so token-limit responses use the same typed termination and semantic-recovery behavior as non-streaming responses. A stream without either terminal event, or with a second terminal event, remains a typed body-parse error. Malformed `response.output_item.done` data, including a missing item or failed item deserialization, is also a typed body-parse error after terminal handling, with a bounded diagnostic preview.

The agent loop is uncapped by default; a user-configured `[limits]` section (see [Configuration](configuration.md)) may bound it. `max_turns` stops the loop before starting a turn that would exceed the cap; `max_tool_calls` stops it before executing a tool batch that would exceed the cap. Streamed conversation records and completed tool work are never discarded.

## Completion JSON

`--output-format json` emits one JSON object after the invocation completes. It contains the final result or error plus session metadata, usage, working directory, turn count, and elapsed time.

`usage` is the task aggregate (see [Persisted sessions](#persisted-sessions)); per-attempt usage and provider identity are not part of this document. Text output likewise reports the task. Stream-JSON keeps `task_complete.usage` as that same aggregate while also emitting main-agent `turn_usage` records for clients that measure context use (ADR 048); the remaining per-attempt audit detail lives in the session file and the telemetry sidecar's `api_attempt` records.

Progress and retry rendering are suppressed so stdout remains parseable. The exact serialized shape is protected by CLI tests; consumers should tolerate additional optional fields.

## Stream JSON

`--output-format stream-json` emits newline-delimited JSON records for the current invocation as events occur. It includes task boundaries, conversation items, main-agent per-attempt usage, hook events, and task completion. It does not emit session metadata, skill-activation metadata, or prior tasks.

The `task_start` record that opens a task reports the resolved model identity serving the run: `model` is the provider model ID and `model_config` is the `[[models]]` entry name, the same split as `session_meta`. Both fields are absent when the identity is unknown; a consumer must tolerate their absence and must not substitute the raw `--model` value.

A redirected stream is an event feed, not a resumable session file. Consumers should process records by their top-level `type`, preserve order, ignore unknown optional fields, and use `call_id` to associate tool calls, outputs, and hook events.

Malformed model tool arguments remain visible on the `function_call` record and produce a corresponding error output instead of making the stream invalid.

A `message` or `function_call_output` record carries the additive `images` array when the item holds image bytes; text-only records omit the field, so consumers that ignore unknown optional fields are unaffected.

A `turn_usage` record reports one main-agent provider attempt that reported usage. It carries the attempt's normalized `usage`, `turn`, `timestamp`, and provider identity, plus optional `attempt` and `terminal_class` on retried or failed attempts. A first successful attempt keeps the legacy shape with both fields absent, even if the run later discards that response as context pressure; a consumer must not infer a discard from the usage record alone. The record is emitted at the provider boundary, so it can appear before that response's `message`, `reasoning`, `function_call`, or `function_call_output` records. Use the most recent record with `usage_presence` equal to `complete` as the current context size: its `usage.input_tokens` is the provider's count of the request just sent. `task_complete.usage` remains the sum of every reported attempt, including retries.

The optional `context_window` and `remaining_context_tokens` fields report the configured input window and the budget left after the attempt. Both are present only when the model configures a window and `usage_presence` is `complete`; an unknown window, a partial provider report, or a record written before the fields existed omits them rather than sending zero or a fabricated fraction. `remaining_context_tokens` subtracts only the attempt's reported input tokens: it does not reserve room for that attempt's output or for tool results appended after it, so a client deciding whether the next request fits must keep its own buffer (see [Configuration](configuration.md)). Summary and compaction attempts are session and telemetry audit records and never appear as stream context measurements.

An automatic semantic continuation is visible as the provider's partial conversation records followed by one Cake-authored user continuation message. The invocation still emits exactly one final `task_complete`; recovery does not create another task boundary.

## Diagnostic JSON

`debug models --json`, `sessions list --json`, and `bash check --json` emit one document with the same top-level fields, so a consumer parses one shape instead of one dialect per command. Arrays the document carries are ordered by the command that builds them, never by directory iteration or hash order, so two runs over the same state emit the same bytes.

```json
{
  "schema_version": 1,
  "command": "debug models",
  "status": "ok",
  "summary": { "count": 1 },
  "checks": [],
  "data": { "models": [] }
}
```

  | Field            | Meaning                                                                                                                           |
  | ---------------- | --------------------------------------------------------------------------------------------------------------------------------- |
  | `schema_version` | Document schema version. Consumers switch on it; a change to the top-level shape or to an existing field's meaning increments it. |
  | `command`        | The invoked command path, such as `debug models`.                                                                                 |
  | `status`         | `ok`, `warning`, or `error`, derived from the findings: the most severe finding wins.                                             |
  | `summary`        | Command-specific scalars, such as the item `count` or the gate `verdict`.                                                         |
  | `checks`         | Structured findings, in the order the command produced them; empty when there is nothing to report.                               |
  | `data`           | The command's payload. Command-specific, and empty when the command could not produce one.                                        |

`status` describes the invocation, not the subject it inspected: a `bash check` document that reports a `block` verdict is still `ok`, because the inspection itself completed.

Each finding carries a stable machine-readable `id`, a `status` of `warning` or `error`, and a human-readable `message`:

- `settings.unknown_key` --- one finding per unrecognized settings key, in loader order, with the source file and key in the message.
- `settings.load_failed` --- settings could not be loaded or parsed. The message matches the stderr diagnostic.
- `sessions.directory_unreadable` --- the sessions directory could not be read.
- `sessions.checkpoint_failed` --- a `cake sessions checkpoint` append failed. The message matches the stderr diagnostic, and the exit code follows that failure's class (see [Checkpoint append](#checkpoint-append)).

Consumers switch on `id`, ignore unknown ids, and tolerate added fields and added findings.

Machine stdout contains the document and nothing else: one pretty-printed object followed by a newline. A command that reports configuration findings in machine mode keeps them out of stderr, so a consumer reads the document for findings and never needs stderr. Human diagnostics, including a failure the command cannot report as a document, stay on stderr. Machine-mode paths that cannot build a document --- a `bash check --json` gate error, or a settings load failure on `bash check --json` --- keep their findings and diagnostics on stderr rather than dropping them.

Exit behavior follows the codes above: `ok` and `warning` exit `0`; `error` reports a problem the command can carry in the document and exits nonzero with the same message on stderr. `error` is not a code of its own: the process exits with the documented class of the failure the finding reports. `settings.load_failed` is a configuration defect and exits `3`; `sessions.directory_unreadable` is a failed directory read and exits `1`; `settings.unknown_key` is a warning and never changes the code. A failure the command cannot render as a document writes only to stderr, exits nonzero, and leaves stdout empty. `bash check --json` keeps its documented gate-failure classification (see [Gate protocol](#gate-protocol)) and renders verdict outcomes as documents.

### Diagnostic payloads

`debug models --json` reports the configured models: `summary.count` is the number of models, `data.models` holds the full model definitions in name order, and unrecognized settings keys appear in `checks` rather than on stderr.

`sessions list --json` reports the sessions for the current working directory: `summary.count` is the number of listed sessions and `data.sessions` holds each `session_id`, `timestamp`, and `first_prompt`, newest first, with the session id breaking ties between equal timestamps.

`cake sessions checkpoint --json` reports one append: `summary` carries `session_id`, `checkpoint_id`, `boundary`, `before_tokens`, `after_tokens`, and `retained_count`, and `data` carries `session_file` and `basis`.

`bash check --json` reports one configured-pipeline result. `summary.verdict` is `allow`, `warn`, `block`, or `bypassed`; `data` carries `verdict` (`null` for a bypass or an all-abstain chain), `code`, `confidence`, `message`, `overridden`, `bypassed`, `latency_ms`, `gate`, and `stages`. `data.gate` names the deciding gate, or `null` for a bypass or a chain with no deciding stage. `data.stages` carries one entry per gate in execution order with `gate`, `version`, `mode`, `decision`, `code`, `latency_ms`, `overridden`, `failure_class`, `failed_open`, and `disabled`; shadow entries carry their would-be decision, and later enforce stages that never ran are absent. A bypass has no stages. A startup failure is reported by `gate_startup` and a startup diagnostic, not as a stage.

The legacy fields `data.stage`, `data.probability`, and the `data.observation` object keep their document positions for consumers, but Cake no longer has a TypeSafe stage: `data.stage` is `"gate"` when a gate decided and `null` for a bypass or no decision, `data.probability` is always `null`, and `data.observation` is always the absent shape (`outcome: "absent"`, null probability, failure class, and elapsed time). `data.gate` and `data.stages` are authoritative. Unrecognized settings keys appear in `checks`, as they do for `debug models --json`. A verdict, allowlist override, or bypass exits `0`; a gate failure uses the exit classes under [Gate protocol](#gate-protocol) and writes no document in `--json` mode. `--diagnostic` is not supported: it is rejected with a message pointing at the standalone gate's own diagnostic command.

## Session replay

`cake --output-format stream-json replay <uuid>` re-emits an existing session transcript as stream-json without running a prompt, reading the file read-only (no lock, no append, no network) and preserving record order. Replay is the only stream-json mode that emits `session_meta`, `prompt_context`, and `skill_activated`; live invocations never do. Both live and replay emit `turn_usage`; replay re-emits the stored records, including historical ones whose optional context budget is absent. It re-emits stored records verbatim, so a replayed `task_start` carries the identity the file records; a file written before the identity fields existed replays without them.

Replay skips a trailing partial record from an interrupted writer, matching the session loader's tolerance, and treats a `session_meta` whose `session_id` does not match the requested UUID as `corrupt`.

Replay requires `--output-format stream-json`. On failure it emits one `replay_error` record (machine-readable `kind`, `error`, `session_id` when known, and the accompanying `exit_code`) before exiting non-zero: `output_format`, `invalid_uuid`, and `session_not_found` exit 3; `corrupt`, `unsupported_format`, and `permission` exit 1.

## Persisted sessions

Resumable sessions are flat `{session_id}.jsonl` files under `~/.local/share/cake/sessions/`, or under `{CAKE_DATA_DIR}/sessions/` when the data root is overridden.

The current format version is 4:

1. The first non-empty record is one `session_meta`.
2. Each invocation that reaches setup completion appends one `task_start`; a failure during setup records only the hook events that ran.
3. Conversation and metadata records are appended live.
4. The invocation ends with one `task_complete` when Cake can record an outcome.

Conversation records restored into model history are `message`, `function_call`, `function_call_output`, and `reasoning`. `session_meta`, `task_start`, `prompt_context`, `skill_activated`, `hook_event`, `turn_usage`, and `task_complete` are audit or lifecycle metadata and are not replayed as conversation. A `skill_activated` record marks the first `Read`-tool read of a known skill in a session; a skill loaded through `Bash` (`cat <location>`) is deliberately not recorded, because matching a known `SKILL.md` path inside arbitrary shell text is a heuristic (see [ADR 002](adr/002-agent-skills.md)). `turn_usage` records one normalized `usage` for every main-agent provider attempt that reports it, before retry or discard classification. A retried or failed attempt therefore has optional 1-based `attempt` and `terminal_class` fields; a first successful attempt keeps the legacy shape even if the response is later discarded, and a terminal failed turn is audited when usage is available. The
record is audit metadata, never restored into model history, but replay emits it as a stream record so a client can measure context use (ADR 048). Cake uses the most recent record to seed the agent's last-usage basis on resume and fork, so a restored run knows its current context size before the first new provider request. `task_complete.usage` totals all reported attempts, including failed or retried attempts.

Usage is reported at three layers, and each record states which one it describes:

- A **provider attempt** is one HTTP request to the provider. Its telemetry record is `api_attempt` (see [Telemetry sidecars](#telemetry-sidecars)), and each main-agent attempt that reports usage also writes one `turn_usage` record. A `turn_usage` record names the attempt's own identity: the configured `model`, the provider's response ID in `provider_request_id`, and the model the provider reported serving the response in `response_model` when its wire format supplied one and the attempt succeeded. Summary and compaction attempts remain attributable in the session and telemetry audit, but they are not streamed as context measurements.
- An **agent turn** is one logical loop iteration, reported by `turns` in the completion JSON and by one provider turn of conversation records. One turn may contain several provider attempts.
- A **task aggregate** is `task_complete.usage` in the session file and `usage` in the completion JSON: the sum of the `Usage` values of every attempt that reported one, unchanged in shape and meaning by this metadata.

`turn_usage.usage_presence` and `api_attempt.usage_presence` state whether a provider actually reported the counters, so an unknown value never reads as a measured zero:

  | Value        | Meaning                                                                                                                 |
  | ------------ | ----------------------------------------------------------------------------------------------------------------------- |
  | `complete`   | The `usage` object carried `input_tokens`, `output_tokens`, and `total_tokens` (or their Chat Completions equivalents). |
  | `partial`    | The `usage` object was present but at least one of those counters was absent.                                           |
  | `unreported` | No `usage` object was present, or the attempt failed before a body could report one.                                    |

A `partial` record still carries the counters the provider did send; an absent counter reads as zero and the marker is what says the value is unknown. The optional detail objects (`input_tokens_details`, `output_tokens_details`) never affect presence, because providers legitimately omit them. An attempt that reports no usage writes no `turn_usage` record and contributes nothing to the aggregate; Cake never infers a token count it was not given. These fields are optional on records written before they existed, and consumers should tolerate their absence.

`unreported` occurs on `api_attempt` records only: a non-2xx body that carries a usage object reports `complete` or `partial`, and an attempt that reports none writes no `turn_usage` record, so a `turn_usage.usage_presence` is always `complete` or `partial`.

Session consumers should ignore unknown optional fields. `terminal_class` uses the bounded values `completed`, `timeout`, `cancelled`, `transport`, `http`, `body_parse`, and `response_failed`; `transport` covers a failure while the request is sent and a transport failure while an accepted 2xx body is read, while `body_parse` covers a body that was received but could not be decoded or parsed into a provider response.

`cake sessions list` displays session UUIDs. `--resume <UUID>` opens the selected session explicitly, while `--fork [UUID]` creates a new session identity seeded with conversation records and prior `skill_activated` metadata from the selected parent. It does not copy parent session, task, prompt-context, hook, or completion metadata.

Session files are append-only and locked for one writer per invocation. Loading tolerates an interrupted final task without `task_complete`. Unsupported format versions fail explicitly; Cake does not silently rewrite older files.

Before an existing session file is opened for append, Cake restores a record boundary if the previous writer was interrupted mid-record, so the next append cannot join its record onto a partial line. Repair inspects only the unterminated final line, under the same writer lock as the append: a final line that is already a complete JSON object keeps the record and gains its missing newline, and any other incomplete fragment is truncated away. Complete records are never modified; a malformed line that ends with a newline is reported by loading rather than guessed at, so a fragment already joined to a later record stays a load error.

An interrupted task can leave a `function_call` whose `function_call_output` was never written. Resume and fork close each such call by appending an ordinary `function_call_output` that records the call as not executed, so the restored history stays valid for providers. That call-pairing repair appends only; prior bytes are never rewritten, and a history whose pairing is ambiguous fails with a diagnostic instead of being guessed at. Repair does not yet consult replay declarations or re-execute calls.

Live `function_call` and `function_call_output` records may carry an optional `replay` field with the execution-time tool declaration, either `"safe"` or `"never"`. Read and ReadImage are declared `safe`; Bash, BashSession, Edit, and Write are `never`; toolbox tools default to `never` and may opt into `safe` through their describe manifest. Missing declarations in historical records and synthetic repair outputs are treated as `"never"`. A call is eligible for a future automatic replay only when both its persisted snapshot and the current registry declaration are `"safe"`; this release only records the declaration and does not change repair behavior.

### Record semantics

- `session_meta`: version, session identity, creation context, tools, optional model/system prompt, optional model-config entry name, and Git state. `model` is the provider model ID; `model_config` is the `[[models]]` entry name the session was created with and is the preferred resume identity when several entries share one ID.
- `task_start`: task identity and timestamp for one CLI invocation, plus the resolved model identity when it is known: `model` is the provider model ID and `model_config` is the `[[models]]` entry name, with the same split as `session_meta`. Both fields are absent when the identity is unknown --- a record replayed from a session file written before they existed, or one built without a resolved model --- and absence is never filled in from the raw `--model` argument.
- `prompt_context`: mutable developer context used by that invocation.
- `message`: typed user, assistant, or tool text, plus an optional `images` array of `{media_type, data_base64}` objects. The field is absent on text-only records and on records written before image support existed; a loader treats an absent field as no images.
- `function_call` and `function_call_output`: provider tool request and result, joined by `call_id`. A `function_call_output` may carry the same optional `images` array when the tool returned image bytes.
- `reasoning`: provider reasoning data retained for round trips. Its `summary` array uses typed objects with `type` and `text`; loaders accept historical string entries and normalize them to `summary_text` objects.
- `skill_activated`: first observed read of a known skill in a session.
- `hook_event`: hook execution, decision, timing, and bounded diagnostics.
- `turn_usage`: one main-agent provider attempt's reported usage, with optional 1-based `attempt`, `terminal_class`, `usage_presence`, `model`, `response_model`, `provider_request_id`, `context_window`, and `remaining_context_tokens`. The budget pair is present only when the window is configured and the provider reported complete usage.
- `task_complete`: outcome, duration, turns, tool-call count, result or error, usage, and optional permission denials. Denial strings keep the existing `Tool(call): ...` shape; filesystem-sandbox entries use `sandbox: <operation> <path>` after that prefix, where operation is `read`, `write`, or `execute`. A `limit_exceeded` outcome also carries the fired `limit` key and a partial `result`.
- `compaction`: checkpoint identity, summary, retained tail, model and instruction metadata, token estimates, and optional summarizer `summary_usage` and `summary_time_ms`. The usage object is the flat provider measurement. Both fields are absent on records written before they existed; `summary_usage` is also absent when the summarizer made no model call, and `summary_time_ms` is absent when the summary stage was not measured.

Serialization snapshots under `src/types/snapshots/` provide canonical record examples.

For evidence-backed review of a persisted session, use the `analyzing-cake-sessions` skill. For reactive triage of a recent failed run, use the `debugging-cake` skill.

## Checkpoint append

`cake sessions checkpoint <UUID> [--json]` appends exactly one `compaction` record to an existing format-4 session. It reads one JSON payload from stdin and neither calls a model nor chooses what to summarize or retain; an interactive terminal with no piped payload is an input error.

The caller owns the selection, the summary, the retained tail, the trigger, and the token estimates. Cake owns everything that identifies or positions the record: `type`, `checkpoint_version`, `checkpoint_id` (a new UUID), `session_id` (the target session), `timestamp` (now), and the boundary (the conversation-item count at the append position). The payload is a protocol: an unknown field, or any payload-supplied core-owned field, is rejected.

```json
{
  "trigger": "manual",
  "summary": "One paragraph describing the retired history.",
  "retained": [
    { "type": "message", "role": "assistant", "content": "the latest answer" },
    { "type": "function_call", "id": "fc-1", "call_id": "call-1", "name": "Read", "arguments": "{}" },
    { "type": "function_call_output", "call_id": "call-1", "output": "file contents" }
  ],
  "estimated_input": { "before_tokens": 12000, "after_tokens": 3000, "basis": "measured_usage" },
  "summary_instruction_version": 1,
  "summary_usage": {
    "input_tokens": 1457,
    "cached_tokens": 0,
    "output_tokens": 779,
    "reasoning_tokens": 291,
    "summary_visible_tokens": 488,
    "total_tokens": 2236
  },
  "summary_time_ms": 6757,
  "model": { "model": "gpt-5.6-luna", "model_config": "luna-high" },
  "task_id": "optional; defaults to the latest task_start"
}
```

`retained` holds the same restricted union a checkpoint persists: `message`, `reasoning`, `function_call`, and `function_call_output` payloads, with their optional pin, origin, skill-read, and source-task metadata. Every retained call needs its matching output, and a retained message may not have the `system` or `tool` role. `model` defaults to the session's recorded model identity and `task_id` to the newest `task_start`; the command rejects a payload when neither source exists. `estimated_input.after_tokens` must be below `before_tokens`.

`summary_usage` is optional and records the summarizer's provider-reported tokens when it made a model call: input, cached input, output, hidden reasoning, visible output, and total. Within the object, `cached_tokens` is a subset of `input_tokens`, `summary_visible_tokens` is `output_tokens - reasoning_tokens` clamped at zero, and `total_tokens` is the provider-reported total. `summary_time_ms` is optional and records the summary stage's wall time, including a summary-file read or command call. Both are copied into the `compaction` record unchanged; a file or command summarizer omits `summary_usage` but can still record `summary_time_ms`.

The command takes the session's exclusive append lock first, repairs an interrupted final record under that lock, and reads identity and history through the same handle. Prior bytes are never rewritten: a `compaction` record is appended only. Validation uses the M1 rules --- checkpoint structure, retained roles and complete call/output pairing, pin and provenance placement, nonempty identity and summary, the framed 8 MiB record ceiling, and the token reduction --- and runs before the writer is used, so a rejected payload leaves the file byte-stable. A failed append preserves the original error and no later append is attempted.

Raw history stays in the session file. Replay emits the raw transcript plus the checkpoint record at its physical position without expanding the retained tail (see [Session replay](#session-replay)); resume and fork use the newest checkpoint as the active one. A Cake build without checkpoint support cannot read a session that has received one, so keep a checkpoint-aware binary for those sessions.

`--json` emits one diagnostic document, described under [Diagnostic payloads](#diagnostic-payloads). Exit codes follow [Exit codes](#exit-codes): `0` success; `3` for invalid input or identity --- an invalid UUID, no piped payload, invalid JSON, an unknown or core-owned field, a rejected checkpoint (including an oversized record or a non-reduction), a missing session, a file whose header names another session, or a missing model or task id; `1` for operational failures --- another invocation holding the session lock, an unreadable or unsupported session file, or a write failure. Failures write nothing.

## Telemetry sidecars

Operational telemetry lives under `~/.cache/cake/session-telemetry/{session_id}.ndjson` or the corresponding `CAKE_DATA_DIR` path. `invocation_id` separates invocations in one sidecar.

Before an existing sidecar is opened for append, Cake restores a record boundary if the previous writer was interrupted mid-record: only the unterminated final segment --- the bytes after the last newline, or the whole file when it has none --- is treated as a truncated tail and removed. Earlier complete records are never modified, and the next invocation's first record starts its own line. A newline-terminated line is a complete record even when it does not parse, so a malformed framed line is left byte-for-byte intact and reported by consumers rather than guessed at or discarded.

Records cover initialization, in-flight and completed conversation API attempts, gate startup/decisions/evidence, retries, tools, and summaries. They omit prompts, assistant text, tool and provider response bodies, command/reason/cwd values, and credentials. Two invariants bound every record. Provider-assigned identifiers (a tool-call id, a provider request id) are persisted only as the lowercase hex SHA-256 digest of the raw value, so a consumer correlates a record with its source by hashing the known raw identifier the same way, and an absent or empty identifier writes `null` or omits the field rather than the digest of an empty string. Provider-supplied text (`api_attempt.error`, the `responses_failed` strings, `retry_scheduled.detail`, `compensation.detail`, `session_summary.error`) is bounded to 1,024 bytes: a longer value is cut on a UTF-8 boundary and ends with the ASCII marker `...[truncated]`, which counts inside the 1,024 bytes, so one oversized provider error cannot grow a
record. Records written before these policies keep their original raw values and remain readable. Each provider attempt emits a bounded `api_attempt_in_flight` record before awaiting headers and another when headers arrive, followed by the existing `api_attempt` completion record; correlate them by `session_id`, `invocation_id`, `task_id`, `turn_index`, and `attempt`. The in-flight record includes the resolved provider/model, UTC `started_at`, phase (`awaiting_headers` or `reading_body`), and status when headers are available. A completed `api_attempt` retains its existing timings and fields, plus the last known phase, the configured `model`, the resolved `provider` when the endpoint matched a known provider strategy, the provider-reported `response_model` when the wire format supplied one and the attempt succeeded, and `usage_presence` (`complete`, `partial`, or `unreported`; see [Persisted sessions](#persisted-sessions)). Its bounded `terminal_class` (`completed`, `timeout`,
`cancelled`, `transport`, `http`, `body_parse`, or `response_failed`) makes timeout, cancellation, HTTP, decode, and accepted-body transport failures explicit; a transport failure while the accepted body was read records the `reading_body` phase, the accepted status, and no usage. For a `response_failed` attempt, the record retains `provider_request_id` as a one-way digest and a bounded `responses_failed` object (`type`, `code`, `param`, `message`) supplied by the provider; the response id has exactly one owner there, and the nested object never repeats it. A present `code` is authoritative for retry classification; `type` is used only as a compatibility fallback when `code` is absent. A transient `server_error` is retried under the bounded retry policy, while auth, invalid-request, quota, context, policy, and other semantic failures are not.

New `tool_call` records add optional UTC `started_at` and `completed_at` fields to their existing monotonic `duration_ms`, including immediately blocked calls, and carry `call_id` as the digest of the provider-assigned identifier, absent when the call has none, so a tool call joins the `gate_decision`, historical `judge_attempt`/`type_safe_shadow`, and compensation digests recorded for the same call. A `retry_wait` record adds the UTC span and measured `duration_ms` of a main-provider backoff after its `retry_scheduled` record; the scheduled delay still describes intent if the task is interrupted before waiting completes. These metadata-only spans let session metrics show an invocation's parallel activity and residual wall time. Consumers must tolerate older sidecars without them; no session JSONL, completion JSON, or model-visible tool record changes.

The historical `judge_attempt` record identifies the former in-process judge's model, request controls, phase timings, retry ordinal, terminal class, request identity digests, usage, and termination. Cake no longer writes it; it remains readable in old sidecars. Current policy evidence and latency are in `gate_startup` and `gate_decision` below.

Main-provider `api_attempt.usage.input_tokens_details` may additionally contain provider-reported `cached_tokens` and `cache_write_tokens`; these optional usage details support the read-only cache-break analysis in `scripts/session-metrics/cache_breaks.py`. The same optional `cache_write_tokens` detail may appear in serialized `Usage` values in `turn_usage`, `task_complete`, and `session_summary` records.

`telemetry_init` carries the invocation's `model` (provider ID) plus optional `settings.model_config` (the resolved `[[models]]` entry name) and `settings.base_url` (the provider endpoint, non-secret), so cache analysis can distinguish same-ID entries on different backends. The endpoint is kept in its parsed, credential-free form: userinfo, query, and fragment are removed before the 1,024-byte bound is applied, and a value that is not a parsable URL writes `null` rather than the raw string. New records carry no `working_directory`, so a sidecar does not disclose the invocation's working directory or the local username embedded in it; a bounded `compensation.detail` value can still name a canonical target path. Project identity comes from the session transcript's `session_meta` record, and a historical sidecar that carries the field remains valid and is preferred by readers when present.

A `compensation` carries its `kind`, optional bounded `detail`, gate-verdict `latency_ms`, and allowlist `overridden` flag. Kinds are `json_repair`, `judge_verdict`, `judge_fail_closed`, `judge_bypass`, `same_path_serialization`, `output_truncation`, `context_overflow_retry`, and `edit_invalid_arguments`. `same_path_serialization` records that a call's start was delayed behind an earlier call targeting the same canonical path; reads and mutations both qualify (ADR 043). The three `judge_*` kinds are compatibility projections emitted for current generic gate decisions: `judge_verdict` details are `block:<code>`, `warn:<code>`, or `allow`; fail-closed details name the failure class; and each event carries `call_id`, the one-way SHA-256 digest of the originating transcript tool-call identifier. `gate_decision` is the authoritative record; the compatibility event keeps existing denial attribution and metrics working.

The historical `type_safe_shadow` record contains the former in-process TypeSafe observation: elapsed time, model identity, probability, optional token usage, failure class, and call digest. Cake no longer runs TypeSafe or writes this record; it remains readable in old sidecars. A legacy `[tools.bash.judge.typesafe]` table is forwarded to the external gate's config, and that gate owns any current observation record.

Historical cascade fast approvals are reconstructed from the old `type_safe_shadow` and `judge_verdict` records exactly as before; they are not a current telemetry surface. New gates report their decisions and evidence generically.

The `cake-gate/1` records are the authoritative current surface (see [Gate protocol](#gate-protocol)). `gate_startup` is one metadata-only record per gate at startup, carrying the gate name and version, mode, declared phases and actions, outcome, failure class, and latency. `gate_decision` is one metadata-only record for every gate that participated in the check, including a disabled gate and the global bypass, carrying the gate name and version, mode, phase, action, decision, code, latency, override, bypass and disabled flags, failure class, and the one-way `call_id` digest. `gate_evidence` carries a gate-declared `kind` and bounded `data`, and is the only path by which gate-controlled text reaches telemetry. A bypass records `gate: null`, `decision: null`, and `bypassed: true`. Every current generic decision also emits the compatibility `judge_verdict`, `judge_fail_closed`, or `judge_bypass` event described above, so consumers can migrate without losing denial attribution. Gate
evidence is never re-emitted as a historical `judge_attempt` or `type_safe_shadow` record.

Consumers must tolerate added enum values and optional fields; old sidecars remain valid. Sidecars never drive resume, fork, or session discovery.

## Bash tool and command-safety checks

Bash `timeout` is a foreground yield window in seconds (default 60, clamped to 1--60). A command that completes inside it keeps the ordinary output and exit footer. A command still running when the window ends, or started with `background: true`, returns output so far and a `bash_` session ID; it continues under the same sandbox. Polling and watch commands should start with `background: true` and use `BashSession`, rather than waiting in the foreground. When the shell exits, Cake stops remaining children in its process group. `BashSession` reads only new output, kills the process group, or lists live and recently exited sessions. Its `read.wait` defaults to 10 seconds, is capped at 120, and accepts `0` for a nonblocking poll. Completed sessions remain readable for a bounded time; an unknown ID explains that sessions do not survive Cake restarts. These results use existing function-call transcript records and do not change the completion JSON or stream JSON shapes.

The Bash tool's optional `reason` is the model's untrusted intent report; guidance directs agents to supply it for state-changing, destructive-looking, and remote-effect commands, stating the intended effect. A `reason` never authorizes a remote destructive command on its own, so a merge or branch delete requires an in-command guard. The configured gate receives the reason with the command; it appears in transcripts, never telemetry. Cake sends each gate request as a standalone check --- command, cwd, repository digest, reason, and any collected script contents --- with no conversation history or earlier tool results, so remediation recommends a self-contained guarded command the next request can evaluate alone.

Bash calls may also provide an optional `cwd`. Relative paths resolve from the invocation working directory; absolute paths are accepted only when they resolve to an existing directory inside that workspace or inside a directory the sandbox already grants (`--add-dir`, settings, a skill directory, a Cake-managed temporary path, or a built-in toolchain cache). Under `--sandbox danger-full-access` any existing directory is accepted, because that policy applies no profile. The selected directory applies to that call only and is used consistently by the child process, gate request, repository digest, and sandbox-denial diagnostics. A `cwd` outside the workspace and every grant fails before gate evaluation and before the process is spawned, with a message naming the boundary and the grant classes. Omitting `cwd` preserves the invocation directory.

Every non-empty Bash command runs through the configured external pipeline before spawn. A gate can allow, warn, or block; a per-check failure follows its `fail` mode, and startup failure removes Bash for the run. The command still runs under the OS sandbox, and a gate approval is never a sandbox bypass. [Security](security.md) defines the trust boundary and [Configuration](configuration.md) the `[tools.bash]` and `[gates]` settings. The gate owns its own provider calls, timeouts, retries, and fast-approval stages; Cake enforces only the returned decision. The wire contract is under [Gate protocol](#gate-protocol).

`cake bash check -- <command>` runs the configured pipeline, including shadow gates, without executing the command and without collecting referenced host files. A pipeline with no usable enforce gate exits `3` with the no-gate diagnostic, unless explicitly bypassed; a bypass exits `0` and reports `bypassed: true`. Verdicts, allowlist overrides, and bypass exit `0`. Gate failure classes map as documented under [Gate protocol](#gate-protocol): timeout, network, auth, and rate limit exit `2`; transport, malformed, refusal, protocol, crash, unsupported, and internal exit `1`; configuration exits `3`.

`--diagnostic` is rejected. It formerly printed the in-process judge's prompts and provider response; use the standalone gate's own diagnostic command instead. `cake bash check --json` remains a metadata-only gate-decision document.

## Gate protocol

Status: this section freezes the `cake-gate/1` contract from [ADR 049](adr/049-external-policy-gates-with-the-cake-gate-1-protocol.md), as revised by [ADR 051](adr/051-external-judge-extraction-with-offline-acceptance.md). The external pipeline is the only command gate; `[gates]` settings are active and there is no in-process fallback. The machine-checkable frame schema is [`docs/schemas/cake-gate-1.schema.json`](schemas/cake-gate-1.schema.json); this section defines the semantics the schema cannot express. The schema's closed enums and cardinality bounds are the `cake-gate/1` known set: adding a field is free, adding an enum value or raising a bound is an additive schema edit that does not bump the version, and only removing or changing the meaning of a value requires `cake-gate/2`. Capabilities and error classes are open strings: Cake ignores a capability it does not know and treats an unknown error class as `protocol`, so a later capability or class is additive without a protocol
bump.

A gate is a trusted executable that owns a pre- or post-action policy decision. Cake owns enforcement, context assembly, process lifecycle, composition, and observability. The initial supported phase and action pair is `pre_tool` and `bash.exec`; `post_tool`, Edit, Write, and toolbox gates are deferred. A gate never changes the command, the working directory, or the tool arguments. It can allow, warn, block, or abstain.

Gate executables are trusted configuration, like hooks and toolbox tools. They run outside the Bash sandbox with Cake's ambient authority. A gate must not read the workspace to learn about the action or to resolve sandbox paths; Cake assembles the action evidence so sandbox semantics have one implementation. A gate may read its own rubric and configuration files named in its config, which are policy inputs, not action evidence. See [Security](security.md) for the trust boundary and the no-gate rule.

### Executable interface

A gate implements two required subcommands and one optional one. Cake invokes the configured `command` with its configured `args`, followed by the subcommand:

- `<command> <args...> describe` prints one JSON object on stdout and exits `0`.
- `<command> <args...> serve` runs the long-lived protocol on stdin and stdout.
- `<command> <args...> check` is an optional human and offline mode. Cake does not invoke it as a transport; a gate may define its own argument handling and sugar for it. The reference gate accepts a command line and prints one decision.

`check` is recommended so a corpus can be evaluated without Cake. Logs go to stderr; stdout carries only the described object or protocol frames. Cake starts the gate process in the invocation working directory; the per-action directory is always `input.cwd`, and a gate must not infer it from its own working directory.

### `describe`

Cake runs `describe` once per gate per run, before any work, and validates the result:

```json
{
  "protocol": "cake-gate/1",
  "name": "safety",
  "version": "0.1.0",
  "phases": ["pre_tool"],
  "actions": ["bash.exec"],
  "transports": ["stdio"],
  "capabilities": ["resolved-models", "diagnostic"]
}
```

- `protocol` is exactly `cake-gate/1`. Cake does not negotiate versions; a gate speaking another version is a configuration failure.
- `name` is the gate's own product name and matches `[a-z0-9][a-z0-9-]{0,63}`. The `[gates.<name>]` key is the local instance name and may differ, so one binary can be configured twice with different `config`; composition, `data.gate`, and telemetry use the instance name, and `describe.name` is informational. `version` is the gate's own version, matching `MAJOR.MINOR.PATCH` with an optional prerelease suffix, at most 64 characters.
- `phases` is a non-empty subset of `pre_tool` and `post_tool`; only `pre_tool` is configurable in this release, so a gate may declare `post_tool` for future compatibility but Cake sends no post-tool checks. `actions` is a non-empty list using the `<tool>.<operation>` convention; `bash.exec` is the only action Cake configures in this release.
- `transports` lists `stdio` and optionally `oneshot`. A configured transport the gate does not declare is a configuration failure.
- `capabilities` is required and may be empty. The known values are `shadow`, `diagnostic`, and `resolved-models`. A configured `mode = "shadow"` requires `shadow`. `options.diagnostic` is sent only to a gate declaring `diagnostic`. A gate declaring `resolved-models` receives resolved model descriptors and their credential variable names. A capability Cake does not know is ignored, not a configuration failure.

`phases`, `actions`, `transports`, and `capabilities` are required fields; the first three must be non-empty, and `capabilities` may be empty. A gate must declare every phase and action pair it supports. If configuration names a gate for `bash.exec` and `describe` does not declare that action, Cake treats it as a configuration failure and does not run the command. `describe` output is cached for the run. Under `transport = "oneshot"`, Cake still runs `describe` once, then spawns a fresh `<command> <args...> serve` process for each check with exactly one `init`/`ready`, one `check`/`decision`, and one `shutdown`, followed by process exit; the same frames and validation apply. `transport = "stdio"` keeps one `serve` process for the run and repeats `init` only after a restart. A nonzero exit, a timeout, extra stdout, or an invalid object is a configuration failure.

### `init`

After a gate process starts, Cake sends exactly one `init` frame before any check. A restarted process receives a fresh `init`:

```json
{
  "protocol": "cake-gate/1",
  "type": "init",
  "id": "init",
  "session": {
    "session_id": "9f1c...",
    "task_id": "4b2e...",
    "invocation_id": "01J...",
    "cwd": "/repo"
  },
  "config": {
    "model": "judge",
    "rubric_file": ".cake/judge-rubric.md"
  },
  "models": {
    "judge": {
      "provider": "openai",
      "base_url": "https://api.example.test/v1",
      "api_type": "chat_completions",
      "model": "judge-model",
      "api_key_env": "JUDGE_API_KEY"
    }
  },
  "env": ["JUDGE_API_KEY"]
}
```

`config` is the gate's own `[gates.<name>.config]` table, passed as JSON. It is limited to 64 KiB; put credentials in environment variables, never in gate configuration.

`session` identifies an agent run. `session_id`, `task_id`, and `invocation_id` are optional; a standalone `cake bash check` supplies `cwd` and an invocation id but no agent session or task. A gate must treat these identifiers as correlation context, never as authorization, and must not require them.

`models` is present only for a gate declaring `resolved-models`. It contains one descriptor for each name listed by `[gates.<name>].models`; the reserved name `default` selects the run's resolved model. The auto-detected safety gate receives `default` even without a list, provided it declares `resolved-models`, so it works with no configuration; an explicitly configured gate receives exactly the names it lists. A descriptor is emitted only for an environment-backed model whose `api_key_env` is the credential the run actually uses. A file-backed subscription model is never emitted: the auto-detected `default` is omitted, and an explicit `[gates.<name>].models` entry that resolves to a file-backed credential is a settings error. A descriptor always carries `base_url`, `api_type` (`chat_completions` or `responses`), `model`, and `api_key_env`; it may also carry `provider`, `provider_headers` (`http_referer` and `x_title`), `temperature`, `top_p`, `max_output_tokens`, `reasoning_effort`,
`reasoning_summary`, `reasoning_max_tokens`. Unset optional fields are omitted. A keyless local endpoint is configured in the gate's own `config`, not as a descriptor. The descriptor never contains a credential value, an `Authorization` header, or a credential file path.

`env` lists every variable Cake passes beyond the fixed base set. It is the union of `[gates.<name>].env` and the `api_key_env` names of the included model descriptors, at most 32 names. Cake starts the gate with `PATH`, `HOME`, `TMPDIR`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TZ`, `SSL_CERT_FILE`, and `SSL_CERT_DIR` from its own environment, plus exactly those names. All other variables are removed. The gate reads its credentials from those variables. A gate model must be environment-backed: Cake does not pass a file-backed subscription credential or its path. If the run's model uses one, the auto-detected `default` descriptor is omitted; a gate that requires a model replies with a `config` error at `init`, Bash is removed for the run, and `cake gates doctor` names the fix (configure `[gates.<name>].models` with an environment-backed model). An explicit `models` entry that resolves to a file-backed credential is a settings error.

The gate answers with `ready`:

```json
{
  "protocol": "cake-gate/1",
  "type": "ready",
  "id": "init",
  "name": "safety",
  "version": "0.1.0",
  "warnings": []
}
```

Cake checks that `ready.name` and `ready.version` match the cached `describe`; a mismatch is a protocol failure. `warnings` is optional, bounded to eight entries of 1,024 characters each, and shown by `cake gates doctor`; it never changes a decision. A gate that cannot initialize replies with an error frame instead. Cake waits `startup_timeout_secs` for `ready`; a timeout or invalid reply is a gate failure.

### `check`

Cake sends one check frame per non-empty Bash command. An empty command is not gated because it has no effect.

```json
{
  "protocol": "cake-gate/1",
  "type": "check",
  "id": "01J...",
  "phase": "pre_tool",
  "action": "bash.exec",
  "input": {
    "command": "git status --short",
    "cwd": "/repo",
    "reason": null,
    "repo_digest": "git repo, branch main",
    "script_evidence": []
  },
  "session": {
    "session_id": "9f1c...",
    "task_id": "4b2e...",
    "call_id": "call_...",
    "turn": 3
  },
  "options": {
    "diagnostic": false
  }
}
```

`input.command` is the normalized text Cake will execute, after removal of one simple trailing `&`. `input.cwd` is the resolved per-call working directory. `input.reason` is the model's untrusted intent report, never authorization. `input.repo_digest` is Cake's compact repository line and may be `null`. `input.script_evidence` is an array of `{ "path": "<rendered path>", "contents": "<text>" }`; this release sends zero or one item, with the same 32 KiB collection limit and recognition rules as the Bash preflight. Script text is untrusted evidence. The trusted authorization projection from 033b and the structured action packet from 033c will arrive as additive `input` fields; a gate must ignore fields it does not know.

`session.call_id` is the provider-assigned tool-call identifier when one exists; it may be absent. `session` itself is optional for a standalone `cake bash check`; a normal agent run supplies `session_id`, `task_id`, `call_id`, and `turn` when available. A gate must not require any of them. `session.turn` is optional. `options.diagnostic` is `true` only for `cake bash check --diagnostic` and only when the gate declares `diagnostic`.

### `decision`

A gate replies to each check with exactly one decision frame carrying the same `id`:

```json
{
  "protocol": "cake-gate/1",
  "type": "decision",
  "id": "01J...",
  "decision": "warn",
  "code": "rg-replace-footgun",
  "message": "Use rg -n so the replace preview can be inspected.",
  "confidence": 0.78,
  "annotations": [
    { "placement": "prepend", "text": "NOTICE: Use rg -n so the replace preview can be inspected." }
  ],
  "short_circuit": false,
  "evidence": [
    { "kind": "attempt", "data": { "model": "judge-model", "latency_ms": 1200 } }
  ]
}
```

- `decision` is `allow`, `warn`, `block`, or `abstain`.
- `code` is a gate-defined stable label matching `[a-z0-9][a-z0-9-]{0,63}`. It is required for `block` and `warn`; it is optional for `allow` and `abstain`.
- `message` is required and non-empty for `block` and `warn` and optional otherwise. Cake strips control characters except newline, carriage return, and tab, and truncates it to 1,000 characters. A block renders as `BLOCKED\n\nReason: <message>`; a warn with no annotations renders as `NOTICE: <message>`.
- `confidence` is an optional finite number from `0.0` to `1.0`.
- `annotations` is an optional list of model-visible lines. The only placement in this version is `prepend`. Cake prepends allow and warn annotations in gate order, joined by blank lines, and sanitizes them; it accepts at most eight annotations and truncates each to 1,000 characters. Annotations from `abstain` and `block` decisions are ignored for model output. A warn that supplies annotations does not also repeat its message as a `NOTICE`.
- `short_circuit` is an optional boolean, default `false`. Cake honors it only for an `allow` decision when the gate's configuration sets `short_circuit_on_allow = true`; otherwise it is ignored. A `warn` or `abstain` never short-circuits.
- `evidence` is an optional list of `{ "kind": "<label>", "data": <JSON value> }` objects. Cake records at most eight items, each payload bounded to 8 KiB, and 32 KiB total per decision; excess is dropped and recorded as a `gate_evidence` truncation record. Evidence is the only gate-controlled data that reaches telemetry, so a gate that places command text, a reason, or a message there is explicitly choosing to disclose it.

### `error`

A gate that cannot evaluate sends an error frame instead of a decision:

```json
{
  "protocol": "cake-gate/1",
  "type": "error",
  "id": "01J...",
  "class": "timeout",
  "message": "the upstream judge call exceeded its deadline"
}
```

The bounded classes are `timeout`, `network`, `auth`, `rate_limit`, `transport`, `malformed`, `refusal`, `config`, `unsupported`, `internal`, and `protocol`. Unknown classes are treated as `protocol`. A gate must not use `abstain` for a failure it actually observed; a failure is an error so it is recorded and composed as one. `message` is diagnostic; it is bounded to 1,024 characters and never becomes the model-visible failure text on its own.

### Composition

Cake runs the gates listed by `[tools.bash] gates` in order for `pre_tool` + `bash.exec`. Only gates whose `describe` declares the pair participate. Composition has these rules:

- `abstain` changes nothing and the next gate runs.
- `allow` continues unless the gate sets `short_circuit_on_allow = true` and the decision sets `short_circuit = true`; then later enforce gates do not run.
- `warn` accumulates and the next gate runs. Cake prepends the accumulated warnings, in gate order, to the command output.
- `block` stops the enforce chain and the command does not run, unless that gate's exact normalized-command allowlist contains `input.command`. An overridden block records `overridden: true` and continues.
- A per-check gate failure in enforce mode blocks unless the gate sets `fail = "open"`, which records `failed_open` and runs the next gate; `fail = "closed"` is the default. A `describe` or `init` startup failure is pipeline-fatal and is not affected by `fail`.
- If every enforce gate abstains or is allowlisted through, the command runs. That is a deliberate no-opinion allow: `data.gate` is `null`, and an enforce gate that always abstains is indistinguishable from no policy. A gate that cannot evaluate must send an error frame, never `abstain`. A chain with no enforce gate is not a chain: `bash.exec` has no decision, so Bash is disabled under the no-gate rule.
- `mode = "shadow"` gates run beside the enforce chain and record their would-be decisions. A shadow decision, annotation, failure, or allowlist override never changes authorization and never short-circuits. Shadow gates still run when an enforce gate has already blocked, so the observation is available.

A gate cannot widen another gate's decision. A later allow cannot undo an earlier block; a gate's allowlist overrides only its own block; and only Cake applies an override.

### Lifecycle and framing

`serve` uses newline-delimited JSON over stdio. Each line is one UTF-8 JSON object, at most 1 MiB, with no unescaped control characters and no pretty printing. Frames are flushed as they are written. Every frame carries `protocol` and `type`; `describe` is the only object without a `type`.

Requests may be concurrent: Cake may keep up to eight checks in flight for one gate. A gate may process them serially, but it must answer each with the matching `id`; responses may arrive out of order. A duplicate or unknown `id`, an unknown frame type, a protocol mismatch, a frame over the limit, stdout noise, or an early exit is a protocol failure for the gate process. Cake kills the process group and fails every in-flight check closed.

On a per-check timeout, crash, or protocol failure, Cake kills the process group and fails that check closed unless the gate is `fail = "open"`. The next check starts a fresh process and repeats `init`. Two consecutive process-level failures disable the gate for the rest of the run; later checks follow the gate's `fail` mode, so an open gate records `failed_open` and continues while a closed gate fails immediately with the recorded class. A successful `ready` or completed check resets the count. `describe` is not repeated within the run. A `describe` or `init` failure is a startup failure: Cake writes a `gate_startup` failure record and follows the startup row in Configuration; it is not a per-check failure and `fail` does not apply.

At session end Cake sends `{"protocol":"cake-gate/1","type":"shutdown","id":"shutdown"}`, closes stdin, waits two seconds, then sends SIGTERM and finally kills the process group. A gate should exit `0` after flushing. Shutdown is best effort and never changes a task outcome.

### Telemetry

Cake writes one `gate_startup` record per gate at startup and one `gate_decision` record for every gate that participated in the check, including a gate disabled by the circuit breaker and the global bypass. The records are metadata-only. A startup record is:

```json
{
  "type": "gate_startup",
  "gate": "safety",
  "version": "0.1.0",
  "mode": "enforce",
  "phases": ["pre_tool"],
  "actions": ["bash.exec"],
  "outcome": "failed",
  "failure_class": "config",
  "latency_ms": 42
}
```

`version`, `phases`, and `actions` are `null` or empty when `describe` failed. `outcome` is `ready` or `failed`; `failure_class` is `null` on `ready`. The record carries no gate text. A decision record is:

```json
{
  "type": "gate_decision",
  "gate": "safety",
  "version": "0.1.0",
  "mode": "enforce",
  "phase": "pre_tool",
  "action": "bash.exec",
  "decision": "block",
  "code": "credential-disclosure",
  "latency_ms": 1200,
  "overridden": false,
  "bypassed": false,
  "failed_open": false,
  "disabled": false,
  "failure_class": null,
  "call_id": "<sha256 digest>"
}
```

A bypass has no gate: Cake writes one record with `gate: null`, `decision: null`, `bypassed: true`, and `disabled: false`. The `bypassed` flag carries the effect; a consumer must not read a bypass as a policy allow. A gate failure records `decision: null`, `failure_class`, and either `failed_open` or the fail-closed block. A gate disabled by the circuit breaker records `decision: null`, `disabled: true`, `failure_class`, and `failed_open` according to its `fail` mode; `latency_ms` is `0`. `call_id` is the one-way SHA-256 digest of the originating transcript tool-call identifier, absent when the call has none, matching the `gate_decision` and `tool_call` linkage and the historical `judge_attempt` shape.

Evidence is a separate record so a decision stays small:

```json
{
  "type": "gate_evidence",
  "gate": "safety",
  "call_id": "<sha256 digest>",
  "kind": "attempt",
  "data": { "model": "judge-model", "latency_ms": 1200 }
}
```

Evidence is recorded as `gate_evidence` with the gate name, the same `call_id` digest, the declared `kind`, and bounded `data`. An oversized or excess item is replaced by one `gate_evidence` record with `kind: "truncated"` and `data: {"dropped_items": N, "dropped_bytes": M}`. Gate evidence is the explicit disclosure surface: normal command text, reasons, and messages stay out of telemetry.

Current gate decisions also emit the compatibility `judge_verdict`, `judge_fail_closed`, and `judge_bypass` compensation events described under [Telemetry sidecars](#telemetry-sidecars), so existing denial attribution keeps working. The historical `judge_attempt` and `type_safe_shadow` records have no producer and remain readable only in old sidecars. Gate evidence is written as `gate_evidence` and is never re-emitted as a historical judge record; consumers should read `gate_decision` and `gate_evidence`.

### `cake bash check`

`cake bash check` runs the configured pipeline, including shadow gates, without executing the command and without collecting referenced host files. A pipeline with no `bash.exec` pre-tool gate exits `3` with a diagnostic naming the no-gate rule and `cake gates doctor`, unless the invocation is explicitly bypassed, which exits `0` and reports `bypassed: true`; it never reports an allow it did not obtain. A deliberate decision writes the existing diagnostic document; a gate failure writes no document in `--json` mode and exits with the class below, matching the former judge-failure behavior.

The document preserves the former judge shape. `summary.verdict` is `allow`, `warn`, `block`, or `bypassed`; `data.verdict`, `code`, `confidence`, `message`, `overridden`, `bypassed`, and `latency_ms` keep their meanings. The pipeline verdict is `block` when the command is blocked, otherwise `warn` when at least one enforce gate warned, otherwise `allow`. The deciding gate supplies `data.gate`, `data.code`, `data.confidence`, and `data.message`: the final blocking gate for a block, the first warning gate for a warn, and the first gate that returned `allow` for an allow. A chain in which every enforce gate abstained has no deciding gate, so those fields are `null` or empty and `data.gate` is `null`. An overridden block is not a deciding gate; the deciding gate is chosen from the remaining stages in composition order, and if no stage decided, those fields are `null` or empty. `data.latency_ms` is the end-to-end pipeline latency; each stage's own latency is in `data.stages[].latency_ms`.
Two additive fields describe the pipeline:

- `data.gate` is the name of the deciding stage, or `null` for a bypass or a chain with no deciding stage. An overridden block is not a deciding stage; the override is recorded in `data.overridden` and the stage, and `data.gate` follows the first-warning, first-allow, or final-block rule over the remaining stages.
- `data.stages` is an array in execution order. Each entry carries `gate`, `version`, `mode`, `decision`, `code`, `latency_ms`, `overridden`, `failure_class`, `failed_open`, and `disabled`. Shadow entries carry `mode: "shadow"` and their would-be decision. A later enforce stage that never ran because an earlier gate blocked or short-circuited is absent. When a later check still produces a document (for example, because the gate is `fail = "open"`), a gate disabled by the circuit breaker appears with `decision: null`, `disabled: true`, `failure_class`, `failed_open` according to its `fail` mode, and `latency_ms: 0`; a fail-closed disabled gate writes no document, matching the gate-failure rule, but its `gate_decision` record is still written. Shadow stages still appear when an earlier enforce gate blocked, because shadow gates always run. A bypass has no stages. A startup failure is not a stage; it is reported in `gate_startup` and as a startup diagnostic.

`data.stage` is retained for document compatibility: it is `"gate"` when a gate decided and `null` for a bypass or a chain with no deciding stage. `data.probability` is always `null`, and `data.observation` is always the absent shape (`outcome: "absent"`, null probability, failure class, and elapsed time), because Cake has no TypeSafe stage. `data.gate` and `data.stages` are authoritative. `data.overridden` is true when any gate's block was overridden by that gate's allowlist; the affected stage also carries `overridden: true`.

Gate failure classes map to the existing exit classes: `timeout`, `network`, `auth`, and `rate_limit` exit `2`; `transport`, `malformed`, `refusal`, `protocol`, `crash`, `unsupported`, and `internal` exit `1`; `config` exits `3`. `crash` and `protocol` are assigned by Cake when the process exits or violates framing; a gate reports the other classes in an error frame. Verdicts, abstentions, allowlist overrides, and a global bypass exit `0`. `--diagnostic` is rejected; use the standalone gate's diagnostic command when needed.

### Trust boundary

Gate executables are trusted user or project configuration. Cake starts them unsandboxed with a minimal environment plus the declared allowlist, and never passes credentials in JSON. A project `.cake/settings.toml` can name a different command, so a gate is only as trustworthy as the checkout and the user's configuration; this is the same trust already granted to hooks and toolbox executables. With no explicit `gates` list, the implicit `safety` gate resolves `cake-safety-judge` through `PATH`, so treat `PATH` as trusted configuration; `cake gates list` shows the selected command and `cake gates doctor` starts it. Neither verifies a still-later replacement of the binary. Cake validates and bounds every gate frame, sanitizes model-visible text, and never executes gate-supplied text. The OS sandbox still runs the approved command; a gate approval is not a sandbox bypass.

## Hook protocol

Hook commands receive one versioned JSON object on stdin. Common fields include `session_id`, `task_id`, `transcript_path`, `cwd`, `hook_event_name`, `model`, and `timestamp`. Tool events also carry tool identity and input; post-tool events carry the result.

`SessionEnd` runs at most once for a root agent invocation after the send result or an early setup failure is known. Its payload adds `reason`, which is `success` when the send succeeds, `error` when setup or the turn fails, and `interrupted` after the first SIGINT or SIGTERM begins graceful shutdown. It runs before the terminal `task_complete` record, preserving task-complete-last ordering, and does not run for subcommands or failures that occur before a valid hook set is loaded. A completed dispatch is never repeated, so a late interrupt cannot double-report. Process crashes, `SIGKILL`, and the second-interrupt hard exit can bypass an in-process hook, and an interrupt that lands while the command is still running kills that subprocess and dispatches `SessionEnd` once more with `interrupted`.

For tool events, `tool_input` is the parsed arguments object a `PreToolUse` decision gates: after Cake's conservative JSON repair pass for tools whose registry entry declares argument repair (Edit, Write, and toolbox tools; the input those executors act on), strict for every other tool, including unregistered names. Repair only escapes raw control characters inside JSON strings when the complete result is valid JSON; it never discards trailing data such as extra objects, braces, or provider markup. `tool_input_json` is the raw argument string as issued. `tool_input` is `{}` only when parsing fails; such a call fails rather than executing.

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

Pre-request failures may abort the invocation. Post-result hooks are best-effort so they cannot replace an existing model or tool outcome. `SessionEnd` is always best-effort: a nonzero exit, invalid JSON, block decision, timeout, or `fail_closed: true` is logged but cannot change the task result, output, or process exit code. Hook stdout and stderr stored in events are bounded.

The [Herdr integration guide](integrations/herdr.md) is a worked example of driving an external lifecycle reporter from these hooks.

## Toolbox protocol

A toolbox tool is one executable implementing two actions selected with the `TOOLBOX_ACTION` environment variable.

### Describe

With `TOOLBOX_ACTION=describe`, the executable prints either JSON or a line-based text description. JSON may use a compact `args` map or a draft 2020-12 `inputSchema` whose top level is an object. Both formats accept an optional `replay` declaration (`"safe"` or `"never"`); missing declarations default to `"never"`. Text uses:

```text
name: run_tests
description: Run the project test suite
path: string? Optional test path
replay: never
```

A `safe` declaration is only a replay hint for future recovery; it does not make a toolbox executable sandboxed or change current repair behavior. Names use letters, numbers, `_`, or `-` and are registered as `tb__<name>`. Descriptions run with bounded time and output. Invalid, duplicate, timed-out, or unencodable tools are skipped.

### Execute

With `TOOLBOX_ACTION=execute`, JSON-described tools receive the raw JSON argument object on stdin. Text-described tools receive `key=value` lines and therefore cannot accept names or string values containing line breaks; Cake rejects those calls rather than changing their meaning.

The process runs in the session working directory with `AGENT=cake` and the session id in `CAKE_THREAD_ID` and `AGENT_THREAD_ID`. Stdout becomes the model tool result. Non-zero status is an error. Execution time, stdout, and stderr are bounded, and timeout or output-cap handling terminates the tool's process group.

Toolbox executables are trusted and run outside the Bash sandbox. Their calls may execute concurrently, and Cake cannot infer mutation targets for same-path serialization.

## Compatibility changes

Changes to session versioning, record names or required fields, stream ordering, exit-code meaning, hook decisions, toolbox framing, gate framing or decisions, or machine-readable stdout are compatibility changes. They require focused serialization or integration tests, migration analysis where applicable, and an update to this document's semantics.

A settings file that cannot be loaded or parsed exited `1`; it now exits `3`, the class this document already assigns to invalid configuration. Every command that loads settings reports the failure with that code: `cake <prompt>`, `cake debug models`, `cake debug models --json`, `cake debug skills`, and `cake bash check` in both output modes. A script that branched on `1` for a broken settings file must branch on `3` instead; the stderr diagnostic is unchanged. See [ADR 044](adr/044-classify-settings-failures-as-input-errors.md).

`debug models --json` and `sessions list --json` emitted a bare JSON array before the diagnostic document above; they now emit the document, and `bash check` gained `--json` ([ADR 029](adr/029-diagnostic-json-document.md)). Consumers that parsed the bare array read `data.models` or `data.sessions` instead. No default behavior changed: the affected flags are explicitly opt-in.

An exhausted HTTP `408` request timeout from a provider now exits `2` instead of `1`, matching the client-side timeouts that already did ([ADR 045](adr/045-classify-request-timeouts-as-api-errors.md)). Scripts that branch on `$?` must treat `2` as the request-timeout class. The mapping depends on the status code the provider supplies, and every other status stays where it was: `400`, `404`, `409`, `5xx`, and `529` still exit `1`.

`cake --fail-on-error` adds an opt-in strict exit contract for `stream-json` ([ADR 046](adr/046-opt-in-strict-exit-codes-for-stream-json.md)). Exit-code meaning grows, but no default behavior changes: without the flag, an in-stream agent, provider, or tool failure still exits `0`, so every existing consumer keeps working. A consumer that adopts the flag reads a nonzero exit where it previously read `0`, which is the point of opting in.

The `cake-gate/1` contract adds `gate_startup`, `gate_decision`, and `gate_evidence` sidecar records and the additive `bash check` fields `data.gate` and `data.stages` ([ADR 049](adr/049-external-policy-gates-with-the-cake-gate-1-protocol.md)). The generic gate records are authoritative, while the compatibility `judge_*` compensation events, document fields, and exit codes keep their meanings. `CAKE_JUDGE=off` remains an alias for `CAKE_GATE=off`. A resolved pipeline with no `bash.exec` pre-tool gate is a configuration failure: `cake bash check` exits `3`, and the Bash tool is not offered at all. There is no in-process fallback; a missing `cake-safety-judge` executable is a startup failure. Consumers should tolerate historical `judge_attempt` and `type_safe_shadow` records in old sidecars and read `gate_decision` for current decisions.

## Related decisions

- [ADR 004](adr/004-append-only-session-task-events.md), append-only task events.
- [ADR 005](adr/005-command-hooks.md), command hooks.
- [ADR 007](adr/007-per-session-telemetry-sidecar.md), the telemetry sidecar.
- [ADR 017](adr/017-trusted-executable-toolbox-tools.md), trusted toolbox executables.
- [ADR 021](adr/021-session-transcript-replay.md), session transcript replay.
- [ADR 029](adr/029-diagnostic-json-document.md), the diagnostic JSON document.
- [ADR 044](adr/044-classify-settings-failures-as-input-errors.md), the exit class for a settings load or parse failure.
- [ADR 049](adr/049-external-policy-gates-with-the-cake-gate-1-protocol.md), external policy gates and the `cake-gate/1` protocol.
