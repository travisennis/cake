---
status: accepted
date: 2026-07-09
---
# Per-Path Serialization of Mutating Tool Calls

## Context and Problem Statement

Tool calls issued in one assistant turn execute concurrently (`join_all` in `src/clients/agent/agent_loop.rs`), so two mutations targeting the same file would race. Instead of fixing the race, the harness compensated with a duplicate-mutation guard (`src/clients/tools/duplicate_guard.rs`) that rejected the second same-file Edit/Write with an error telling the model to re-read and retry, burning an API round-trip. The same restriction was then echoed in three prompt layers: `src/prompts/system.md`, `edit-description.txt`, and `write-description.txt` — four encodings of one implementation shortcut. A model that correctly batches two sequential edits to one file gets punished for coherent behavior.

Task 202 proposed serializing all tool calls in a turn (including Bash) and was cancelled as over-complicated: it constrained model judgment and destroyed useful concurrency to fix a race that only exists between same-path mutations.

## Decision Drivers

- Fix the harness's own concurrency choice mechanically instead of teaching the model to work around it (mechanism over judgment).
- Preserve transcript, session-record, and stream-json ordering and per-call attribution — documented compatibility surfaces.
- Keep concurrency for non-mutating calls and mutations to distinct files.
- Eliminate the rejection round-trip and the prompt-layer echoes.

## Considered Options

- Serialize mutating tool calls per canonical target path; run everything else concurrently.
- Keep the duplicate-mutation rejection guard and prompt echoes (status quo).
- Serialize all tool calls in a turn (cancelled task 202).

## Decision Outcome

Chosen option: per-path serialization of mutating tool calls, because it fixes the race the harness created without constraining model behavior or reducing concurrency anywhere the race cannot occur.

The concrete semantics:

- When scheduling a turn's tool calls, executable calls are grouped by canonical mutating target path (Edit and Write, via the existing `mutating_target` extraction). Groups run concurrently with each other and with all other calls; calls within a group run sequentially in issue order, each observing the previous call's effects.
- Calls whose target path cannot be determined (argument parse or path-validation failures) are not serialized; they execute as scheduled and surface their own errors.
- Hook-blocked calls carry no executable arguments, are never members of a serialization group, and resolve to immediate error results as before. A blocked or failed call does not abort later calls in its group; each subsequent call operates on whatever state prior calls left and succeeds or fails on its own, preserving per-call attribution.
- Tool results are re-emitted in the model's issue order regardless of grouping, so transcript ordering, session records, and stream-json output are unchanged.
- The rejection path and all three prompt-layer echoes are deleted (intentionally reverting the same-file guidance added by task 164). No prompt text describes mutation scheduling: the mechanism makes any batching the model chooses safe.

### Consequences

- Good, because a model that batches sequential same-file edits now succeeds in one turn with no rejection round-trip.
- Good, because one implementation (the scheduler) replaces four encodings (guard plus three prompt layers).
- Good, because ordering and attribution compatibility surfaces are byte-for-byte unchanged.
- Bad, because a genuinely conflicting second mutation now surfaces as an ordinary tool error (for example `old_text` no longer matching after the first edit) instead of a guard message naming the conflict.
- Bad, because Bash commands that write files are still unserialized against Edit/Write calls for the same path; that race predates this decision and remains out of scope, consistent with the rejection of task 202.

## More Information

- Task 236: Replace Duplicate-Mutation Guard with Per-Path Serialization (source: 2026-07-07 backlog triage, bitter-lesson audit V2 item 7).
- Cancelled task 202 documents why whole-turn serialization was rejected.
- Key code: `src/clients/tools/scheduling.rs` (grouping), `src/clients/agent/agent_loop.rs` (`Agent::run_tool_plans`), `src/clients/tools/mod.rs` (`ToolRegistry::mutating_target`).
- Design docs: `docs/design-docs/tools.md` (Tool-Call Scheduling), `docs/guardrails/agent-loop-tools-and-tool-execution.md`.

