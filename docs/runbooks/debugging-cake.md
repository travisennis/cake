# Debugging Failed Cake Runs

Use this runbook for **fast, reactive triage** of the user's most recent failed cake run: the CLI returned `None`, empty output, or a clearly truncated response; it reported `Tool error:` with no further detail; a task crashed, hung, or was interrupted mid-stream; or the user reports their last cake run "broke". The goal is to identify what broke, not to produce a full session analysis report.

This runbook is one of three failure procedures; do not invoke them for each other's cases:

- For deeper structural analysis of a session (issue categories, quality assessment, improvement recommendations), use the [Analyzing Cake Sessions runbook](analyzing-cake-sessions/index.md).
- For `Operation not permitted (os error 1)` or other sandbox-denied operations, use the [Debugging Sandbox Denials runbook](debugging-sandbox.md).

Triage reads two contracts first: exit codes and persisted-session layout and record semantics. [Integration contracts](../integrations.md) owns both; this runbook restates only what triage needs and links there for the rest. For how Cake handles an interrupt (Ctrl-C) and graceful shutdown, see [ADR 011](../adr/011-interrupt-handling.md) rather than relying on the summary here.

## Step 1: Find the Failing Session

The newest file may be the session running this investigation. This happens when a probe launches cake from inside another cake run. Inspect the first user message in the newest few files before choosing a target; do not let the probe select itself.

```bash
SESSION_DIR="${CAKE_DATA_DIR:-$HOME/.local/share/cake}/sessions"

# Inspect the newest three candidates before launching any cake probe.
for candidate in $(ls -t "$SESSION_DIR"/*.jsonl 2>/dev/null | head -3); do
  printf '\n%s\n' "$candidate"
  prompt="$(jq -r 'select(.type == "message" and .role == "user") | .content' \
    "$candidate" 2>/dev/null | head -1)"
  printf '%s\n' "${prompt:-"(no user message)"}"
done
```

Compare each first user message with the task you are investigating. If the newest file contains the probe's own request, skip it and choose the matching older file. Set `LATEST` to that explicit target, then snapshot it before launching the probe or running further analysis:

```bash
LATEST="/absolute/path/to/the-selected-session.jsonl"
TARGET_SNAPSHOT="${TMPDIR:-/tmp}/cake-session-target.$$.jsonl"
cp "$LATEST" "$TARGET_SNAPSHOT"
LATEST="$TARGET_SNAPSHOT"
echo "$LATEST"
```

Analyze the snapshot through the rest of this runbook. If `$CAKE_DATA_DIR` is set, sessions live under `$CAKE_DATA_DIR/sessions/`.

## Step 2: Check How the Session Ended

A complete invocation ends with `task_complete`. Anything else means the task did not finish cleanly.

```bash
tail -1 "$LATEST" | jq '{type, is_error, subtype, error}'
```

Interpretation:

  | Last record type                                                   | Meaning                                                      |
  | ------------------------------------------------------------------ | ------------------------------------------------------------ |
  | `task_complete` (no error)                                         | Task finished normally — issue is in the result, not the run |
  | `task_complete` (`is_error`)                                       | Task ended with a recorded error — read `.error`             |
  | `reasoning` / `function_call` / `function_call_output` / `message` | Task was interrupted mid-stream (timeout, crash, signal)     |
  | `task_start`                                                       | Task never produced any output                               |

An interrupted run (Ctrl-C) is the one interruption that does end with `task_complete`: since the graceful-shutdown change, Cake writes an `Interrupted` outcome and exits `130`. See [ADR 011](../adr/011-interrupt-handling.md) for that path and [Integration contracts](../integrations.md) for exit-code meanings.

## Step 3: Look at the Last Few Records

```bash
tail -5 "$LATEST" | jq '.'
```

This usually reveals: the last tool the model invoked, the last output it saw, or where the reasoning trailed off.

## Step 4: Check Today's Log

```bash
tail -100 ~/.cache/cake/cake.$(date +%Y-%m-%d).log | grep -iE "error|warn|truncat"
```

Common patterns:

- `output truncated` --- a tool output exceeded the cap
- API errors --- provider returned an error or timed out
- stream interruption --- connection dropped mid-response
- panics --- cake itself crashed (see the panic message)

