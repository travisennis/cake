# Harness-Anchored Editing

Status: active
Created: 2026-05-20
Updated: 2026-05-20
Related tasks: 117, 139, 163, 164
Related plans: -
Confidence: medium

## Summary

Recent external discussion around edit tools points to a useful direction for cake: move edit anchoring out of model-reproduced `old_text` and into harness-managed file views. Instead of asking the model to repeat exact old content, the harness can remember what the model saw during `Read` or search, let the model refer to line ranges from that old view, and accept the edit only when the current file still matches the remembered view at those ranges.

This is still compare-and-set editing, but the comparison data comes from cake's own read snapshot, not from text the model must regenerate. It targets three common failures:

- Wasted tokens from repeating long `old_text`.
- Exact-match failures from whitespace, escaping, or partial recall mistakes.
- Corruption risk when line numbers are used without validating that the file still has the expected content.

The most promising design for cake is a new view-aware edit mode layered alongside the current literal `Edit`: `Read` records a content snapshot for the returned line range, the model calls an edit tool with `path`, `view_id`, `start_line`, `end_line`, and `new_text`, and cake rejects the edit if the current file no longer matches the remembered lines for that view.

## Sources

- Salvatore Sanfilippo, "Alternatives for the EDIT tool of LLM agents", 2026-05-19: https://antirez.com/news/166
- Follow-up idea from the same discussion: let the harness remember the old file view and ask the model for lines/ranges, failing if the current range no longer matches the remembered content.
- Can Boluk, "I Improved 15 LLMs at Coding in One Afternoon. Only the Harness Changed.", 2026-02-12: https://blog.can.ac/2026/02/12/the-harness-problem/

## External Approach

### Current str_replace / literal old_text

Cake currently follows the common `str_replace` shape: each edit gives exact `old_text` and `new_text`. This is simple and strong when it works. It validates the file content at edit time and refuses missing, duplicated, or overlapping matches. Cake has already improved this with atomic multi-edit preflight, nearest-match hints, CRLF normalization, no-op reporting, and overlap rejection.

The weakness is that the model must reproduce the old text exactly. That means the edit protocol spends output tokens on content the model already saw, and it often fails for reasons unrelated to the intended change: whitespace, escaping, indentation, special characters, or stale context.

### Hashline / tagged lines

The `hashline` proposal changes read output so each line includes a compact content tag, for example:

```text
10:Q8fA int count = 10;
11:rA3_ if (count > limit) {
12:Kq9z     count = limit;
13:PX0b }
```

The model then edits by referring to `line + tag`, or a range of such anchors, plus `new_text`. The edit is rejected if the tag no longer matches current file content. This gives the model a compact stable handle for a specific old line without forcing it to quote the line.

Benefits:

- Reduces output tokens for edits, especially deletions and large replacements.
- Makes stale edits fail before corruption.
- Gives the model line numbers it can reason about naturally.
- Avoids exact old-text reproduction.

Costs and risks:

- Read/search output grows by line tags.
- Tags can collide unless they are long enough or combined with path/range/version context.
- The model must copy tags correctly.
- Tag design becomes an interface contract.

### Whole-file CRC / version CAS

The antirez post also considers a simpler file-level tag: every view has a whole-file checksum, and edits specify line ranges plus that checksum. This is compact and supports ranges naturally, but it rejects edits after any unrelated file change. That is often too conservative in collaborative or parallel-agent workflows.

This is equivalent to optimistic file-version editing. It is simple and safe, but it gives up useful concurrency.

### Harness-remembers-old-view

The follow-up idea is cleaner for cake than exposing line tags directly. When the model reads a file, the harness already has the exact bytes or lines it returned. It can persist a view object:

- canonical path
- file identity metadata, if useful
- read range and total lines at read time
- old line content for the returned range
- optional whole-file or range digest
- view id visible to the model

The model later edits by referencing the view and line range:

