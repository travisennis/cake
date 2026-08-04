# Context Compaction Design Notes from Pi

This document summarizes how `packages/coding-agent` implements context/session compaction in Pi. It is written as implementation context for another coding agent, not as user-facing documentation.

## Problem

Long coding-agent sessions eventually exceed model context windows. Pi solves this by replacing older active context with a structured summary while preserving the complete original session log.

The important distinction is:

- The session history is not compacted destructively.
- The LLM-facing context is compacted by materializing a smaller view over the append-only session log.

This lets the agent keep working with a small context window while retaining the full original session for tree navigation, auditing, replay, and future summaries.

## Core Approach

Pi stores sessions as append-only entries. Normal messages, model changes, thinking-level changes, labels, branch summaries, custom entries, and compaction checkpoints all live in the same session tree.

Compaction appends a new `compaction` entry. It does not delete or rewrite the earlier entries.

The compaction entry contains:

```ts
interface CompactionEntry<T = unknown> {
  type: "compaction";
  id: string;
  parentId: string | null;
  timestamp: string;
  summary: string;
  firstKeptEntryId: string;
  tokensBefore: number;
  details?: T;
  fromHook?: boolean;
}
```

`summary` is the replacement for old context. `firstKeptEntryId` points to the first original entry that should remain in the active context. `tokensBefore` records the size of the context before compaction. `details` stores implementation-specific metadata, such as files read or modified.

After compaction, Pi rebuilds active context like this:

```text
active LLM context =
  compaction summary message
  + raw original entries from firstKeptEntryId through the compaction checkpoint
  + raw entries after the compaction checkpoint
```

The full original log still contains everything before `firstKeptEntryId`; those entries are simply omitted from the LLM-facing view.

## Source Files in Pi

- `packages/coding-agent/src/core/compaction/compaction.ts`: threshold checks, cut-point selection, summary generation, compaction preparation.
- `packages/coding-agent/src/core/compaction/utils.ts`: conversation serialization and file operation tracking.
- `packages/coding-agent/src/core/session-manager.ts`: session entry types, append-only persistence, tree traversal, `buildSessionContext()`.
- `packages/coding-agent/src/core/agent-session.ts`: manual and automatic compaction orchestration.
- `packages/coding-agent/src/core/messages.ts`: conversion of compaction summaries into LLM messages.
- `packages/coding-agent/docs/compaction.md`: existing user/developer docs.

## Session Model

Each session entry has an `id` and `parentId`. This makes the session a tree rather than only a flat log. The active branch is found by walking from the current leaf back to the root.

Compaction is branch-local because it is just another entry on the current branch. Different branches can have different compaction checkpoints.

The session manager exposes two important concepts:

- `getEntries()`: the full loaded session entries.
- `getBranch()`: entries on the active path from root to current leaf.
- `buildSessionContext()`: the materialized active context sent to the agent/model.

Only `buildSessionContext()` applies compaction. Raw session reads still see the original entries.

## Building Active Context

`buildSessionContext(entries, leafId?, byId?)` does the following:

1. Find the active leaf. If no leaf is specified, use the last entry.
2. Walk `parentId` links from leaf to root to build the active path.
3. Scan the active path for current thinking level, model, and latest compaction entry.
4. If there is no compaction entry, append every context-producing entry as a message.
5. If there is a compaction entry:
   - emit a synthetic compaction summary message first
   - find the compaction entry in the path
   - scan path entries before the compaction entry and start emitting when `entry.id === compaction.firstKeptEntryId`
   - emit context-producing entries after the compaction entry

Entries that produce LLM context include:

- normal `message` entries
- `custom_message` entries
- `branch_summary` entries
- the latest `compaction` entry, represented as a synthetic summary message

Entries that do not directly produce LLM context include:

- `custom` extension-state entries
- labels
- session info
- model/thinking changes, except as metadata
- older compaction entries superseded by the latest compaction on the active path

## LLM Message Representation

Pi represents a compaction summary internally as:

```ts
interface CompactionSummaryMessage {
  role: "compactionSummary";
  summary: string;
  tokensBefore: number;
  timestamp: number;
}
```

When converting agent messages to provider/LLM messages, the compaction summary becomes a user-role message wrapped in compaction summary markers. This makes the summary visible to the model while keeping it distinguishable in the UI and internal state.

For a new agent, the important implementation requirement is to treat the summary as authoritative prior context, not as a normal user request to answer.

## Triggering Compaction

Pi supports manual and automatic compaction.

Manual compaction:

