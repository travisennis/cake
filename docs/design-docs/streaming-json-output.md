# `--output-format stream-json`

`--output-format stream-json` emits newline-delimited JSON records for the current task as they happen. It is a live progress feed, not a complete session file.

## Usage

```bash
cake --output-format stream-json "Your prompt here"
```

The final plain-text assistant response is not printed in this mode. Consumers should read the streamed `message` and `task_complete` records.

## Behavior

The stream starts with `task_start`, includes the current task's hook and conversation records, and ends with `task_complete`.

It never emits `session_meta`. It also does not replay prior tasks when used with `--continue`, `--resume`, or `--fork`.

Redirected stream-json output cannot be resumed by path. Use the persisted session UUID with `--resume <uuid>`.

## Schema

`stream-json` is newline-delimited JSON. Each line is one `StreamRecord` serialized with a top-level `type` discriminator. The stream schema is intentionally the same as the per-task portion of a v4 session file, with one exclusion: `session_meta` is never emitted.

All timestamps are UTC RFC 3339 strings. `session_id` and `task_id` are UUID strings. Optional fields are omitted when absent.

### Task Start

```json
{"type":"task_start","session_id":"550e8400-e29b-41d4-a716-446655440000","task_id":"2b15f29d-8c42-4c53-9bdf-35c8f2390d3e","timestamp":"2026-05-03T12:00:01Z"}
```

  | Field        | Type   | Required | Description               |
  | ------------ | ------ | -------- | ------------------------- |
  | `type`       | string | yes      | Always `task_start`       |
  | `session_id` | string | yes      | Session UUID for this run |
  | `task_id`    | string | yes      | UUID for this invocation  |
  | `timestamp`  | string | yes      | Task start time           |

### Message

```json
{"type":"message","role":"assistant","content":"Hello!","id":"msg_abc123","status":"completed"}
```

Roles are `system`, `user`, `assistant`, or `tool`. `id`, `status`, and `timestamp` are optional.

  | Field       | Type   | Required | Description                                         |
  | ----------- | ------ | -------- | --------------------------------------------------- |
  | `type`      | string | yes      | Always `message`                                    |
  | `role`      | string | yes      | One of `system`, `user`, `assistant`, or `tool`     |
  | `content`   | string | yes      | Plain text message content                          |
  | `id`        | string | no       | Provider message id                                 |
  | `status`    | string | no       | Provider status such as `completed` or `incomplete` |
  | `timestamp` | string | no       | Item creation time                                  |

### Function Call

```json
{"type":"function_call","id":"fc_abc123","call_id":"call_xyz789","name":"bash","arguments":"{\"command\":\"ls\"}"}
```

  | Field       | Type   | Required | Description                                |
  | ----------- | ------ | -------- | ------------------------------------------ |
  | `type`      | string | yes      | Always `function_call`                     |
  | `id`        | string | yes      | Provider function-call item id             |
  | `call_id`   | string | yes      | Correlation id used by the matching output |
  | `name`      | string | yes      | Tool name                                  |
  | `arguments` | string | yes      | JSON-encoded tool argument string          |
  | `timestamp` | string | no       | Item creation time                         |

### Function Call Output

```json
{"type":"function_call_output","call_id":"call_xyz789","output":"Cargo.toml\nsrc"}
```

  | Field       | Type   | Required | Description                                          |
  | ----------- | ------ | -------- | ---------------------------------------------------- |
  | `type`      | string | yes      | Always `function_call_output`                        |
  | `call_id`   | string | yes      | Matches the preceding `function_call.call_id`        |
  | `output`    | string | yes      | Tool output or tool error text returned to the model |
  | `timestamp` | string | no       | Item creation time                                   |

### Reasoning

```json
{"type":"reasoning","id":"reason_abc123","summary":["Inspecting the project"],"encrypted_content":"gAAAAABencrypted..."}
```

`encrypted_content` and `content` are optional and are preserved for reasoning model round-tripping.

  | Field               | Type             | Required | Description                                |
  | ------------------- | ---------------- | -------- | ------------------------------------------ |
  | `type`              | string           | yes      | Always `reasoning`                         |
  | `id`                | string           | yes      | Provider reasoning item id                 |
  | `summary`           | array of strings | yes      | Human-readable reasoning summaries         |
  | `encrypted_content` | string           | no       | Opaque provider content for round-tripping |
  | `content`           | array of objects | no       | Provider reasoning content array           |
  | `timestamp`         | string           | no       | Item creation time                         |