```json
{
  "path": "/repo/src/example.rs",
  "view_id": "v42",
  "start_line": 120,
  "end_line": 126,
  "new_text": "replacement text"
}
```

The harness maps that request back to the remembered content from `v42`, checks that the current file still has the same content for those lines, and applies the replacement only if the check passes.

This preserves the key CAS property without making the model repeat old content or copy per-line tags. It also avoids rejecting unrelated changes outside the edited range, unlike whole-file CRC.

## Fit For Cake

Cake's current tool surface:

- `Read` returns line-numbered content with optional line ranges.
- `Edit` uses literal `old_text` / `new_text`, applies up to 10 edits atomically in one call, and writes via reverse-order replacement after preflight.
- `Write` overwrites whole files and is discouraged for targeted edits.
- The agent currently executes tool calls from a model turn concurrently, which motivated task 163 for same-file mutating tool protection.

Harness-anchored editing fits naturally because cake already owns both sides of the transaction:

1. `Read` can return a `view_id` and remember the exact returned line slice in agent state.
2. A new edit path can replace by remembered line range instead of literal `old_text`.
3. Existing edit validation and reverse-order application concepts can be reused after the range has been resolved into byte offsets.
4. Task 163's same-file mutation guard should still exist, because view validation prevents stale edits but not necessarily concurrent writes racing in one turn.

## Proposed Cake Design

### Add read views to agent state

Introduce an in-memory per-session view store owned by the agent or tool context. Each successful file `Read` creates one view.

Suggested fields:

```rust
struct FileView {
    id: String,
    canonical_path: PathBuf,
    start_line: usize,
    end_line: usize,
    total_lines: usize,
    lines: Vec<String>,
    line_ending: LineEnding,
    range_digest: String,
    file_digest_at_read: Option<String>,
    created_turn: u32,
}
```

The persisted session does not need to restore all views initially. For a first implementation, view state can be ephemeral within one CLI invocation. A later implementation could persist compact view records in JSONL if resume/continue should support view-based edits.

### Change Read output

Keep existing line-numbered output, but add a compact header:

```text
File: /repo/src/example.rs
View: rv_01HZ... lines 120-160/420
Lines 120-160/420
   120: ...
```

For directory reads, no view is needed.

### Add a range-based edit tool or mode

Prefer a separate tool at first, such as `EditRange`, rather than overloading literal `Edit`. This lets cake benchmark and compare behavior without destabilizing the current tool.

Possible schema:

```json
{
  "path": "string",
  "view_id": "string",
  "edits": [
    {
      "start_line": 120,
      "end_line": 126,
      "new_text": "string"
    }
  ]
}
```

Semantics:

- `start_line` and `end_line` are 1-indexed and inclusive, matching `Read`.
- Empty `new_text` deletes the range.
- For insertion, support either `insert_before_line` / `insert_after_line`, or allow zero-width ranges only with explicit separate fields. Avoid ambiguous `start_line > end_line`.
- All edits in one call are atomic.
- Edits must be non-overlapping.
- Every edited line must be within the remembered view range.
- Current file content for the requested old range must exactly equal the remembered view lines.
- Reject if the current file has changed in a way that shifts or changes the target range. The model should re-read and retry.

### Validation algorithm

For each edit:

1. Look up `view_id`.
2. Verify canonical `path` matches the view path.
3. Verify requested range is within the view's remembered range.
4. Read current file bytes and decode as UTF-8.
5. Split current file into lines while preserving line ending behavior.
6. Compare current lines at `start_line..=end_line` with the stored view lines for the same range.
7. If all edits validate and do not overlap, compute byte ranges and apply replacements in reverse order.
8. Return a bounded unified diff.

This is essentially literal old-text edit where cake synthesizes `old_text` from a trusted view snapshot instead of asking the model to provide it.

### Handling searches

The same mechanism can eventually apply to Bash search results, but that is harder because cake does not own arbitrary `rg` output semantically. A phased approach:

1. Support view-based edits only from `Read`.
2. Encourage the model to `Read` the target range before `EditRange`.
3. Later, add a first-class `Search`/`Grep` tool that returns path, line, text, and view anchors backed by the same view store.

## Tradeoffs

### Advantages over literal Edit

- Less model output for replacements and especially deletions.
- Fewer exact-match failures from whitespace or escaping.
- Better stale-file safety than bare line-number edits.
- Can give clearer errors: "line 124 no longer matches the view; re-read lines 120-130."
- Creates a natural foundation for benchmarking edit protocol success rates across models.

### Advantages over hashline

- The model does not need to copy hashes.
- Read output stays mostly human-readable and only adds one view id.
- Collision risk is internal only; cake can use full cryptographic/range digests without exposing them.
- The interface can evolve without changing every displayed line.

### Disadvantages versus hashline

- Requires a harness-side view store.
- View ids add statefulness to tool calls.
- A model may refer to an old view after compaction or resume unless the error path is clear.
- Edits outside the read range require another `Read`.

### Staleness behavior

Range-level validation is less conservative than whole-file checksum validation: unrelated changes elsewhere in the file do not matter if line numbers remain stable and the target lines match. But if lines are inserted above the target, the same content may move to different line numbers and the edit will fail. That failure is acceptable because the model can re-read.

More permissive variants could search the current file for the remembered range content if line numbers moved. That should not be the first version because it reintroduces ambiguity and duplicate-match questions that literal `Edit` already handles.

## Implementation Path

### Phase 1: Research prototype behind a feature flag

- Add a `FileViewStore` to the agent/tool execution context.
- Have `Read` store views and print a `View:` header.
- Add `EditRange` as a new tool available only behind a setting or environment flag.
- Keep literal `Edit` unchanged.
- Add unit tests for range validation and stale-view rejection.

### Phase 2: Prompt and tool guidance

- Teach models to `Read` before `EditRange`.
- Tell models not to use stale view ids after an edit to the same file; re-read first.
- Connect this with task 164's parallel-write guidance.

### Phase 3: Benchmarks

Build a small local benchmark similar in spirit to the Can Boluk post:

- Generate simple mutation/reversion tasks from existing repository files.
- Run the same model with literal `Edit` and `EditRange`.
- Measure task success, edit failure count, retries, output tokens, and wall time.
- Include weaker/smaller models because harness improvements may help them most.

### Phase 4: Decide default behavior

If `EditRange` lowers retries and token use without adding confusing failures, make it available by default. Keep literal `Edit` as a fallback for edits where the model has exact target text but no current view.

## Relation To Existing Tasks

- Task 117: stronger edit semantics and tests should cover both literal and range-based edit cores.
- Task 139: structured bulk edit support may reuse view ranges for safe mechanical transformations.
- Task 163: same-file mutating calls still need agent-loop protection; view CAS is not a substitute for path-level scheduling.
- Task 164: prompt guidance should mention that range/view edits are tied to the most recent read view and should not be mixed with same-file parallel writes.

## Open Questions

- Should view ids be persisted into the JSONL session, or should they be per-invocation only?
- Should view stores survive compaction, and if so, should the model get a reminder of active views?
- What is the right insertion schema: `insert_before_line`, `insert_after_line`, or zero-width ranges?
- Should `Read` always emit view ids, or only when a range is small enough to make editing plausible?
- Should view ids be path-scoped and short, or globally unique and opaque?
- Can cake expose a first-class search tool so search results also become editable views?
- How should view-based edits interact with CRLF preservation, BOM handling, and final newline behavior?

## Follow-ups

- Create a task for a feature-flagged `EditRange` prototype if this direction is accepted.
- Consider updating task 117 to include view-based edit invariants.
- Consider updating task 139 to evaluate whether bulk transforms should be view-backed.
- After prototype data exists, write an ADR if cake changes the default edit protocol.