```text
/compact
/compact <custom instructions>
```

Manual compaction aborts any current agent operation before running.

Automatic compaction has two paths:

1. Proactive threshold compaction:

```ts
contextTokens > contextWindow - reserveTokens
```

2. Overflow recovery:

- If the provider returns a context-overflow error, Pi compacts and retries once.
- If retry also fails, it stops and reports that context overflow recovery failed.

Default compaction settings:

```ts
{
  enabled: true,
  reserveTokens: 16384,
  keepRecentTokens: 20000
}
```

`reserveTokens` leaves room for the next model response. `keepRecentTokens` controls how much recent raw context survives compaction.

## Token Accounting

Pi prefers actual provider usage when available. For context size, it uses the latest successful assistant message usage:

```ts
usage.totalTokens || usage.input + usage.output + usage.cacheRead + usage.cacheWrite
```

If there are messages after the latest usage-bearing assistant message, Pi estimates only those trailing messages and adds them to the known usage total.

If no usage exists, Pi estimates all messages with a simple character heuristic. Most text is estimated as `ceil(chars / 4)`. Images are assigned a fixed large estimate.

This hybrid approach avoids recomputing the whole context when provider usage is available, while still handling locally added messages after the last response.

## Preparing a Compaction

Compaction preparation takes the current branch entries and settings, then returns:

```ts
interface CompactionPreparation {
  firstKeptEntryId: string;
  messagesToSummarize: AgentMessage[];
  turnPrefixMessages: AgentMessage[];
  isSplitTurn: boolean;
  tokensBefore: number;
  previousSummary?: string;
  fileOps: FileOperations;
  settings: CompactionSettings;
}
```

The preparation step is separate from summary generation so extensions can inspect or replace the compaction behavior.

Preparation returns `undefined` when the branch cannot or should not compact, for example when the latest entry is already a compaction entry.

## Repeated Compactions

Repeated compaction is iterative.

When preparing a new compaction, Pi finds the latest previous compaction on the branch.

If found:

- `previousSummary` is set to that compaction summary.
- the new summarization range starts at the previous compaction's `firstKeptEntryId`.
- if that id is unavailable, it falls back to the entry after the previous compaction.

This matters because messages kept raw by the previous compaction may later become old enough to summarize. Starting at the old `firstKeptEntryId` lets the next compaction roll those previously kept raw messages into the new summary once the recent window moves forward.

The previous summary is not included as a normal message to summarize. Instead, it is passed separately in `<previous-summary>` tags and the model is asked to update it with the newly summarized messages.

## Cut-Point Selection

Pi chooses a cut point by walking backward from the newest entry and accumulating estimated tokens until it reaches `keepRecentTokens`.

It only cuts at valid cut points:

- user messages
- assistant messages
- bash execution messages
- custom messages
- branch summary messages

It never cuts at tool result messages because tool results must stay associated with their tool calls.

When the initial token-based cut falls near non-message entries, Pi scans backward to include adjacent non-message entries, stopping at message or compaction boundaries.

The result is:

```ts
interface CutPointResult {
  firstKeptEntryIndex: number;
  turnStartIndex: number;
  isSplitTurn: boolean;
}
```

## Turn Boundaries and Split Turns

Pi treats a turn as:

```text
user message or bash execution
+ assistant responses
+ tool calls/results
until the next user message
```

Normally, compaction prefers to cut at turn boundaries.

However, a single turn can exceed `keepRecentTokens`. For example, one user request can trigger a large amount of tool output and assistant work. In that case, cutting only at user messages would keep too much context and fail to reduce the window.

Pi handles this by allowing a cut at an assistant message. If the cut point is not a user message, it finds the user message that started the turn and marks the compaction as a split turn.

For split turns, Pi separates:

- `messagesToSummarize`: complete older turns before the split turn
- `turnPrefixMessages`: the early part of the oversized turn
- kept raw messages: the suffix of the oversized turn from `firstKeptEntryId` onward

The final summary combines:

1. a normal history summary for complete old turns
2. a special turn-prefix summary explaining the early part of the current large turn

The kept suffix remains raw, so the model can continue from the precise recent tool calls/results while still understanding the beginning of the large turn.

## Summary Generation

Before sending messages to the summarizer model, Pi serializes the conversation into text. This prevents the summarizer from treating the old conversation as a conversation to continue.

Serialization uses labels such as:

```text
[User]: ...
[Assistant thinking]: ...
[Assistant]: ...
[Assistant tool calls]: read(path="...")
[Tool result]: ...
```

