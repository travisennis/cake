---
status: proposed
date: 2026-10-12
decision-makers: Travis Ennis
informed: ahm task 034, ahm task 034c
---

# Compact model context with self-contained append-only checkpoints

## Context and Problem Statement

Task 034, inspected with `ahm task show 034`, owns long-session compaction. Cake currently resends its conversation history on every provider request. Its configured `context_window` is an input-token limit, and its latest reported usage measures the request already sent, before subsequent tool results are appended. The existing overflow retry reduces output allowance; it cannot make oversized input fit.

The October 9, 2026 source research compared pi `f1b2e77`, fold `e3a8aeb`, and fx `2456a69`. Pi and fold replace old model context with an incremental summary and a recent tail while preserving the transcript. fx additionally preserves exact user wording and retrievable evidence. These are evidence for this proposal, not dependencies or guarantees of summary fidelity.

Cake has append-only version-4 JSONL, one typed `ConversationItem` representation, a shared locked writer, and two provider adapters. Its agent loop uses raw-history offsets to attribute current-invocation results. A checkpoint must therefore change request projection without shifting those offsets or replaying completed tools.

This ADR is proposed. It records a reviewable design; it does not assert that compaction exists or authorize implementation before the decision is accepted.

## Decision Drivers

- Preserve every original transcript record and the existing result-attribution and tool-execution contracts.
- Resume and fork from one complete checkpoint without following cross-session pointers.
- Handle one tool-heavy task, not only conversations with many user messages.
- Keep provider calls, memory, summary input, summary output, and recovery bounded.
- Preserve the configured provider, model, and reasoning effort; account for failed summary attempts without confusing cost with context size.
- Make the new session and stream records self-versioning and fail-closed, without changing the file format version.
- Keep prompts, permissions, sandbox policy, skills, and live BashSession state outside generated memory's authority.

## Considered Options

- Stop with an actionable exhaustion error. This remains the fallback when compaction cannot safely reduce context, but does not let long tasks continue.
- Drop old messages or shorten retained tool results without a summary. This loses the task's rationale and can separate a call from its result.
- Append a summary and a retained-entry pointer, as in pi and fold. This is compact on disk but requires stable entry identities, pointer validation, and additional fork semantics that Cake does not currently have.
- Append a self-contained summary and typed retained tail, and build a separate request projection. Chosen: it duplicates bounded recent data but fits Cake's existing transcript and typed conversation model.
- Adopt fx's full sourced ledger and record-retrieval system. It offers richer memory, but adds storage and retrieval interfaces beyond the first compaction feature.

## Decision Outcome

Chosen option: append a self-contained checkpoint, preserve raw history, and project the latest checkpoint plus subsequent conversation into provider requests. Manual, threshold, and context-pressure recovery use one compaction module. The module selects, summarizes, and validates a candidate; the agent or manual-command owner commits it.

### Checkpoint and projection

Add a `compaction` session record with a checkpoint version, checkpoint UUID, session/task identity, timestamp, trigger (`manual`, `threshold`, or `overflow`), summary text, retained conversation records, configured model identity, and estimated input sizes before and after. It is one more record kind in the existing version-4 JSONL stream: the session header and its `format_version` do not change, and the record carries its own version so a reader can validate it. Record sizes as estimates, with their basis, rather than presenting them as provider measurements. Store the resolved summary instruction's version for provenance.

The retained records form a restricted typed union of existing message, reasoning, function-call, and function-call-output payloads. They preserve IDs, timestamps, images, and execution-time replay declarations. They cannot contain another checkpoint, prompt-context records, task boundaries, hook audit, or usage records. This avoids recursively nested checkpoints and a second provider conversation schema.

Keep the full raw conversation for invocation offsets, results, exports, and tool attribution. A request projection contains fresh system/developer context, a marked user-role historical summary, the checkpoint's retained records converted to `ConversationItem`, and conversation records appended after that checkpoint. Without a checkpoint it is the current history. A shared projector is used by live requests, resume, and fork. Copied retained records are not counted or emitted again as original tool executions, messages, usage, or activated skills.

