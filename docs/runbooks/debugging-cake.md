# Debugging Failed Cake Runs

Use this runbook for **fast, reactive triage** of the user's most recent failed cake run: the CLI returned `None`, empty output, or a clearly truncated response; it reported `Tool error:` with no further detail; a task crashed, hung, or was interrupted mid-stream; or the user reports their last cake run "broke". The goal is to identify what broke, not to produce a full session analysis report.

This runbook is one of three failure procedures; do not invoke them for each other's cases:

- For deeper structural analysis of a session (issue categories, quality assessment, improvement recommendations), use the [Analyzing Cake Sessions runbook](analyzing-cake-sessions/index.md).
- For `Operation not permitted (os error 1)` or other sandbox-denied operations, use the [Debugging Sandbox Denials runbook](debugging-sandbox.md).

Triage reads two contracts first: exit codes and persisted-session layout and record semantics. [Integration contracts](../integrations.md) owns both; this runbook restates only what triage needs and links there for the rest. For how Cake handles an interrupt (Ctrl-C) and graceful shutdown, see [ADR 011](../adr/011-interrupt-handling.md) rather than relying on the summary here.

## Step 1: Find the Failing Session

The newest file may be the session running this investigation. This happens when a probe launches cake from inside another cake run. Inspect the first user message in the newest few files before choosing a target; do not let the probe select itself.

Set the storage roots first. `CAKE_DATA_DIR` relocates the cache, logs, telemetry, and sessions together; without it, sessions and cache use their normal separate locations.

```bash
if [ -n "${CAKE_DATA_DIR:-}" ]; then
  SESSION_DIR="$CAKE_DATA_DIR/sessions"
  CACHE_DIR="$CAKE_DATA_DIR"
else
  SESSION_DIR="$HOME/.local/share/cake/sessions"
  CACHE_DIR="$HOME/.cache/cake"
fi
TELEMETRY_DIR="$CACHE_DIR/session-telemetry"
LOG_DIR="$CACHE_DIR"

# Inspect the newest three candidates before launching any cake probe. Read
# each line as one path so spaces in the data-root or filename are preserved.
ls -t "$SESSION_DIR"/*.jsonl 2>/dev/null | head -3 |
while IFS= read -r candidate; do
  printf '\n%s\n' "$candidate"
  prompt="$(jq -r 'select(.type == "message" and .role == "user") | .content' \
    "$candidate" 2>/dev/null | head -1)"
  printf '%s\n' "${prompt:-"(no user message)"}"
done
```

Compare each first user message with the task you are investigating. If the newest file contains the probe's own request, skip it and choose the matching older file. Set `LATEST` to that explicit target. Before snapshotting or classifying it, check that no Cake process still has the target open:

```bash
LATEST="/absolute/path/to/the-selected-session.jsonl"

if command -v lsof >/dev/null 2>&1; then
  lsof_status=0
  PIDS="$(lsof -t "$LATEST" 2>/dev/null)" || lsof_status=$?
  if [ "$lsof_status" -gt 1 ]; then
    echo 'Cannot inspect the session file with lsof; liveness is unknown.' >&2
    exit 1
  fi
elif command -v fuser >/dev/null 2>&1; then
  fuser_status=0
  PIDS="$(fuser "$LATEST" 2>/dev/null)" || fuser_status=$?
  if [ "$fuser_status" -gt 1 ]; then
    echo 'Cannot inspect the session file with fuser; liveness is unknown.' >&2
    exit 1
  fi
else
  echo 'Cannot verify that Cake stopped: install lsof or fuser, then retry.' >&2
  exit 1
fi

if [ -n "$PIDS" ]; then
  printf 'Cake is still using %s (processes: %s). Wait, then retry.\n' "$LATEST" "$PIDS" >&2
  exit 1
fi
```

If the process check reports an error, or if the target's mtime changes while you wait, do not classify it yet. A live session must not be resumed or diagnosed from a partial snapshot. Once the check shows no writer, snapshot the target securely before running further analysis:

```bash
umask 077
TARGET_SNAPSHOT="$(mktemp "${TMPDIR:-/tmp}/cake-session-target.XXXXXX")"
trap 'rm -f "$TARGET_SNAPSHOT"' EXIT
cp "$LATEST" "$TARGET_SNAPSHOT"
LATEST="$TARGET_SNAPSHOT"
echo "$LATEST"
```

