# CLI Module

The CLI module provides the command-line interface for cake, handling argument parsing and user-facing error messages.

## Overview

The CLI layer is intentionally thin---it delegates all business logic to lower layers while handling:

- **Argument parsing**: Using `clap` to define and validate command-line flags
- **User interaction**: Reading from stdin, handling worktrees, and formatting output
- **Session lifecycle**: Managing session creation, continuation, resumption, and forking

## Architecture

### Command Structure

The primary prompt-running CLI is implemented by the `CodingAssistant` clap struct in `src/main.rs`. The authoritative flag set --- names, aliases, defaults, value parsing, and help text --- is that struct's clap derive attributes, surfaced to users via `cake --help` and the README. This document covers the semantics that clap cannot express.

The CLI also accepts optional top-level subcommands for introspection commands that should exit before agent/session setup:

```text
cake [OPTIONS] [PROMPT]
cake debug models
cake sessions list
```

The prompt path remains the default when no subcommand is provided. Top-level subcommands live under `src/cli/` and implement the `CmdRunner` trait --- the single interface every command implements --- so they can reuse the main command dispatch without running an agent turn.

### Toolbox Directories

`--toolbox <DIR>` appends a directory to the user-defined tool search path and may be repeated. Relative paths are resolved against the invocation directory before any `--worktree` cwd change. Environment-driven directories come from colon-separated `CAKE_TOOLBOX`; when it is unset cake scans `~/.config/cake/tools`, and an empty value disables that default. Toolbox executables are trusted and unsandboxed; see [tools.md](./tools.md#toolbox-tools) and [sandbox.md](./sandbox.md#toolbox-trust-boundary).

### Debug Models

`cake debug models` loads merged settings for the current directory and prints configured model metadata to stdout. It displays the configured API key environment variable name (`api_key_env`) but does not resolve or display API key values.

### Model Configuration

Model selection is settings-driven: `--model <name>` selects a named model from `settings.toml`, `--profile <name>` applies a behavior overlay, and reasoning flags (`--reasoning-effort`, `--reasoning-budget`) override per-invocation. If `--model` is not provided, cake uses the configured `default_model`; if no `default_model` is configured, cake exits with setup instructions --- there is no built-in default model.

The settings file format, merge behavior, profile precedence, and validation rules are documented in [settings.md](./settings.md), the single home for that contract.

## Session Management

The CLI handles four session modes:

1. **New Session** (default): Creates a fresh session with a new UUID
2. **Continue** (`--continue`): Loads the most recent session for the current directory
3. **Resume** (`--resume <UUID>`): Loads a specific session by UUID
4. **Fork** (`--fork [UUID]`): Copies history from an existing session into a new session

These modes are mutually exclusive---only one can be used at a time. Lifecycle details and storage are in [session-management.md](./session-management.md).

## Input Sources

The CLI accepts input from multiple sources:

1. **`[PROMPT]`**: Positional argument for the prompt (use `-` to read from stdin)
2. **stdin**: Pipe input or use heredocs for multi-line prompts

The prompt and stdin can be combined. When both are present, cake sends them as labeled sections so the prompt remains the user request and stdin remains supplied input.

### Examples

```bash
# Positional prompt
cake "Implement a binary search tree"

# Read from stdin
cat file.txt | cake "Summarize this"

# Heredoc
cake << 'EOF'
Implement a function that:
1. Takes a list of numbers
2. Returns the sum
EOF

# Explicit stdin with dash
echo "Hello" | cake -
```

## Output Formats

Three output formats are supported:

- **`text`** (default): Human-readable text output. Progress is streamed to stderr while the final assistant message is printed to stdout. The final progress line includes the session ID along with duration, turn count, and token usage.
- **`stream-json`**: Machine-readable JSON streaming with events for each conversation item as they occur. Useful for building frontends that consume cake output live. See [streaming-json-output.md](./streaming-json-output.md).
- **`json`**: A single JSON object printed at completion containing the result, session metadata, token usage, working directory, session file path, turn count, and elapsed time. Designed for scripting and CI integration where a structured summary is needed rather than a live stream.

When using `stream-json` or `json`, console progress reporting (spinner) is automatically suppressed to avoid polluting stdout.

## Schema-Constrained Final Output

`--output-schema <path>` constrains the final response to a single JSON document that validates against a caller-supplied JSON Schema file (draft 2020-12, as implemented by the `jsonschema` crate). Schemas must be self-contained: external `$ref` resolution is disabled, and a schema referencing remote or file resources fails to compile.

```bash
cake --output-schema review.schema.json --output-format stream-json "Review this diff"
```

Only the final response is constrained. The run remains fully agentic --- tool use, reasoning, and intermediate messages are unchanged --- and the schema requirement is injected as developer context so the model aims for conforming output on its own. When the final (no-tool-call) message does not validate, cake runs at most two corrective turns with tools disabled and the provider's native structured-output constraint attached (falling back to unconstrained retries if the provider rejects the constrained request with HTTP 400). Local validation is authoritative in all cases.

On success, the final response is exactly the schema-valid JSON document with no Markdown fences or surrounding prose:

- **`text`**: stdout is exactly the JSON document.
- **`json`**: the top-level `result` field remains a JSON string containing the document.
- **`stream-json`**: the `task_complete` record's `result` is the document.

The flag composes with `--continue`, `--resume`, and `--fork`. The schema is per-invocation and is not persisted to the session; corrective turns are ordinary conversation items, so resumed sessions replay cleanly.

Failure behavior:

- An unreadable or invalid schema file fails before the run starts (no `task_start` is emitted, no worktree is created) with a clear error on stderr and exit code 3.
- A final response that cannot be made schema-valid (refusal, truncation, correction exhaustion) emits a `task_complete` record with subtype `error_output_schema` and `is_error: true`, with validation detail in `error`, and exits 1. Callers never receive a successful `task_complete.result` containing non-conforming prose.

## Exit Codes

cake returns structured exit codes so that shell scripts and CI pipelines can branch on the reason for failure:

  | Code | Name        | Description                                               |
  | ---- | ----------- | --------------------------------------------------------- |
  | `0`  | Success     | The agent completed and produced a response               |
  | `1`  | Agent error | The model or a tool encountered an error during execution |
  | `2`  | API error   | Rate limit, auth failure, or network error                |
  | `3`  | Input error | No prompt provided, invalid flags, missing API key        |

### Classification Logic

The `exit_code` module classifies `anyhow::Error` values by inspecting the error chain:

1. **Input errors** (exit 3): Missing environment variables, no input provided, invalid model names, invalid session UUIDs, and clap argument errors.
2. **API errors** (exit 2): `reqwest::Error` downcasts (auth, connect, timeout, request errors) and rate-limit/authentication message patterns.
3. **Agent/tool errors** (exit 1): The default for any error not matching the above categories.

The `main()` function returns `std::process::ExitCode` directly (not `anyhow::Result`), classifying errors before exiting. The authoritative pattern list is the `classify` function in `exit_code` and its tests.

### Streaming JSON Integration

When using `--output-format stream-json`, the task completion event reports success or failure. The process exit code is still returned by the shell and is not embedded in the JSON record:

```json
{"type":"task_complete","subtype":"success","is_error":false,...}
{"type":"task_complete","subtype":"error_during_execution","is_error":true,"error":"...",...}
{"type":"task_complete","subtype":"error_output_schema","is_error":true,"error":"...",...}
{"type":"task_complete","subtype":"cut_off","is_error":true,"error":"...",...}
```

### JSON Summary Output

When using `--output-format json`, a single JSON object is printed at the end of the run:

```json
{
  "result": "The assistant response text",
  "session_id": "550e8400-e29b-41d4-a716-446655440000",
  "usage": {
    "input_tokens": 1234,
    "input_tokens_details": { "cached_tokens": 200 },
    "output_tokens": 567,
    "output_tokens_details": { "reasoning_tokens": 100 },
    "total_tokens": 1801
  },
  "cwd": "/home/user/project",
  "session_file": "/home/user/.local/share/cake/sessions/550e8400-e29b-41d4-a716-446655440000.jsonl",
  "turns": 3,
  "elapsed_time": 4500
}
```

On error, `result` is `null` and an `error` field is included with the error message. The error is then propagated to produce a non-zero exit code.

A cut-off turn --- the model produced no final assistant message (for example, it stopped mid-reasoning or returned an empty response) --- is reported as an error, never as a `result`. The object additionally carries an additive `"subtype": "cut_off"` field so consumers can distinguish cut-offs from other agent errors (parity with stream-json's `task_complete` subtype), and the process exits 1:

```json
{
  "result": null,
  "error": "The model's response was cut off during reasoning.",
  "subtype": "cut_off",
  ...
}
```

In `text` output, a cut-off prints `Error: <detail>` on stderr and exits 1; the explanation never appears in the assistant-output position on stdout. In `stream-json` output, cut-offs follow the in-stream-error policy and exit 0 (see Streaming JSON Integration above).

## Related Documentation

- [prompts.md](./prompts.md): System prompt construction and AGENTS.md integration
- [session-management.md](./session-management.md): Session lifecycle and storage
- [settings.md](./settings.md): `settings.toml` format, merge behavior, and profiles