Each `content` item has a required `type` string and an optional `text` string.

### Hook Event

```json
{"type":"hook_event","timestamp":"2026-05-03T12:00:02Z","task_id":"2b15f29d-8c42-4c53-9bdf-35c8f2390d3e","event":"PreToolUse","source":"Bash","call_id":"call_xyz789","tool_name":"Bash","tool_input_summary":"ls","source_file":"/Users/user/project/.cake/hooks.json","command":"./check-tool.sh","exit_code":0,"duration_ms":21,"decision":"none","resolved_decision":"allow","fail_closed":false,"stdout":"","stderr":""}
```

Hook events are emitted live in execution order. Session and user-prompt hooks appear before the task/model records they precede. Tool hooks appear around the associated tool call: pre-tool hooks before the tool result, and post-tool or failure hooks after the tool output or failure they observe.

  | Field                | Type   | Required | Description                                                                 |
  | -------------------- | ------ | -------- | --------------------------------------------------------------------------- |
  | `type`               | string | yes      | Always `hook_event`                                                         |
  | `timestamp`          | string | yes      | Record creation time                                                        |
  | `task_id`            | string | yes      | UUID for the invocation that ran the hook                                   |
  | `event`              | string | yes      | Hook lifecycle event, such as `PreToolUse` or `PostToolUse`                 |
  | `source`             | string | no       | Hook matcher source, such as a tool name                                    |
  | `call_id`            | string | no       | Tool call id for tool hooks; matches `function_call.call_id`                |
  | `tool_name`          | string | no       | Tool name for tool hooks                                                    |
  | `tool_input_summary` | string | no       | Compact, capped tool input summary for quick inspection                     |
  | `source_file`        | string | yes      | Hook configuration file that defined the command                            |
  | `command`            | string | yes      | Hook shell command                                                          |
  | `exit_code`          | number | no       | Process exit code when available                                            |
  | `duration_ms`        | number | yes      | Hook process duration in milliseconds                                       |
  | `decision`           | string | yes      | Historical coarse decision label preserved for compatibility                |
  | `resolved_decision`  | string | no       | Effective parsed decision, such as `allow`, `deny`, `stop`, `error`, `none` |
  | `fail_closed`        | bool   | yes      | Whether a hook execution error blocks the associated action                 |
  | `stdout`             | string | yes      | Captured hook stdout, capped at 64 KiB                                      |
  | `stderr`             | string | yes      | Captured hook stderr, capped at 64 KiB                                      |

Hook stdout and stderr may include local paths, policy diagnostics, or other project-specific details produced by hook commands.

### Task Complete

```json
{"type":"task_complete","subtype":"success","is_error":false,"duration_ms":1523,"turn_count":2,"tool_call_count":3,"session_id":"550e8400-e29b-41d4-a716-446655440000","task_id":"2b15f29d-8c42-4c53-9bdf-35c8f2390d3e","result":"Done","usage":{"input_tokens":150,"input_tokens_details":{"cached_tokens":50},"output_tokens":320,"output_tokens_details":{"reasoning_tokens":120},"total_tokens":470}}
```

`subtype` is one of `success`, `error_during_execution`, `error_output_schema`, `cut_off`, or `interrupted`. On failure, `error` contains the error message and `result` is omitted.

  | Field                | Type             | Required | Description                                                                         |
  | -------------------- | ---------------- | -------- | ----------------------------------------------------------------------------------- |
  | `type`               | string           | yes      | Always `task_complete`                                                              |
  | `subtype`            | string           | yes      | One of `success`, `error_during_execution`, `error_output_schema`, `cut_off`, or `interrupted` |
  | `is_error`           | boolean          | yes      | `false` for successful completion                                                   |
  | `duration_ms`        | number           | yes      | Wall-clock task duration in milliseconds                                            |
  | `turn_count`         | number           | yes      | Number of API turns with usage accumulated                                          |
  | `tool_call_count`    | number           | yes      | Number of tool calls executed during the task                                       |
  | `session_id`         | string           | yes      | Session UUID                                                                        |
  | `task_id`            | string           | yes      | Task UUID from the matching `task_start`                                            |
  | `result`             | string           | no       | Final assistant text on success                                                     |
  | `error`              | string           | no       | Error message on failure                                                            |
  | `usage`              | object           | yes      | Aggregate token usage for the task                                                  |
  | `permission_denials` | array of strings | no       | Tool permission denial messages when present                                        |