Tool results are truncated during serialization to a fixed maximum size. The full original tool result remains in the session log, but summarization does not need unbounded output.

The summarizer receives:

- a system prompt saying to only produce a structured summary
- one user message containing:
  - `<conversation>...</conversation>`
  - optional `<previous-summary>...</previous-summary>`
  - summary instructions
  - optional custom user instructions from `/compact <instructions>`

Initial summaries use a prompt that asks for:

```md
## Goal
## Constraints & Preferences
## Progress
### Done
### In Progress
### Blocked
## Key Decisions
## Next Steps
## Critical Context
```

Repeated compactions use an update prompt that tells the model to preserve prior summary content, add new information, update progress, and remove no-longer-relevant items when appropriate.

For split turns, the turn-prefix summary uses a smaller prompt with:

```md
## Original Request
## Early Progress
## Context for Suffix
```

Pi allocates summary output budget from `reserveTokens`:

- normal summary: `floor(0.8 * reserveTokens)`
- turn-prefix summary: `floor(0.5 * reserveTokens)`

## File Operation Tracking

Pi tracks file operations across compactions.

It extracts file paths from assistant tool calls named:

- `read`
- `write`
- `edit`

It stores cumulative file metadata in `CompactionEntry.details` for Pi-generated compactions:

```ts
interface CompactionDetails {
  readFiles: string[];
  modifiedFiles: string[];
}
```

On a new compaction, it starts with file details from the previous Pi-generated compaction, then adds file operations from the messages being summarized and from the split-turn prefix if present.

The summary is appended with machine-readable sections:

```xml
<read-files>
path/to/read-only-file.ts
</read-files>

<modified-files>
path/to/changed-file.ts
</modified-files>
```

Read files exclude files that were later modified. Modified files include writes and edits.

This gives future agents a durable list of files touched in the lost raw context, even if the natural-language summary omits some paths.

## Manual Compaction Flow

High-level manual flow:

```text
AgentSession.compact(customInstructions)
  disconnect from current agent stream
  abort active operation
  create AbortController
  emit compaction_start(reason = manual)
  require selected model
  require auth for selected model
  pathEntries = sessionManager.getBranch()
  settings = settingsManager.getCompactionSettings()
  preparation = prepareCompaction(pathEntries, settings)
  emit session_before_compact extension event if handlers exist
  if extension cancels, throw "Compaction cancelled"
  if extension returns compaction, use it
  otherwise generate default compaction summary
  if abort signal is set, throw "Compaction cancelled"
  sessionManager.appendCompaction(...)
  sessionContext = sessionManager.buildSessionContext()
  agent.state.messages = sessionContext.messages
  emit session_compact extension event
  emit compaction_end(reason = manual)
```

Manual compaction fails when:

- no model is selected
- auth is missing
- the latest branch entry is already a compaction
- there is nothing meaningful to compact
- the extension cancels
- the summarizer fails
- the abort signal fires

## Automatic Compaction Flow

Automatic compaction runs after assistant messages and context checks.

High-level threshold flow:

```text
assistant response arrives
  determine current model context window
  ignore response if it is from a different model than current model
  ignore response if it predates latest compaction checkpoint
  compute context tokens from usage or estimate
  if shouldCompact(contextTokens, contextWindow, settings)
    run auto-compaction(reason = threshold, willRetry = false)
```

High-level overflow flow:

```text
assistant response is a context-overflow error
  if overflow recovery already attempted
    emit failure
    stop
  mark overflow recovery attempted
  remove overflow error from in-memory agent context
  run auto-compaction(reason = overflow, willRetry = true)
  after successful compaction, call agent.continue()
```

Auto-compaction uses the same preparation, extension hook, summary generation, append, and context rebuild logic as manual compaction.

If auto-compaction completes while queued messages exist, Pi calls `agent.continue()` so queued follow-up/custom messages can be delivered.

## Extension Hooks

Pi exposes compaction preparation to extensions through `session_before_compact`.

Extensions receive:

- the prepared compaction data
- branch entries
- custom instructions, for manual compaction
- abort signal

Extensions can:

- return `{ cancel: true }`
- return a complete custom compaction result
- do nothing and allow default compaction

If an extension provides compaction content, Pi still appends the compaction entry itself so ids, parent links, and session state stay consistent.

After a compaction entry is saved, Pi emits `session_compact` with the saved entry and whether it came from an extension.

This is useful for agents that want structured memory, artifact indexes, or project-specific summaries.

## Important Edge Cases and Workarounds

### Preserve the complete original session

