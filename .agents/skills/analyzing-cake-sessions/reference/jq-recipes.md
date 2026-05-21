# jq Recipes for Cake Session Files

Reference cookbook for ad-hoc queries against persisted cake session files
(`{uuid}.jsonl`, format version 4). Load this only when running queries
beyond the essentials already in `SKILL.md`.

All examples assume:

```bash
SESSION=~/.local/share/cake/sessions/{uuid}.jsonl
```

## Validation and Overview

```bash
# Pretty-print the session_meta header
head -1 "$SESSION" | jq '.'

# Validate the file is parseable JSONL
jq -c '.' "$SESSION" >/dev/null

# Count records by type
jq -r '.type' "$SESSION" | sort | uniq -c

# First and last record types (was the session cleanly closed?)
head -1 "$SESSION" | jq -r '.type'
tail -1 "$SESSION" | jq -r '.type'
```

## Task Boundaries and Outcomes

```bash
# All task_complete records with key fields
jq 'select(.type == "task_complete") | {
  task_id, subtype, is_error, duration_ms, turn_count, tool_call_count,
  result, error, usage, permission_denials
}' "$SESSION"

# Tasks that errored
jq 'select(.type == "task_complete" and .is_error == true)' "$SESSION"

# Task durations sorted
jq -c 'select(.type == "task_complete") | {task_id, duration_ms}' "$SESSION" \
  | jq -s 'sort_by(.duration_ms) | reverse'

# Total tokens across all tasks
jq 'select(.type == "task_complete") | .usage.total_tokens' "$SESSION" \
  | awk '{s+=$1} END {print s}'
```

## Conversation

```bash
# All user messages with timestamps
jq 'select(.type == "message" and .role == "user") | {timestamp, content}' "$SESSION"

# Assistant messages (truncated for readability)
jq 'select(.type == "message" and .role == "assistant") | {
  status, content: (.content[0:500])
}' "$SESSION"

# Developer/system messages
jq 'select(.type == "message" and (.role == "developer" or .role == "system"))' "$SESSION"

# All reasoning (use .content, NOT .summary — see SKILL.md caveat)
jq 'select(.type == "reasoning") | {timestamp, content: (.content[0:1000])}' "$SESSION"

# Search user prompts for a keyword
jq 'select(.type == "message" and .role == "user")
    | select(.content | test("refactor"; "i"))' "$SESSION"
```

## Tools

```bash
# Tool calls with arguments
jq 'select(.type == "function_call") | {call_id, name, arguments}' "$SESSION"

# Tool outputs (truncated)
jq 'select(.type == "function_call_output") | {call_id, output: (.output[0:1000])}' "$SESSION"

# Calls and outputs interleaved
jq 'select(.type == "function_call" or .type == "function_call_output")' "$SESSION"

# Count tool calls by name
jq -r 'select(.type == "function_call") | .name' "$SESSION" | sort | uniq -c

# Find tool calls without a matching output (orphans)
jq -s '
  (map(select(.type == "function_call") | .call_id) | unique) as $calls
  | (map(select(.type == "function_call_output") | .call_id) | unique) as $outputs
  | { orphan_calls: ($calls - $outputs), orphan_outputs: ($outputs - $calls) }
' "$SESSION"

# Detect repeated identical bash commands (stuck-loop signal)
jq -r 'select(.type == "function_call" and .name == "bash") | .arguments' "$SESSION" \
  | sort | uniq -c | sort -rn | head -10

# Largest tool outputs (bytes)
jq -r 'select(.type == "function_call_output") | "\(.output | length)\t\(.call_id)"' "$SESSION" \
  | sort -rn | head -10
```

## Audit / Metadata Records

```bash
# prompt_context audit records (truncated)
jq 'select(.type == "prompt_context") | {
  task_id, role, timestamp, content: (.content[0:500])
}' "$SESSION"

# Hook events (timing, decision, output)
jq 'select(.type == "hook_event") | {
  hook, decision, duration_ms, exit_code, stdout: (.stdout // "" | .[0:200])
}' "$SESSION"

# Skill activations
jq 'select(.type == "skill_activated") | {name, task_id, timestamp}' "$SESSION"
```

## Tails and Truncation Checks

```bash
# Last 5 records (quick "how did it end" check)
tail -5 "$SESSION" | jq '.'

# Last record type (should usually be task_complete)
tail -1 "$SESSION" | jq '{type, is_error, subtype, error}'

# Find the last task_start that has no matching task_complete (interrupted task)
jq -s '
  reduce .[] as $r ({open: null};
    if $r.type == "task_start" then .open = $r.task_id
    elif $r.type == "task_complete" then .open = null
    else . end)
' "$SESSION"
```

## Correlate with Logs and Telemetry

```bash
# Find all log lines for this session
SESSION_ID="$(head -1 "$SESSION" | jq -r '.session_id')"
grep "$SESSION_ID" ~/.cache/cake/cake.*.log

# Telemetry sidecar location
TELEMETRY="$HOME/.cache/cake/session-telemetry/$SESSION_ID.ndjson"
[ -f "$TELEMETRY" ] && echo "telemetry: $TELEMETRY"

# Retry decisions from telemetry
jq 'select(.type == "retry_scheduled") | {attempt, reason, delay_ms, detail}' "$TELEMETRY"

# Per-tool durations from telemetry
jq 'select(.type == "tool_call") | {turn_index, name, duration_ms, output_bytes, was_error}' "$TELEMETRY"

# Final session summary (if present)
jq 'select(.type == "session_summary")' "$TELEMETRY"
```

## Safety

Do not modify the session file during analysis. All queries above are
read-only; do not pipe results back into the file with `>` or `>>`.