Analyze the snapshot through the rest of this runbook. If `$CAKE_DATA_DIR` is set, sessions, logs, and telemetry use that root.

## Step 2: Check How the Session Ended

After Step 1 confirms that no writer holds the target, a complete invocation ends with `task_complete`. Anything else means the task did not finish cleanly.

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
tail -100 "$LOG_DIR"/cake.$(date +%Y-%m-%d).log | grep -iE "error|warn|truncat"
```

Common patterns:

- `output truncated` --- a tool output exceeded the cap
- API errors --- provider returned an error or timed out
- stream interruption --- connection dropped mid-response
- panics --- cake itself crashed (see the panic message)

## Step 5: Check Telemetry for Retries and Timing

```bash
SESSION_ID="$(awk 'NF { print; exit }' "$LATEST" | jq -r '.session_id')"
TELEMETRY="$TELEMETRY_DIR/$SESSION_ID.ndjson"

if [ -f "$TELEMETRY" ]; then
  # Retry decisions (model retries with backoff)
  jq 'select(.type == "retry_scheduled") | {attempt, reason, delay_ms, detail}' "$TELEMETRY"

  # Per-tool durations
  jq 'select(.type == "tool_call") | {turn_index, name, duration_ms, output_bytes, was_error}' "$TELEMETRY"

  # Final session summary (if present)
  jq 'select(.type == "session_summary")' "$TELEMETRY"
else
  echo "Telemetry sidecar not found: $TELEMETRY" >&2
fi
```

Telemetry is **not** resumable conversation history; it is a separate performance sidecar.

## Step 6: Correlate Session and Log

```bash
SESSION_ID="$(awk 'NF { print; exit }' "$LATEST" | jq -r '.session_id')"
grep "$SESSION_ID" "$LOG_DIR"/cake.*.log
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
$ SESSION_DIR="${CAKE_DATA_DIR:-$HOME/.local/share/cake}/sessions"
$ LATEST="$(ls -t "$SESSION_DIR"/*.jsonl | head -1)"
$ lsof -t "$LATEST"
$ tail -1 "$LATEST" | jq '{type, is_error, subtype, error}'
{
  "type": "reasoning",
  "is_error": null,
  "subtype": null,
  "error": null
}
```

Last record is `reasoning`, not `task_complete`, and the preceding `lsof` check found no writer → task was interrupted mid-stream.

```bash
$ tail -3 "$LATEST" | jq '{type, name: .name, content: (.content // .arguments)[0:120]}'
{ "type": "function_call_output", "name": null, "content": "...build finished in 4.2s\n" }
{ "type": "function_call", "name": "bash", "content": "{\"cmd\":\"cargo test --release --all\"}" }
{ "type": "reasoning", "name": null, "content": "Now I need to verify the integration tests pass before" }
```

Model was reasoning about running tests when the stream ended.

```bash
$ LOG_DIR="${CAKE_DATA_DIR:-$HOME/.cache/cake}"
$ grep -iE "error|timeout|truncat" "$LOG_DIR"/cake.$(date +%Y-%m-%d).log | tail -5
2026-05-21T14:32:18Z ERROR cake::clients::responses: stream error: connection reset by peer
2026-05-21T14:32:18Z WARN  cake::session: task ended without task_complete; session may be incomplete
```

**Diagnosis**: Streaming connection dropped during the model's response.

**Next step for the user**: `cake --continue "Continue where you left off"` will reload the partial session and let the model finish.

## File Locations

  | File                           | Purpose                                  |
  | ------------------------------ | ---------------------------------------- |
  | `$SESSION_DIR/{uuid}.jsonl`    | Session files                            |
  | `$TELEMETRY_DIR/{uuid}.ndjson` | Per-session telemetry (timings, retries) |
  | `$LOG_DIR/cake.YYYY-MM-DD.log` | Daily logs                               |

## When to Switch Procedures

- For full session review, scoring, or recommendations on what cake should change → load the [Analyzing Cake Sessions runbook](analyzing-cake-sessions/index.md).
- For `Operation not permitted (os error 1)` or other sandbox-denied operations → load the [Debugging Sandbox Denials runbook](debugging-sandbox.md).
- For the full JSONL record contract, format version 4 schema, and the LLM-visible vs. audit-only distinction → see [Integration contracts](../integrations.md) and the [Analyzing Cake Sessions runbook](analyzing-cake-sessions/index.md).