`usage` contains `input_tokens`, `input_tokens_details.cached_tokens`, `output_tokens`, `output_tokens_details.reasoning_tokens`, and `total_tokens`.

With `--output-schema`, a final response that cannot be made valid against the schema (refusal, truncation, or correction exhaustion) fails with subtype `error_output_schema` and process exit code 1. The validation detail is in `error` and `result` is omitted:

```json
{"type":"task_complete","subtype":"error_output_schema","is_error":true,"duration_ms":6100,"turn_count":3,"tool_call_count":0,"session_id":"550e8400-e29b-41d4-a716-446655440000","task_id":"2b15f29d-8c42-4c53-9bdf-35c8f2390d3e","error":"Final response did not conform to the output schema after 3 attempt(s): \"summary\" is a required property","usage":{"input_tokens":150,"input_tokens_details":{"cached_tokens":50},"output_tokens":320,"output_tokens_details":{"reasoning_tokens":120},"total_tokens":470}}
```

The subtype is additive: consumers keying on `is_error` are unaffected, and consumers that distinguish schema exhaustion from other agent errors can match `subtype == "error_output_schema"`.

A turn that ends without a final assistant message (for example, the model stops mid-reasoning or returns an empty response) fails with subtype `cut_off` and `is_error: true`. The explanation is in `error` and `result` is omitted. Stream-json keeps its in-stream-error policy for cut-offs: the process exits 0. This differs from `json` and `text` output, where a cut-off exits 1:

```json
{"type":"task_complete","subtype":"cut_off","is_error":true,"duration_ms":4210,"turn_count":3,"tool_call_count":2,"session_id":"550e8400-e29b-41d4-a716-446655440000","task_id":"2b15f29d-8c42-4c53-9bdf-35c8f2390d3e","error":"The model's response was cut off during reasoning.","usage":{"input_tokens":150,"input_tokens_details":{"cached_tokens":50},"output_tokens":320,"output_tokens_details":{"reasoning_tokens":120},"total_tokens":470}}
```

## Consumer Guidance

Consumers should treat records as an event stream and should not assume a `task_complete` record will arrive after crashes, process termination, or broken pipes. Use `task_id` to group events from one invocation and `call_id` to join tool calls, hook events, and tool outputs.

Consumers must ignore unknown `type` values and unknown fields for forward compatibility.

Do not use redirected stream-json output as a session archive. Persisted sessions are the files under `~/.local/share/cake/sessions/` or `$CAKE_DATA_DIR/sessions/`.

## Example

```json
{"type":"task_start","session_id":"550e8400-e29b-41d4-a716-446655440000","task_id":"2b15f29d-8c42-4c53-9bdf-35c8f2390d3e","timestamp":"2026-05-03T12:00:01Z"}
{"type":"message","role":"user","content":"List files in the current directory"}
{"type":"function_call","id":"fc_001","call_id":"call_001","name":"bash","arguments":"{\"command\":\"ls\"}"}
{"type":"hook_event","timestamp":"2026-05-03T12:00:02Z","task_id":"2b15f29d-8c42-4c53-9bdf-35c8f2390d3e","event":"PreToolUse","source":"Bash","call_id":"call_001","tool_name":"bash","tool_input_summary":"ls","source_file":"/Users/user/project/.cake/hooks.json","command":"./check-tool.sh","exit_code":0,"duration_ms":21,"decision":"none","resolved_decision":"allow","fail_closed":false,"stdout":"","stderr":""}
{"type":"function_call_output","call_id":"call_001","output":"Cargo.toml\nsrc"}
{"type":"message","role":"assistant","content":"The current directory contains Cargo.toml and src.","id":"msg_001","status":"completed"}
{"type":"task_complete","subtype":"success","is_error":false,"duration_ms":1523,"turn_count":2,"tool_call_count":1,"session_id":"550e8400-e29b-41d4-a716-446655440000","task_id":"2b15f29d-8c42-4c53-9bdf-35c8f2390d3e","result":"The current directory contains Cargo.toml and src.","usage":{"input_tokens":150,"input_tokens_details":{"cached_tokens":50},"output_tokens":320,"output_tokens_details":{"reasoning_tokens":120},"total_tokens":470}}
```
