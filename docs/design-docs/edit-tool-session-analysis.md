# Edit Tool Session Analysis

This document describes a repeatable strategy for finding, extracting, and assessing `Edit` tool use in persisted cake session files. The goal is to analyze many real sessions and build an evidence-backed list of ways the Edit tool fails or underperforms.

## Scope

Use this method for persisted session JSONL files, usually:

```bash
~/.local/share/cake/sessions/{session_id}.jsonl
```

or:

```bash
$CAKE_DATA_DIR/sessions/{session_id}.jsonl
```

Current sessions use format version 4. The first record should be `session_meta`, each invocation starts with `task_start`, and tool calls are represented by paired `function_call` and `function_call_output` records correlated by `call_id`.

## Finding Edit Calls

Start with session validation and task boundaries:

```bash
SESSION=~/.local/share/cake/sessions/{session_id}.jsonl
head -1 "$SESSION" | jq '.'
jq -r '.type' "$SESSION" | sort | uniq -c
jq 'select(.type == "task_start" or .type == "task_complete")' "$SESSION"
```

Count Edit calls:

```bash
jq -r 'select(.type == "function_call") | .name' "$SESSION" | sort | uniq -c
```

List Edit call sites with line numbers, call ids, target paths, and edit counts:

```bash
jq -r '
  select(.type == "function_call" and (.name | ascii_downcase) == "edit")
  | (.arguments | fromjson) as $args
  | [
      input_line_number,
      .call_id,
      $args.path,
      ($args.edits | length)
    ]
  | @tsv
' "$SESSION"
```

List matching outputs:

```bash
jq -r '
  select(.type == "function_call_output")
  | [
      input_line_number,
      .call_id,
      (.output | gsub("\n"; " ") | .[0:500])
    ]
  | @tsv
' "$SESSION"
```

For deeper inspection, print Edit calls with their full arguments:

```bash
jq '
  select(.type == "function_call" and (.name | ascii_downcase) == "edit")
  | {
      line: input_line_number,
      call_id,
      arguments: (.arguments | fromjson)
    }
' "$SESSION"
```

## Correlating Calls And Outputs

Every Edit assessment should correlate:

- The `function_call` record: `line`, `call_id`, `name`, `arguments`.
- The matching `function_call_output`: `line`, `call_id`, `output`.
- The nearby reasoning and messages before the call.
- Any follow-up reads, searches, build/test failures, re-edits, reverts, or git recovery commands after the call.

Useful window query:

```bash
jq -r '
  select(input_line_number >= START and input_line_number <= END)
  | "LINE \(input_line_number) \(.type) "
    + (
      if .type == "function_call" then
        "\(.name) \(.call_id) \(.arguments)"
      elif .type == "function_call_output" then
        "\(.call_id) " + (.output | tostring | .[0:1200])
      elif .type == "reasoning" then
        ((.content // .summary // "") | tostring | .[0:1200])
      else
        ((.content // .result // "") | tostring | .[0:1200])
      end
    )
' "$SESSION"
```

Replace `START` and `END` with a small range around the Edit call and the subsequent recovery work.

## What To Extract Per Edit Call

For each Edit call, capture:

| Field | Source | Why It Matters |
|---|---|---|
| `session_id` | `session_meta.session_id` | Joins evidence across reports |
| `task_id` | nearest `task_start` / `task_complete` | Groups edits by user request |
| call line | `input_line_number` of `function_call` | Evidence pointer |
| output line | `input_line_number` of matching output | Evidence pointer |
| `call_id` | `function_call.call_id` | Correlates call and output |
| path | `arguments.path` | Identifies target file |
| edit count | `arguments.edits | length` | Detects large or risky edit batches |
| old text length | each `old_text | length` | Proxy for specificity and brittleness |
| new text length | each `new_text | length` | Proxy for rewrite size |
| output status | output prefix / error text | Success or failure |
| follow-up work | later records | Reveals hidden underperformance |

## Success Signals

An Edit call worked well when:

- The output says the expected number of edits were applied.
- The diff in the output is small and matches the stated intent.
- There is no immediate corrective Edit for the same location.
- Later validation succeeds without errors caused by that edit.
- The agent does not need to restore the file, stash, reset, or manually repair a broad unintended change.
- The final diff remains focused and no unrelated files were changed because of the Edit.

## Failure Signals

Classify an Edit call as failed when the output indicates the tool did not apply the requested edit. Common signals:

- Invalid JSON arguments.
- Path validation failure.
- Path not found or not a file.
- Binary or invalid UTF-8 file rejection.
- No edits provided.
- Too many edits in one call.
- `old_text` equals `new_text`.
- `old_text` did not match.
- `old_text` matched more than once.
- Overlapping edits.
- File write failure.

These are direct tool-call failures and should be recorded as `tool_call_error`.

## Underperformance Signals

Classify an Edit call as underperforming when it succeeds technically but causes avoidable downstream work. Common signals:

- **Overbroad replacement**: the edit applies but changes more code than intended.
- **Underspecified match**: short `old_text` or low-context snippets force retry after ambiguous or wrong matching.
- **Large batch risk**: many independent edits in one call make failure diagnosis difficult.
- **Partial migration**: the edit updates definitions but misses fixtures, call sites, snapshots, docs, or imports.
- **Bad locality**: the edit targets a broad file region when a smaller replacement would have reduced risk.
- **Formatting fallout**: the edit leaves indentation or formatting broken and requires `cargo fmt` or manual repair.
- **Semantic fallout**: later compiler, clippy, or test failures trace directly to the edited region.
- **Recovery loop**: the next records show repeated reads/searches/re-edits of the same file because the first edit was not well targeted.
- **Reversion needed**: the agent uses `git checkout`, `git restore`, `git stash`, or equivalent recovery after the edit.
- **Wrong tool choice**: Edit is used for a generated/mechanical transformation that would have been safer as a structured script or purpose-built command.

Underperformance is usually `performance_issue`, `repeated_tool_call`, `tool_result_error`, or `missing_tool_or_capability`, depending on the root cause.

## Assessing The Agent Versus The Tool

Separate tool limitations from agent behavior:

- If the Edit tool returned a precise error and the agent adjusted quickly, the tool likely worked; the agent may have needed better context.
- If the Edit tool accepted a risky broad replacement and only downstream validation revealed damage, the tool may need stronger previews, warnings, dry-run behavior, or structured edit support.
- If the same pattern recurs across sessions, prefer a tool improvement over a prompt-only fix.
- If the agent repeatedly supplies tiny, ambiguous `old_text`, that may indicate the schema or description should push harder for larger unique context.
- If agents avoid Edit and use Bash scripts for safe mechanical changes, that may signal Edit is too cumbersome for batch transformations.

## Evidence Patterns To Look For

Direct failure pattern:

1. `function_call` with `name == "Edit"`.
2. `function_call_output` contains an error.
3. Later reasoning mentions retrying, matching, escaping, line endings, or uniqueness.

Hidden underperformance pattern:

1. `function_call_output` says edits were applied.
2. Later `cargo check --tests`, `cargo test`, `just ci`, or another validation fails.
3. The error points to the edited file or removed/changed symbol.
4. The agent performs corrective edits, restores the file, or reruns broad searches.

Recovery-loop pattern:

1. Edit succeeds.
2. The next 5-20 records contain repeated `Read`, `Bash rg`, `Edit`, and validation calls against the same file.
3. The final diff shows the intended result, but the path there was slow or fragile.

Wrong-abstraction pattern:

1. The agent needs many similar literal edits.
2. Edit calls are large, repetitive, or near the maximum edit count.
3. The agent switches to Bash or Write to script the change.
4. The session shows less friction after using a script.

## Review Checklist

For each session:

1. Validate format and task boundaries.
2. Count Edit calls and total tool calls.
3. Extract every Edit call with path, edit count, and output.
4. Mark direct failures from output text.
5. Inspect the 10-30 records after each Edit for validation failures or corrective work.
6. Link downstream failures to the edited path only when evidence supports that link.
7. Classify each issue with one primary category and one concrete recommendation.
8. Record the smallest useful excerpt, not the full diff or full command output.

For each aggregated pattern:

1. Count affected sessions and Edit calls.
2. Keep at least three representative examples when available.
3. Identify whether the fix belongs in the Edit implementation, tool schema, tool description, agent prompt, or repo instructions.
4. Prefer implementation improvements when the same failure occurs across different agents or tasks.

## Output Format For Multi-Session Analysis

Use a table or JSONL artifact with one row per Edit finding:

| Field | Description |
|---|---|
| `session_id` | Session UUID |
| `task_id` | Task UUID |
| `call_id` | Edit call id |
| `line` | Function call line number |
| `path` | Edited file |
| `status` | `success`, `failed`, or `underperformed` |
| `category` | Issue category |
| `symptom` | Short failure or underperformance label |
| `evidence` | Minimal excerpt |
| `impact` | What it cost or broke |
| `recommendation` | Tool or prompt improvement |

Example:

```json
{"session_id":"...","task_id":"...","call_id":"call_...","line":137,"path":"src/example.rs","status":"underperformed","category":"performance_issue","symptom":"large batch caused recovery loop","evidence":"Applied 8 edits, then validation failed in same file","impact":"Added 12 follow-up tool calls","recommendation":"Add Edit dry-run summaries and warnings for large unrelated edit batches"}
```

## Improvement Ideas To Test Against Real Sessions

Use the aggregated findings to evaluate candidate Edit improvements:

- Dry-run mode that reports match counts and diff previews without writing.
- Built-in unique-match diagnostics showing why `old_text` failed or matched multiple places.
- Better error messages for line-ending, whitespace, and escaping mismatches.
- Optional context-aware edits by line range plus exact text.
- Batch-edit summaries that identify independent hunks and warn about risky breadth.
- Post-edit structured diff metadata, not only a text diff.
- Safer support for mechanical transformations where many similar struct fields or imports must be removed.
- Hints in tool output when the result likely requires formatting.

The purpose of this analysis is not to prove that Edit is bad. It is to identify the cases where a literal search-and-replace tool is too brittle, too opaque, or too slow for real agent workflows.
