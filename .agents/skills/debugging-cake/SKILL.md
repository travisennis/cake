---
name: debugging-cake
description: |
  How to investigate and debug issues with the cake CLI tool. Use this skill whenever:
  - The user reports the CLI returned "None" or an empty response
  - The user mentions truncated, incomplete, or cut-off responses
  - The user says "Tool error" without explanation occurred
  - The user wants to debug why a task failed or behaved unexpectedly
  - The user asks about session files, logs, or how to investigate CLI behavior
  - The user needs to understand what happened during a previous CLI session
  - Any mention of debugging, investigating, or troubleshooting the cake CLI itself
---

# Debugging Cake CLI

This skill helps investigate and debug issues with the cake CLI tool.

## Quick Reference: Essential Commands

### Find Latest Session

```bash
# List all session files
ls ~/.local/share/cake/sessions/

# Find the latest session file (most recently modified .jsonl)
ls -t ~/.local/share/cake/sessions/*.jsonl 2>/dev/null | head -1
```

### View Session Telemetry

Persisted sessions also have per-session telemetry at `~/.cache/cake/session-telemetry/{uuid}.ndjson` (or `$CAKE_DATA_DIR/session-telemetry/{uuid}.ndjson`). Use it for timings, retries, tool durations, and final usage; it is not resumable conversation history.

```bash
# Show the session performance timeline
jq -c '. | {type, invocation_id, turn_index, attempt, total_ms, delay_ms, name, duration_ms, success}' ~/.cache/cake/session-telemetry/{uuid}.ndjson

# Show retry decisions
jq 'select(.type == "retry_scheduled") | {attempt, reason, delay_ms, detail, changed_request_overrides}' ~/.cache/cake/session-telemetry/{uuid}.ndjson

# Show tool durations
jq 'select(.type == "tool_call") | {turn_index, name, duration_ms, output_bytes, was_error}' ~/.cache/cake/session-telemetry/{uuid}.ndjson
```

### View Session Files

Sessions use JSON Lines (`.jsonl`) format. Use `jq -c` to process each line:

```bash
# View full session (all lines, pretty-printed)
jq '.' ~/.local/share/cake/sessions/{uuid}.jsonl

# View session metadata (first line)
head -1 ~/.local/share/cake/sessions/{uuid}.jsonl | jq '.'

# View last 5 records (most useful)
tail -5 ~/.local/share/cake/sessions/{uuid}.jsonl | jq '.'

# View all user prompts (see what was asked)
jq 'select(.type == "message" and .role == "user") | .content' ~/.local/share/cake/sessions/{uuid}.jsonl

# View all assistant responses (see what was returned)
jq 'select(.role == "assistant") | .content' ~/.local/share/cake/sessions/{uuid}.jsonl

# Check if the task completed (last record should usually be task_complete)
tail -1 ~/.local/share/cake/sessions/{uuid}.jsonl | jq '{type, is_error, subtype, error}'

# View task summaries
jq 'select(.type == "task_complete") | {task_id, subtype, is_error, duration_ms, turn_count, tool_call_count, result, error, usage}' ~/.local/share/cake/sessions/{uuid}.jsonl

# View all reasoning messages
jq 'select(.type == "reasoning")' ~/.local/share/cake/sessions/{uuid}.jsonl

# View all tool calls
jq 'select(.type == "function_call")' ~/.local/share/cake/sessions/{uuid}.jsonl

# View all tool outputs
jq 'select(.type == "function_call_output")' ~/.local/share/cake/sessions/{uuid}.jsonl

# View tool calls AND outputs together (correlate calls with results)
jq 'select(.type == "function_call" or .type == "function_call_output")' ~/.local/share/cake/sessions/{uuid}.jsonl

# Count messages by type (see conversation structure)
jq -r '.type' ~/.local/share/cake/sessions/{uuid}.jsonl | sort | uniq -c

# Find what prompt caused a specific behavior (search by content)
jq 'select(.type == "message" and .role == "user") | select(.content | contains("refactor"))' ~/.local/share/cake/sessions/{uuid}.jsonl
```

### Search Logs

Log files use daily rotation with naming `cake.YYYY-MM-DD.log`. The dated file IS the current
log file for that day - there is no separate "current" file without a date.

