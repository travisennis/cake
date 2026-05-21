---
name: debugging-cake
description: |
  Triage a recent cake CLI failure. Use only for reactive, user-reported failures:
  - The CLI returned `None`, empty output, or a clearly truncated response
  - The CLI reported "Tool error:" with no further detail
  - A task crashed, hung, or was interrupted mid-stream
  - The user reports their last cake run "broke" and wants to know why
  For deeper session review, quality assessment, or scoring how cake performed,
  use `analyzing-cake-sessions` instead. For sandbox `Operation not permitted`
  errors, use `debugging-sandbox`.
---

# Debugging a Failed Cake Run

This skill is for **fast, reactive triage** of the user's most recent failed
cake run. The goal is to identify what broke, not to produce a full session
analysis report.

For deeper structural analysis of a session (issue categories, quality
assessment, improvement recommendations), use `analyzing-cake-sessions`.

## Step 1: Find the Failing Session

```bash
# Latest session file (most recently modified .jsonl)
LATEST=$(ls -t ~/.local/share/cake/sessions/*.jsonl 2>/dev/null | head -1)
echo "$LATEST"
```

If `$CAKE_DATA_DIR` is set, sessions live under `$CAKE_DATA_DIR/sessions/`.

## Step 2: Check How the Session Ended

A complete invocation ends with `task_complete`. Anything else means the
task did not finish cleanly.

```bash
tail -1 "$LATEST" | jq '{type, is_error, subtype, error}'
```

Interpretation:

| Last record type             | Meaning                                                       |
| ---------------------------- | ------------------------------------------------------------- |
| `task_complete` (no error)   | Task finished normally — issue is in the result, not the run |
| `task_complete` (`is_error`) | Task ended with a recorded error — read `.error`              |
| `reasoning` / `function_call`/ `function_call_output` / `message` | Task was interrupted mid-stream (timeout, crash, signal)      |
| `task_start`                 | Task never produced any output                                |

## Step 3: Look at the Last Few Records

```bash
tail -5 "$LATEST" | jq '.'
```

This usually reveals: the last tool the model invoked, the last output it
saw, or where the reasoning trailed off.

## Step 4: Check Today's Log

```bash
tail -100 ~/.cache/cake/cake.$(date +%Y-%m-%d).log | grep -iE "error|warn|truncat"
```

Common patterns:

- `output truncated` — a tool output exceeded the cap
- API errors — provider returned an error or timed out
- stream interruption — connection dropped mid-response
- panics — cake itself crashed (see the panic message)

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

Telemetry is **not** resumable conversation history; it is a separate
performance sidecar.

## Step 6: Correlate Session and Log

```bash
SESSION_ID="$(head -1 "$LATEST" | jq -r '.session_id')"
grep "$SESSION_ID" ~/.cache/cake/cake.*.log
```

## Why "None" Happens

`None` or empty output almost always means **no completed assistant result
was produced** before the session ended. Typical causes:

- Model hit token limits mid-response
- Response or streaming connection timed out
- Process was interrupted (signal, panic, crash)
- A tool call hung and never returned

When this happens, the session file ends without a `task_complete` record
(or `task_complete` is present with `is_error: true`).

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

**Next step for the user**: `cake --continue "Continue where you left off"`
will reload the partial session and let the model finish.

## File Locations

| File                                            | Purpose                                              |
| ----------------------------------------------- | ---------------------------------------------------- |
| `~/.local/share/cake/sessions/{uuid}.jsonl`     | Session files (or `$CAKE_DATA_DIR/sessions/`)        |
| `~/.cache/cake/session-telemetry/{uuid}.ndjson` | Per-session telemetry (timings, retries)             |
| `~/.cache/cake/cake.YYYY-MM-DD.log`             | Daily logs (or `$CAKE_DATA_DIR/cake.YYYY-MM-DD.log`) |

## When to Switch Skills

- For full session review, scoring, or recommendations on what cake should
  change → load `analyzing-cake-sessions`.
- For `Operation not permitted (os error 1)` or other sandbox-denied
  operations → load `debugging-sandbox`.
- For details on JSONL record types, format version 4 schema, and the
  LLM-visible vs. audit-only distinction → see `analyzing-cake-sessions`.
