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

`stream-json` is newline-delimited JSON. Each line is one `StreamRecord` serialized with a top-level `type` discriminator. The stream schema is intentionally the same as the per-task portion of a v4 session file. The field-level contracts for the shared records --- `task_start`, `message`, `function_call`, `function_call_output`, `reasoning`, `hook_event`, and `task_complete` --- are documented once, in [session-management.md](./session-management.md#schema). Differences from a session file:

- `session_meta` is never emitted.
- Session-only audit records (`prompt_context`, `skill_activated`) are never emitted.
- Only the current task's records appear; prior tasks are not replayed.

All timestamps are UTC RFC 3339 strings. `session_id` and `task_id` are UUID strings. Optional fields are omitted when absent.

### Malformed tool arguments

`function_call.arguments` is the raw tool argument string produced by the model. When the model emits malformed JSON, `arguments_parse_error` is included so consumers can detect the condition without failing to parse the enclosing stream-json record. Consumers that parse `arguments` as JSON should treat `arguments_parse_error` as a signal that the nested parse will fail.

### Hook event ordering

Hook events are emitted live in execution order. Session and user-prompt hooks appear before the task/model records they precede. Tool hooks appear around the associated tool call: pre-tool hooks before the tool result, and post-tool or failure hooks after the tool output or failure they observe.

Hook stdout and stderr may include local paths, policy diagnostics, or other project-specific details produced by hook commands.

### Task completion and errors

`task_complete.subtype` is one of `success`, `error_during_execution`, `error_output_schema`, `cut_off`, or `interrupted`. On failure, `error` contains the error message and `result` is omitted.

With `--output-schema`, a final response that cannot be made valid against the schema (refusal, truncation, or correction exhaustion) fails with subtype `error_output_schema` and process exit code 1. The validation detail is in `error` and `result` is omitted:

```json
{"type":"task_complete","subtype":"error_output_schema","is_error":true,"duration_ms":6100,"turn_count":3,"tool_call_count":0,"session_id":"550e8400-e29b-41d4-a716-446655440000","task_id":"2b15f29d-8c42-4c53-9bdf-35c8f2390d3e","error":"Final response did not conform to the output schema after 3 attempt(s): \"summary\" is a required property","usage":{"input_tokens":150,"input_tokens_details":{"cached_tokens":50},"output_tokens":320,"output_tokens_details":{"reasoning_tokens":120},"total_tokens":470}}
```

The subtype is additive: consumers keying on `is_error` are unaffected, and consumers that distinguish schema exhaustion from other agent errors can match `subtype == "error_output_schema"`.

A turn that ends without a final assistant message (for example, the model stops mid-reasoning or returns an empty response) fails with subtype `cut_off` and `is_error: true`. The explanation is in `error` and `result` is omitted. Stream-json keeps its in-stream-error policy for cut-offs: the process exits 0. This differs from `json` and `text` output, where a cut-off exits 1.

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