```bash
# View today's log entries
tail -100 ~/.cache/cake/cake.$(date +%Y-%m-%d).log

# View logs in real-time
tail -f ~/.cache/cake/cake.$(date +%Y-%m-%d).log

# View recent errors (one-liner)
tail -50 ~/.cache/cake/cake.$(date +%Y-%m-%d).log | grep -i error

# Search for errors in today's log
grep -i "error" ~/.cache/cake/cake.$(date +%Y-%m-%d).log

# Search for warnings
grep -i "warn" ~/.cache/cake/cake.$(date +%Y-%m-%d).log

# Search across all log files
grep -i "error" ~/.cache/cake/cake.*.log

# Find all API requests
grep "https://opencode.ai" ~/.cache/cake/cake.*.log

# Find truncated outputs
grep "output truncated" ~/.cache/cake/cake.*.log

# List all log files
ls -la ~/.cache/cake/cake.*.log
```

## Session Storage Structure

Sessions are stored in `~/.local/share/cake/sessions/` (or `$CAKE_DATA_DIR/sessions/`) as flat `.jsonl` files:

```
~/.local/share/cake/sessions/
  {uuid}.jsonl          # Individual session files (JSON Lines format)
~/.cache/cake/session-telemetry/
  {uuid}.ndjson         # Per-session telemetry timeline
```

The most recent session is determined by `.jsonl` file modification time (no symlink needed). Telemetry sidecars are not considered during session discovery or resume.

### Finding Your Session Directory

```bash
# Find the most recently modified session
ls -lt ~/.local/share/cake/sessions/*.jsonl 2>/dev/null | head -5
```

### Session File Structure

Current persisted sessions use append-only JSON Lines (`.jsonl`) format version 4. Each line is a valid JSON object.

**Line 1: Session Metadata**

```json
{
  "type": "session_meta",
  "format_version": 4,
  "session_id": "uuid-v4",
  "timestamp": "2026-03-28T12:00:00Z",
  "working_directory": "/absolute/path/to/project",
  "model": "model-name",
  "tools": ["bash", "read", "edit", "write"],
  "cake_version": "0.1.0",
  "git": {
    "repository_url": null,
    "branch": null,
    "commit_hash": null
  }
}
```

**Subsequent Lines: Task and Conversation Records**

```json
{"type":"task_start","session_id":"uuid-v4","task_id":"task-uuid","timestamp":"2026-03-28T12:00:01Z"}
{"type":"prompt_context","session_id":"uuid-v4","task_id":"task-uuid","role":"developer","content":"...","timestamp":"2026-03-28T12:00:01Z"}
{
  "type": "message",
  "role": "user",
  "content": "Hello"
}
{"type":"task_complete","subtype":"success","is_error":false,"duration_ms":1000,"turn_count":1,"num_turns":1,"session_id":"uuid-v4","task_id":"task-uuid","result":"Hi","usage":{"input_tokens":0,"input_tokens_details":{"cached_tokens":0},"output_tokens":0,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":0}}
```

Each task starts with `task_start` and should end with `task_complete`.

### LLM-Visible vs. Audit-Only Records

It is important to distinguish records restored into model history (which
consume LLM context window) from records that are purely diagnostic metadata.

**LLM-visible records** (restored into model history via `--continue` / `--resume`):

| Type | Purpose |
|------|---------|
| `message` | User, assistant, system, or developer text |
| `reasoning` | Model reasoning traces (echoed back to the API) |
| `function_call` | Tool invocation request |
| `function_call_output` | Tool execution result |

**Metadata / audit-only records** (NOT restored into model history, do not affect
LLM context):

| Type | Purpose |
|------|---------|
| `session_meta` | Session metadata, first line of every session |
| `task_start` | CLI invocation boundary |
| `task_complete` | Task result, duration, turn count, token usage, permission denials |
| `prompt_context` | Audit record of mutable prompt context (AGENTS.md, skills, env) |
| `hook_event` | Hook execution diagnostic trail (timing, decisions, output) |
| `skill_activated` | Skill usage audit record |

Records like `hook_event`, `prompt_context`, `skill_activated`, and the task
boundary markers occupy session file space but do not contribute to the LLM
context window. When debugging, focus on the four LLM-visible types for
conversation-flow issues; use the audit-only types for operational diagnostics
(hook timing, tool denials, prompt context at invocation time).

## Common Debugging Patterns

### 1. Response Was Truncated (Root Cause of "None" Output)

**Symptom**: CLI returns `None` instead of a meaningful response.

**Check**:

```bash
# A complete invocation usually ends with type: "task_complete"
# If the file ends with task_start, reasoning, function_call, function_call_output,
# or an assistant message without a following task_complete, the invocation may
# have crashed, timed out, or been interrupted.
tail -1 ~/.local/share/cake/sessions/{uuid}.jsonl | jq '{type, is_error, subtype, error}'
```

**Example truncated response** (last line of `.jsonl` file):

```json
{"type":"reasoning","id":"rs_tmp_tf8nkow8vrp","summary":["Now"],"timestamp":"2026-03-28T12:00:05Z"}
```
Note: A trailing conversation record without `task_complete` indicates the task did not finish cleanly.

**How to investigate**:
1. Find the session directory
2. View the last few messages to see where it ended
3. Check logs for API, timeout, stream, or tool errors
4. Check the telemetry sidecar for failed API attempts, retry delays, and final `session_summary`
5. Look at the final task's reasoning and tool records to understand what the model was doing

### 2. Tool Execution Failed

**Check**:

```bash
# Find all function_call_output messages and check for errors
jq 'select(.type == "function_call_output") | {call_id, output: .output[0:200]}' ~/.local/share/cake/sessions/{uuid}.jsonl
```

### 3. "Tool Error" Without Explanation

**Symptom**: CLI returns just "Tool error:" with no context.

**Investigation steps**:
1. Check the log file for that day: `~/.cache/cake/cake.YYYY-MM-DD.log`
2. Look for the specific tool that failed
3. Check if it's a transient issue (network, file permissions, etc.)

### 4. Session Grew Too Large

**Check**:

```bash
# Check session file size
ls -lh ~/.local/share/cake/sessions/{uuid}.jsonl

# Count total lines (messages + header)
wc -l ~/.local/share/cake/sessions/{uuid}.jsonl

# Count total characters in all content fields
jq -r '.content // ""' ~/.local/share/cake/sessions/{uuid}.jsonl | wc -c
```

### 5. Model Made Unexpected Tool Calls

**Check**:

```bash
# List all tool calls made
jq 'select(.type == "function_call") | {name, arguments}' ~/.local/share/cake/sessions/{uuid}.jsonl
```

## Correlating Sessions with Logs

```bash
# 1. Get the session ID from the header line
SESSION_ID=$(head -1 ~/.local/share/cake/sessions/{uuid}.jsonl | jq -r '.session_id')
echo "Session ID: $SESSION_ID"

# 2. Find log entries around session creation time
TIMESTAMP=$(head -1 ~/.local/share/cake/sessions/{uuid}.jsonl | jq -r '.timestamp')
echo "Session start: $TIMESTAMP"

# 3. Search logs for that session's activity
grep "$SESSION_ID" ~/.cache/cake/cake.*.log
```

## Quick Reference Commands

```bash
# Find latest session file
LATEST=$(ls -t ~/.local/share/cake/sessions/*.jsonl 2>/dev/null | head -1)

# View last 5 messages (most common debugging command)
tail -5 "$LATEST" | jq '.'

# Check if response was complete (last line)
tail -1 "$LATEST" | jq '{type, is_error, subtype, error}'

# View recent errors in logs (one-liner)
tail -50 ~/.cache/cake/cake.$(date +%Y-%m-%d).log | grep -i error

# View full session file
less "$LATEST"
```

## Debugging Checklist

When the user reports an issue:

1. **Find the session**
   - List session files in `~/.local/share/cake/sessions/`
   - Find the most recently modified `.jsonl` file

2. **Check for truncation**
   - `tail -1 session.jsonl | jq '{type, is_error, subtype, error}'` - should usually end with `task_complete`
   - If it ends with a conversation record or `task_start`, the task likely ended abruptly

3. **Review the conversation flow**
   - `tail -5 session.jsonl | jq '.'` - see the last few interactions
   - Look for where things went wrong

4. **Check logs**
   - `tail -100 ~/.cache/cake/cake.$(date +%Y-%m-%d).log | grep -i error`
   - Look for tool failures or API errors

5. **Identify patterns**
   - Were there multiple rapid tool calls?
   - Did the model get stuck in a loop?
   - Was there a specific error message?

## Key Insight: Why "None" Happens

The common failure pattern behind `None` or empty output is **no completed assistant result for the task**. When the model response is cut off, the process times out, or streaming is interrupted, the session may end without a successful `task_complete` record or without final assistant text.