## Step 5: Check Telemetry for Retries and Timing

```bash
SESSION_ID="$(head -1 "$LATEST" | jq -r '.session_id')"
TELEMETRY="$HOME/.cache/cake/session-telemetry/$SESSION_ID.ndjson"

# Retry decisions (model retries with backoff)
jq 'select(.type == "retry_scheduled") | {attempt, reason, delay_ms, detail}' "$TELEMETRY"

# Per-tool durations
jq 'select(.type == "tool_call") | {turn_index, name, duration_ms, output_bytes, was_error}' "$TELEMETRY"

# Final session summary (if present)
jq 'select(.type == "session_summary")' "$TELEMETRY"
```

Telemetry is **not** resumable conversation history; it is a separate performance sidecar.

## Step 6: Correlate Session and Log

```bash
SESSION_ID="$(head -1 "$LATEST" | jq -r '.session_id')"
grep "$SESSION_ID" ~/.cache/cake/cake.*.log
```

## Why "None" Happens

`None` or empty output almost always means **no completed assistant result was produced** before the session ended. Typical causes:

- Model hit token limits mid-response
- Response or streaming connection timed out
- Process was interrupted (signal, panic, crash)
- A tool call hung and never returned

When this happens, the session file ends without a `task_complete` record (or `task_complete` is present with `is_error: true`).

## Continuing or Resuming

```bash
# Continue the latest session in the current directory
./target/release/cake --continue "Try again"

# Resume a specific session by UUID (not file path)
./target/release/cake --resume {uuid} "Continue"
```

## Worked Example: Diagnosing a "None" Output

User reports: "I ran cake and it just printed `None`."

```bash
$ LATEST=$(ls -t ~/.local/share/cake/sessions/*.jsonl | head -1)
$ tail -1 "$LATEST" | jq '{type, is_error, subtype, error}'
{
  "type": "reasoning",
  "is_error": null,
  "subtype": null,
  "error": null
}
```

Last record is `reasoning`, not `task_complete` → task was interrupted mid-stream.

```bash
$ tail -3 "$LATEST" | jq '{type, name: .name, content: (.content // .arguments)[0:120]}'
{ "type": "function_call_output", "name": null, "content": "...build finished in 4.2s\n" }
{ "type": "function_call", "name": "bash", "content": "{\"cmd\":\"cargo test --release --all\"}" }
{ "type": "reasoning", "name": null, "content": "Now I need to verify the integration tests pass before" }
```

Model was reasoning about running tests when the stream ended.

```bash
$ grep -iE "error|timeout|truncat" ~/.cache/cake/cake.$(date +%Y-%m-%d).log | tail -5
2026-05-21T14:32:18Z ERROR cake::clients::responses: stream error: connection reset by peer
2026-05-21T14:32:18Z WARN  cake::session: task ended without task_complete; session may be incomplete
```

**Diagnosis**: Streaming connection dropped during the model's response.

**Next step for the user**: `cake --continue "Continue where you left off"` will reload the partial session and let the model finish.

## File Locations

  | File                                            | Purpose                                              |
  | ----------------------------------------------- | ---------------------------------------------------- |
  | `~/.local/share/cake/sessions/{uuid}.jsonl`     | Session files (or `$CAKE_DATA_DIR/sessions/`)        |
  | `~/.cache/cake/session-telemetry/{uuid}.ndjson` | Per-session telemetry (timings, retries)             |
  | `~/.cache/cake/cake.YYYY-MM-DD.log`             | Daily logs (or `$CAKE_DATA_DIR/cake.YYYY-MM-DD.log`) |

## When to Switch Procedures

- For full session review, scoring, or recommendations on what cake should change → load the [Analyzing Cake Sessions runbook](analyzing-cake-sessions/index.md).
- For `Operation not permitted (os error 1)` or other sandbox-denied operations → load the [Debugging Sandbox Denials runbook](debugging-sandbox.md).
- For the full JSONL record contract, format version 4 schema, and the LLM-visible vs. audit-only distinction → see [Integration contracts](../integrations.md) and the [Analyzing Cake Sessions runbook](analyzing-cake-sessions/index.md).