Preserve the current invocation's user input exactly, including images. If recent-tail selection would remove it, copy it once into the checkpoint before the selected exchanges; never duplicate it when already retained. Preserve multiple actual user inputs in their original order if an invocation gains steering in the future. Synthetic hook or recovery context must retain its original role and must not be relabeled as a user's instruction. A user input too large to fit under the bounds yields an explicit no-fit failure rather than silent shortening.

Cut only between complete provider exchanges: an assistant response's associated reasoning, tool calls, and all outputs from that batch stay together. Completed calls never run again because of compaction. The most recent exchanges are kept whole within the budget; one oversized exchange can instead be summarized whole, leaving no raw exchange in the tail. On resume, validate and repair abandoned calls using the existing conservative repair policy before preparing compaction. An open tool batch is never a compaction point.

Only the latest checkpoint contributes a summary. The next summary incorporates the previous summary and newly retired context; it does not repeatedly re-read all raw history. Prompt-context records remain audit data, and each invocation rebuilds current instructions as it does today.

The retired boundary is positional, not a stored index: the latest `compaction` record's place in the append-only stream marks the cut. Every conversation record before that point is represented by the checkpoint --- its summary and its retained tail --- and every record after it is sent as-is. A stored raw index would be a pointer into history, which the append-only and fork semantics cannot keep stable, while a record's position always can. Matching retired records by content or call ID is rejected for the same reason: provider call IDs repeat after completed exchanges. Fork re-envelops the latest checkpoint at its original position among the copied records so the same boundary is reconstructed under the new session identity.

### Initial policy and sizing

Add a profile-aware `[compaction]` settings overlay with `enabled = false`, `threshold_percent = 80`, `keep_recent_tokens = 20000`, and `summary_tokens = 4096`. Validate positive token settings and a threshold from 10 through 80. Explicit manual compaction remains available with automatic policy disabled. The feature is opt-in while continuation quality is evaluated.

Automatic checks require a known configured input window `W` and run immediately before every logical provider request, after tool results and other context additions are settled. The trigger is estimated input at or above `floor(W * threshold_percent / 100)`. Retained-tail and summary targets are clamped to `min(keep_recent_tokens, floor(W / 4))` and `min(summary_tokens, floor(W / 16))`; a zero target is an invalid effective budget. The input-window setting retains its current meaning: do not subtract output allowance a second time. The provider's output cap and any provider-specific total-window constraints remain separate.

Prefer complete, positive main-request input usage tied to the exact projection and model that produced it, plus estimates of all subsequently added content. Fall back to estimating the complete request, including prompts, tool schemas, images, and replay content, when usage is missing, zero, partial, or invalidated by compaction, changed prompts/tools, or model changes. Cake's current partial marker does not prove which input counters were present. Cache token details are not added again to normalized input tokens. Estimates are heuristics, not hard provider guarantees; use a conservative margin and the bounded overflow path.

Estimate unmeasured content with one pure function that assigns a fixed characters-per-token ratio by content class: prose, JSON and code text, and base64 or opaque replay bytes each converts more densely than plain prose, and each inline image carries a conservative flat token cost because its provider cost tracks its dimensions rather than its base64 length. Apply a documented safety margin (for example one quarter above the raw estimate) so the estimate errs toward triggering early. Keep the ratios and margin in one place, and treat every value computed this way as an estimate with a recorded basis. The measured `input_tokens` of the last main request is the anchor and is never re-derived by this function; Cake ships no tokenizer and downloads none.

A candidate must be smaller than its source projection and, with a known window, fit below the trigger after including fresh prompts, exact pinned inputs, summary, retained tail, and tool schemas. Shrink the tail only by whole exchanges. If fixed context or pinned input prevents progress, report no fit. Invalidate the old usage basis after activation and use the candidate estimate until a new main request reports usage. Never use a summary request's usage as that basis.

### Summary generation and cost

Use the same resolved provider, credentials, model, and reasoning effort as the conversation. Supply a dedicated summary instruction, no tools, no ordinary task hooks, and no final-output schema. No hidden fallback model or effort change is permitted. Reuse provider transport and bounded retry handling without recursively entering the agent loop.

The instruction asks for the objective, constraints, done/in-progress/blocked work, decisions, verification with actual results, exact paths and available output references, and next steps. It distinguishes user requirements from assistant conclusions and tool evidence. The summary is historical data, not fresh user authorization or developer instructions. Serialize old conversation with explicit roles and provenance; omit opaque reasoning blobs and image base64 from the summary prompt while retaining their original transcript records.