This typically happens when:
- The model hits token limits
- The response times out
- The streaming connection is interrupted

**Fix approach**: The CLI should detect incomplete responses and either:
- Automatically retry/continue
- Warn the user that the task may be incomplete
- Return a meaningful message instead of `None`

## Debugging Sandbox Violations

When commands fail with `Operation not permitted (os error 1)` inside the sandbox, use `sandbox-exec`'s trace mode to identify exactly which operations are being denied.

### Quick Diagnosis

```bash
# Check if sandbox is active
echo $CAKE_SANDBOX  # Sandboxing is enabled unless this is off, 0, false, or no

# Test a command in the sandbox with the same profile
sandbox-exec -f "$TMPDIR"/cake/sandbox_profiles/cake_sandbox_*.sb bash -c "your-command-here"
```

### Using Trace Mode

Create a debug profile that logs denials instead of blocking them. Add a `(trace)` directive to the profile:

```bash
# 1. Find the generated profile
ls -la "$TMPDIR"/cake/sandbox_profiles/

# 2. Copy it and add trace mode
cp "$TMPDIR"/cake/sandbox_profiles/cake_sandbox_XXXX.sb /tmp/debug_sandbox.sb

# 3. Edit to add trace output — replace "(deny default)" with:
#    (deny default (with send-signal SIGKILL))
#    (trace "/tmp/sandbox_trace.log")
# Or for just logging without blocking:
#    (deny default (with no-log))
#    (trace "/tmp/sandbox_trace.log")

# 4. Run the failing command with the debug profile
sandbox-exec -f /tmp/debug_sandbox.sb bash -c "cargo check"

# 5. View the trace to see what operations were denied
cat /tmp/sandbox_trace.log
```

### Common Missing Permissions

  | Error Pattern                                 | Likely Cause                               | Fix                                |
  | --------------------------------------------- | ------------------------------------------ | ---------------------------------- |
  | `Operation not permitted` on `target/` writes | Missing `file-lock`                        | Add `(allow file-lock)` to profile |
  | `/tmp` access denied despite being allowed    | Symlink mismatch (`/tmp` → `/private/tmp`) | Ensure both forms in profile       |
  | Cargo registry download fails                 | `~/.cargo/registry` is read-only           | Add to `read_write` paths          |
  | `flock` / `fcntl` failures                    | Missing `file-lock` permission             | Add `(allow file-lock)` to profile |

### Inspecting the Generated Profile

```bash
# View the actual profile being used (check cake logs)
grep "Generated sandbox profile" ~/.cache/cake/cake.*.log

# Or find the latest profile file
ls -lt "$TMPDIR"/cake/sandbox_profiles/ | head -5
cat "$TMPDIR"/cake/sandbox_profiles/cake_sandbox_*.sb
```

## File Locations Summary

  | File Type               | Location                                    |
  | ----------------------- | ------------------------------------------- |
 | Sessions                | `~/.local/share/cake/sessions/{uuid}.jsonl` |
  | Session telemetry       | `~/.cache/cake/session-telemetry/{uuid}.ndjson` |
 | Logs                    | `~/.cache/cake/cake.YYYY-MM-DD.log`         |
  | Global config           | `~/.config/cake/settings.toml`              |
  | Project config          | `.cake/settings.toml`                       |
  | User-level AGENTS.md    | `~/.cake/AGENTS.md`                         |
  | Project-level AGENTS.md | `./AGENTS.md`                               |

## Configuration

- **Cache directory**: `~/.cache/cake/` (logs and ephemeral data)
- **Sessions directory**: `~/.local/share/cake/sessions/` (session files)
- **Session telemetry directory**: `~/.cache/cake/session-telemetry/` (structured per-session performance sidecars)
- **Data directory override**: Set `CAKE_DATA_DIR` to use a custom path for both cache and sessions
- **Logs**: `~/.cache/cake/cake.YYYY-MM-DD.log` (or `$CAKE_DATA_DIR/cake.YYYY-MM-DD.log` if set, daily rotation)
- **API key**: Required via environment variable (set via .cake/settings.toml or ~/.config/cake/settings.toml)

## Session Restoration and Continuation

To continue a previous session:

```bash
./target/release/cake --continue "What was my last message?"
```

The `--continue` flag loads the latest session from the current directory.

To resume a specific session, pass the UUID, not a file path:

```bash
./target/release/cake --resume {uuid} "Continue"
```