The key workaround is append-only compaction. Do not delete messages. Append a checkpoint and make context building aware of the latest checkpoint.

This prevents data loss and allows tree navigation or future tools to recover old raw entries.

### Do not cut at tool results

Tool results depend on preceding tool calls. Cutting at a tool result can leave malformed or confusing context. Pi only cuts at entries that can safely start a kept context segment.

### Support oversized single turns

If one turn exceeds the recent-token budget, compaction must be able to split inside that turn. Pi summarizes the prefix and keeps the suffix raw.

Without this, one enormous tool-heavy turn could prevent compaction from shrinking the context.

### Avoid stale usage after compaction

Kept raw assistant messages from before compaction may contain old `usage.totalTokens` values reflecting the pre-compaction context size. If those stale usage values are used after compaction, they can immediately retrigger compaction.

Pi avoids this by ignoring assistant messages whose timestamps are before or equal to the latest compaction timestamp when checking for post-compaction compaction triggers.

For error messages with no usage, Pi may estimate context from the last successful usage-bearing assistant message, but it also rejects that usage source if it predates the latest compaction.

### Ignore overflow from an old model

If the user switches from a smaller-context model to a larger-context model, an overflow error from the old model should not trigger compaction against the new model. Pi checks that the assistant message provider/model matches the current model before treating it as an overflow trigger.

### Retry overflow only once

Provider context overflow recovery compacts and retries once. If it fails again, Pi stops. This avoids infinite compact/retry loops.

### Do not compact twice in a row

`prepareCompaction()` returns `undefined` if the last branch entry is already a compaction. Manual compaction reports this as already compacted.

### Keep queued work moving

Auto-compaction can happen while queued follow-up messages exist. After compaction, Pi schedules `agent.continue()` so the agent loop resumes with the compacted context.

### Keep cancellation explicit

Manual and automatic compaction use abort controllers. UI code can bind Escape to `abortCompaction()`. The compaction flow checks the abort signal before appending the checkpoint.

## Implementation Checklist for Another Agent

1. Store the full session as append-only entries with stable ids and parent ids.
2. Add a `CompactionEntry` with `summary`, `firstKeptEntryId`, `tokensBefore`, and optional `details`.
3. Make the active context builder understand latest compaction checkpoints.
4. Convert compaction summaries into LLM-visible messages with clear summary markers.
5. Implement context-token accounting using provider usage where possible and estimates where necessary.
6. Add threshold-based auto-compaction with `reserveTokens`.
7. Add overflow recovery compaction and retry once.
8. Implement cut-point selection from newest to oldest using `keepRecentTokens`.
9. Forbid cuts at tool results or other dependent entries.
10. Detect split turns and summarize the prefix separately.
11. Serialize old conversation into a summarization prompt instead of sending it as chat history.
12. Pass previous summary separately during repeated compactions and ask the model to update it.
13. Track file operations or other durable artifacts in structured `details`.
14. Rebuild in-memory agent context immediately after appending compaction.
15. Ignore stale pre-compaction usage when deciding whether to compact again.
16. Add extension or hook points only if the new agent needs custom memory/summary behavior.

## Minimal Data Flow

```text
raw session entries
  -> active branch entries
  -> prepareCompaction()
      -> previous summary
      -> messages to summarize
      -> split-turn prefix, if any
      -> firstKeptEntryId
      -> tokensBefore
      -> file/details metadata
  -> summarize old context with LLM
  -> append CompactionEntry
  -> buildSessionContext()
      -> compaction summary
      -> raw kept entries
      -> raw post-compaction entries
  -> agent.state.messages
  -> next model request
```

## Conceptual Example

Before compaction:

```text
u1 a1 tool1 u2 a2 tool2 u3 a3
```

Assume the cut point chooses `u2` as the first kept entry.

Append:

```text
cmp1 {
  summary: "Summary of u1/a1/tool1 plus relevant state",
  firstKeptEntryId: u2.id,
  tokensBefore: 95000
}
```

The full log becomes:

```text
u1 a1 tool1 u2 a2 tool2 u3 a3 cmp1
```

The active LLM context becomes:

```text
[compaction summary for cmp1]
u2
a2
tool2
u3
a3
```

Later messages append after `cmp1`. The original `u1/a1/tool1` still exist in the log but no longer consume active context.

## Main Design Lesson

Pi separates persistence from context materialization.

Persistence keeps the full fidelity, append-only session tree. Context materialization produces a compact, model-ready projection of that tree using the latest compaction checkpoint. That separation is what allows long-running sessions without losing the original session history.