Bound summary work to four logical requests and a 180-second deadline for the entire operation, including retries. A typical compaction uses one request. Summary prompts have a one-MiB byte ceiling and, when the window is known, an estimated input ceiling of 70% of that window. Apply bounded head/tail excerpts to large tool outputs, preserving failure information and existing spill references. If the whole prefix still cannot fit, process oldest complete exchanges in at most four sequential batches, carrying the candidate summary forward privately. Do not publish intermediate summaries. If a request cannot fit, fail rather than drop unrepresented exchanges. An unknown window permits manual compaction with the byte ceiling and the same failure bounds; it does not enable automatic threshold or overflow compaction.

The generation allowance is at most 16384 total output tokens and no greater than a configured model output cap. Preserve reasoning effort and validate any reasoning-token allowance against that cap; a provider that cannot honor the bounded request must fail explicitly. The accepted summary must fit the clamped summary-text target. Reject empty, tool-calling, refused, incomplete, length-stopped, cancelled, or over-budget output, even if it contains plausible partial text. The entire serialized checkpoint, including images and pinned input, has an eight-MiB ceiling.

Reuse `turn_usage` and `api_attempt` accounting with optional `purpose`, `compaction_id`, and `summary_step` attribution; absence of `purpose` means the historical main-agent request. A compaction attempt uses `purpose = compaction`. Every reported summary attempt is settled once before retry or rejection and contributes once to task usage. The checkpoint contains no duplicate billed usage. Summary calls do not consume `max_turns` or `max_tool_calls`; their own request/deadline bounds apply. Unreported usage remains unknown under ADR 030.

### Commit, interruption, and failures

Prepare and validate the complete candidate before changing the active projection. For persisted sessions, append the checkpoint under the existing shared writer lock, flush, and call `sync_data` before activation. This strengthens checkpoint durability without requiring fsync on every ordinary record. A no-session invocation uses the same validation and an in-memory activation without pretending it persisted data.

Before append, cancellation leaves context unchanged. Once append begins, finish its bounded commit section before honoring cancellation. If append, flush, or sync fails, stop the invocation and do not activate or send another request: the file may contain a partial or complete checkpoint, so continued writing against an uncertain state is unsafe. A later resume uses existing partial-tail repair and validates any complete checkpoint. A successful commit followed by output failure remains committed and is restored as such; never append a duplicate checkpoint merely because the output sink failed.

A summary failure before commit leaves context intact. A proactive attempt may continue uncompacted only if estimated input still fits the known window; suppress another proactive attempt for that same source generation. An already rejected or estimated over-window request cannot continue uncompacted. No-fit, exhausted summary bounds, or a second rejected recovery produces an actionable error with the session ID. Prompt/tool-schema growth can itself exhaust the budget and is reported explicitly.

Automatic context-pressure recovery requires enabled policy and a known window. Allow one compact-and-retry per `Agent::send` invocation, shared by input overflow and recoverable context-pressure termination. Task 034b supplies the termination verdict; a generic `length` stop alone never proves input overflow. Classify before persisting assistant items or dispatching tools, while retaining reported attempt usage. Do not execute tool calls from a discarded response.

Keep transport retries bounded by their existing policy. Preserve the current `TurnMode`, output-schema correction count, and semantic-recovery state across compaction. A classified pressure response cannot also receive a semantic continuation. The existing output-allowance retry may handle a request whose input fits and whose total requested output caused rejection; it must hand genuine input overflow to this one compaction guard, rather than start a second recovery loop. Output-schema correction requests retain their schema and disabled-tool mode after compaction.

### Compatibility, fork, and output

The session file format stays version 4. A checkpoint is one additional, self-versioned record kind appended to the existing stream, so no header changes and no format migration occurs. Readers validate the checkpoint record's own version and reject unknown checkpoint versions. Because `SessionRecord` is internally tagged by `type`, a reader that predates this record kind fails loudly on a checkpoint-bearing file instead of misreading it; older binaries are simply not a supported reader for such files, and the file need not advertise that. Compaction appends in place on any format-4 session, subject to the writer lock. It never rewrites earlier bytes and never changes session identity.

Fork creates a new file and preserves copied raw conversation and replay metadata, activation metadata, and the latest self-contained checkpoint when present. It re-envelopes that checkpoint for the new session at its original position among the copied records, so the same positional boundary is reconstructed. Later records retain their order, checkpoint envelopes name the new session, and no checkpoint points into the source file. Reconstruct the same effective context under fresh prompts, with fresh usage estimation rather than copied pre-compaction measurements. Interrupted-call repairs still execute no tools.

Add `compaction_start` and `compaction_end` live stream events with bounded status/trigger/size metadata. Successful live ordering is start, checkpoint commit, checkpoint stream emission, then end. Persist start/end as audit metadata, without sending them to the provider. Replay emits the checkpoint once in physical order, never expanding its copied tail into duplicate message/tool events. Session-only usage records remain absent from stream-json. A failed prepare operation emits failure end metadata and no checkpoint; an interrupted process may leave start without end. Text progress goes to stderr. Normal task JSON keeps its established fields; manual command JSON reports its checkpoint identity and estimates explicitly, without interleaving stream events.

Manual entry point: `cake sessions compact <UUID> [--instructions TEXT] [--json]`. It obtains the session writer lock before loading mutable state, resolves model identity and fresh prompts by the existing session rules, appends a normal task boundary for the operation, runs only compaction, and exits without a continuation or tool execution. It compacts in place and reports an explicit no-op when nothing is safely compactable. JSON uses the existing diagnostic document envelope. Input/identity errors exit 3, operational failures exit 1, success/no-op exits 0, and interruption exits 130.

### Trust and scope

Summary content never changes tool allowlists, sandbox grants, permission decisions, hook configuration, skill activation state, or the BashSession process registry. Summarization does not read archived output files on its own. Existing output paths may be cited, but compaction grants no new archive access and promises no retention beyond existing spill-file lifetime. A full archive retrieval tool and fx-style ledgers require separate tasks.

This decision extends ADR 004's record vocabulary within format version 4 and extends ADR 021's stream vocabulary, ADR 025/030's usage attribution, and ADR 026's preservation requirement to checkpoint tails. It preserves append-only transcript storage, conservative interrupted-call repair, and ordinary tool execution semantics.

### Consequences

- Good, because one completed checkpoint is sufficient to continue a long run, resume it, or fork it without discarding the audit transcript.
- Good, because raw-history offsets and execution records remain stable, while the model receives bounded working context.
- Good, because explicit cost attribution and operation bounds respect the user's configured model and quota.
- Bad, because summaries can omit or distort information; exact current input and recent evidence reduce but do not eliminate that risk.
- Bad, because retained records duplicate bounded transcript data, and checkpoint sync adds latency.
- Bad, because a session that contains a checkpoint is no longer readable by an older Cake binary, and the unchanged file-format version does not advertise that.
- Bad, because fixed prompts, oversized pinned input, opaque content, provider limits, or summary failure can still stop a task; this feature does not promise unlimited context.

## More Information

- Planning parent: ahm task 034; authoring work: task 034c; existing companion tasks: 034a (request-prefix invariant), 034b (termination classification), and 004 (older context-management scope).
- [ExecPlan](../exec-plans/active/context-compaction.md).
- [Append-only sessions](004-append-only-session-task-events.md), [transcript replay](021-session-transcript-replay.md), [usage settlement](025-provider-usage-settlement.md), [replay declarations](026-tool-replay-safety.md), and [usage semantics](030-provider-attempt-usage-semantics.md).
- [Integration contracts](../integrations.md), [configuration](../configuration.md), and [security](../security.md) describe current behavior and are updated only when implementation lands.
- Research provenance: [pi compaction](https://github.com/earendil-works/pi/blob/f1b2e77f5b13b2a199b1052cb79c235451afe7d7/packages/coding-agent/src/core/compaction/compaction.ts), [fold compaction](https://github.com/humanlayer/fold/blob/e3a8aeb3ed62398cd6221c9bd13ba3d3cf88b567/packages/fold-core/src/Compaction/CompactionLayer.ts), and [fx compactor](https://github.com/vercel-labs/fx/blob/2456a6965e5879301d8972301beeaa5852e909b0/src/core/compactor/compactor.zig), inspected October 9, 2026.
