# Opencode Compaction Approach

Status: synthesized
Created: 2026-06-06
Updated: 2026-06-06
Related tasks: -
Related plans: -
Confidence: high

## Summary

Opencode uses a **multi-layered compaction system** with two complementary mechanisms: (1) **tool-output pruning**, which cheaply erases old tool result bodies to reclaim context, and (2) **model-in-the-loop summarization**, which generates a structured anchored summary of older turns while preserving the most recent turns verbatim. Compaction is a first-class orchestrated service with plugin hooks, auto-continue replay, and configurable thresholds. The system has two code layers: a low-level core package (`@opencode-ai/core`) owning the prompt template and basic selection, and a higher-level session package owning orchestration, persistence, replay, and event emission.

## Notes / Evidence

### Source Files

**Session layer** ([opencode](https://github.com/anomalyco/opencode)):
- [`packages/opencode/src/session/compaction.ts`](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/session/compaction.ts) --- Full compaction service (overflow detection, turn selection, prune, process, create)
- [`packages/opencode/src/session/overflow.ts`](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/session/overflow.ts) --- Token budget and overflow calculations
- [`packages/opencode/src/session/processor.ts`](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/session/processor.ts) --- Agent loop processor that triggers compaction
- [`packages/opencode/src/session/message-v2.ts`](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/session/message-v2.ts) --- Message serialization, media stripping, tool output truncation

**Core layer** ([opencode-core](https://github.com/anomalyco/opencode)):
- [`packages/core/src/session/compaction.ts`](https://github.com/anomalyco/opencode/blob/dev/packages/core/src/session/compaction.ts) --- Lower-level `buildPrompt`, `select`, `compactIfNeeded`, `compactAfterOverflow`
- [`packages/core/src/v1/config/config.ts`](https://github.com/anomalyco/opencode/blob/dev/packages/core/src/v1/config/config.ts) --- Compaction config schema definition

--------------------------------------------------------------------------------

### Mechanism 1: Tool-Output Pruning

Before resorting to full summarization, opencode can **prune old tool outputs** to cheaply free context space.

**Trigger:** `compaction.prune = true` (opt-in, default `false`).

**Algorithm:**

1. Walk messages **backwards** from newest to oldest.
2. Skip the first 2 user turns (always preserve recent tool results).
3. Stop if an assistant message with `summary: true` is encountered (already-compacted region).
4. For each tool result, accumulate its token estimate. Skip "protected" tools (`skill`), errored calls, and already-compacted parts.
5. Once accumulated tokens exceed `PRUNE_PROTECT` (40,000 chars), start marking tool parts for pruning.
6. Only actually prune if total pruned exceeds `PRUNE_MINIMUM` (20,000 chars).
7. Mark pruned parts with `part.state.time.compacted = Date.now()`.

**Effect on serialization:** When a tool part has `time.compacted`, `message-v2.ts` renders its output as `"[Old tool result content cleared]"` and drops its media attachments.

--------------------------------------------------------------------------------

### Mechanism 2: Model-in-the-Loop Summarization (Full Compaction)

#### Trigger Conditions

Overflow detection (`overflow.ts`):

```
tokens >= model.contextLimit - reserved
```

Where `reserved` defaults to `min(20_000, model.maxOutputTokens)`. When `model.limit.input` is set separately (some providers), that's used instead of context minus output tokens.

Can be disabled globally via `compaction.auto = false`.

#### Trigger Points

Compaction is initiated by the `Processor` during the agent loop when `isOverflow` returns true. The processor sets `ctx.needsCompaction = true`, which causes the loop to call `compaction.create()` (inserts a compaction-trigger user message) and then `compaction.process()`.

Also user-triggerable (the `create` function accepts an `auto` flag to distinguish automatic vs. manual).

#### Selection Algorithm (`select`)

Determines what gets summarized vs. what stays verbatim:

1. Compute **preserve_recent_tokens** budget: config value, or `min(8000, max(2000, 25% of usable))`.
2. Walk **turns** (from user message to next user message). Skip compaction messages.
3. Take the last `N` turns (default: `compaction.tail_turns = 2`).
4. Going backwards, accumulate sizes. If the budget is exceeded:
   - Try to **split** the overflow turn mid-stream (walk forward through the turn until the remaining messages fit the budget).
   - If splitting fails, fall back to whatever tail was accumulated so far.
5. Return `{ head: messages_to_summarize, tail_start_id: id_of_first_preserved_message }`.

#### Previously Compacted Turns

`completedCompactions()` identifies prior compactions by looking for user messages with a `"compaction"`-type part, paired with a following assistant message that has `summary: true` and no error. These prior compaction spans are **hidden** from the selection --- they don't get re-serialized into the summary input.

#### Summary Prompt (`buildPrompt`)

Located in `packages/core/src/session/compaction.ts`:

```
## Goal
- [single-sentence task summary]

## Constraints & Preferences
- [user constraints, preferences, specs, or "(none)"]

## Progress
### Done
- [completed work or "(none)"]

### In Progress
- [current work or "(none)"]

### Blocked
- [blockers or "(none)"]

## Key Decisions
- [decision and why, or "(none)"]

## Next Steps
- [ordered next actions or "(none)"]

## Critical Context
- [important technical facts, errors, open questions, or "(none)"]

## Relevant Files
- [file or path: why it matters, or "(none)"]
```

If a previous summary exists, the prompt asks to **update** it ("Preserve still-true details, remove stale details, and merge in new facts"). Otherwise, it asks to create a new one. The previous summary and recent tail are passed as additional context.

The template is wrapped with `buildPrompt({ previousSummary?, context })` which assembles the final prompt.

#### Compilation Flow (`process`)

1. **Find parent**: Must be a user message with a `"compaction"`-type part.
2. **Overflow replay detection**: If `overflow=true`, find the last pre-compaction user message and set it up for replay after compaction completes. This lets the LLM seamlessly continue its work.
3. **Load agent**: Uses a special `"compaction"` agent (or falls back to the original model).
4. **Build summary input**:
   - Filter out previously-compacted turns via `hidden` set.
   - Run `select()` to get `head` (summarize) + `tail` (preserve verbatim).
   - Plugins can inject context or override the prompt via `experimental.session.compacting`.
   - Serialize head messages to model messages with `stripMedia: true` and `toolOutputMaxChars: 2000`.
5. **Call LLM**: Feed serialized head + summary prompt; stream the response.
6. **Handle overflow during compaction**: If the LLM returns `"compact"`, mark the compaction as errored (`ContextOverflowError`) and stop.
7. **Auto-continue** (if `auto=true` and compaction succeeded):
   - If replay was set up: re-create the replay user message (without compaction parts or media files) so the LLM continues from where it left off.
   - If no replay: inject a synthetic "Continue if you have next steps, or stop and ask for clarification" user message.
   - The synthetic message carries `{ compaction_continue: true }` metadata for provider plugins to distinguish it from manual prompts.
8. **Publish events**: `SessionEvent.Compaction.Started` / `SessionEvent.Compaction.Ended` with timestamp, summary text, and the recent tail JSON.

#### Serialization Format

Messages are serialized to text before being fed to the summary LLM (`packages/core/src/session/compaction.ts` serialize + `packages/opencode/src/session/message-v2.ts` toModelMessagesEffect):

  | Message Type        | Format                                     |
  | ------------------- | ------------------------------------------ |
  | User                | `[User]: <text> [Attached <mime>: <name>]` |
  | Assistant text      | `[Assistant]: <text>`                      |
  | Assistant reasoning | `[Assistant reasoning]: <text>`            |
  | Tool call           | `[Assistant tool call]: <name>(<input>)`   |
  | Tool result         | `[Tool result]: <truncated_output>`        |
  | Tool error          | `[Tool error]: <message>`                  |
  | System update       | `[System update]: <text>`                  |
  | Synthetic           | `[Synthetic context]: <text>`              |
  | Shell               | `[Shell]: <command>\n<truncated_output>`   |

Tool output is truncated to 2,000 characters. Media is replaced with `[Attached <mime>: <name>]` placeholders. Pruned tool outputs render as `"[Old tool result content cleared]"`.

#### Compaction Message Storage

A compaction is stored as a `"compaction"`-type part on a user message. The user message carries the trigger metadata (`auto`, `overflow`). The resulting summary is stored on the following assistant message (`summary: true`). The part also records `tail_start_id` if tail preservation happened, which gets updated if a re-compaction changes the tail boundary.

--------------------------------------------------------------------------------

### Configuration Schema

Defined in `packages/core/src/v1/config/config.ts`:

  | Key                                 | Type      | Default                | Description                                                    |
  | ----------------------------------- | --------- | ---------------------- | -------------------------------------------------------------- |
  | `compaction.auto`                   | `boolean` | `true`                 | Enable automatic compaction when context is full               |
  | `compaction.prune`                  | `boolean` | `false`                | Enable pruning of old tool outputs (lighter alternative)       |
  | `compaction.tail_turns`             | `number`  | `2`                    | Number of recent user turns to keep verbatim during compaction |
  | `compaction.preserve_recent_tokens` | `number`  | 25% of usable          | Max tokens for verbatim recent tail (clamped 2K–8K)            |
  | `compaction.reserved`               | `number`  | `min(20K, max_output)` | Token buffer to avoid overflow during compaction               |

--------------------------------------------------------------------------------

### Event System

Compaction publishes events:

- **SessionEvent.Compaction.Started** — emitted when a compaction is initiated (manual or auto)
- **SessionEvent.Compaction.Ended** — emitted on completion with the summary text and recent tail JSON
- **Event.Compacted** (local) — simple `{ sessionID }` event for internal coordination

All guarded by `flags.experimentalEventSystem`.

--------------------------------------------------------------------------------

### Plugin Hooks

  | Hook                                   | Purpose                                                     |
  | -------------------------------------- | ----------------------------------------------------------- |
  | `experimental.session.compacting`      | Inject additional context or override the compaction prompt |
  | `experimental.chat.messages.transform` | Transform the head messages before serialization            |
  | `experimental.compaction.autocontinue` | Gate whether auto-continue fires after compaction           |

--------------------------------------------------------------------------------

### Key Design Choices

1. **Two-layer architecture**: Core layer (`@opencode-ai/core`) owns the prompt template, selection, and lightweight `compactIfNeeded` check. Session layer (`@opencode-ai/opencode`) owns orchestration, persistence, replay, and event emission. Clean separation of concerns.

2. **Pruning as a lighter alternative**: Erasing old tool outputs (instead of summarizing) frees context with zero LLM calls and no information loss for recent turns. Opt-in via `compaction.prune`.

3. **Verbatim recent tail + structured summary**: Old context is replaced by a structured summary, while the most recent turns (default 2 turns, ~25% of usable) are kept verbatim. This limits information loss vs. pure summarization.

4. **Structured summary template**: Forces the LLM to preserve actionable information (file paths, decisions, blockers) in a consistent format rather than writing prose. Every section must be present, even if empty.

5. **Compaction as a message in the DAG**: Compaction trigger is stored as a `compaction`-type part on a user message, making it visible in the message DAG. The summary lives on the following assistant message.

6. **Auto-continue with replay**: After compaction, the system can optionally re-inject the user's last message so the LLM picks up where it left off, including an overflow-aware preamble when media was stripped.

7. **Previous-summary chaining**: Each compaction passes the previous summary as context to the next, so the LLM iteratively updates rather than regenerating from scratch.

8. **Plugin extensibility**: Compaction process has three hook points for plugins to inject context, transform messages, or gate auto-continue.

9. **Unified overflow check**: A single `usable()` function in `overflow.ts` computes the available token budget respecting model limits, reserved buffer, and configured overrides. Used by both pruning and full compaction.

10. **Compaction error handling**: If the compaction LLM call itself overflows (the head messages + summary prompt exceed the model), the compaction is marked as a `ContextOverflowError` and the session stops. The compaction tail boundary is updated on re-compaction if needed.

--------------------------------------------------------------------------------

### Comparison with Related Approaches

  | Aspect                              | opencode                                              | pi                                       | ds4_agent                                    |
  | ----------------------------------- | ----------------------------------------------------- | ---------------------------------------- | -------------------------------------------- |
  | Pruning                             | Yes — old tool outputs erased before full compaction  | No                                       | No                                           |
  | Summary format                      | Structured sections (Goal, Progress, Decisions, etc.) | Structured sections (similar)            | Free-form task-state summary                 |
  | Tail preservation                   | Last N turns (default 2) at ~25% budget               | `keepRecentTokens` (default 20K)         | Last ~10% of context (capped 50K)            |
  | Cut alignment                       | Turn boundaries with mid-turn splitting               | Turn boundaries with mid-turn splitting  | Nearest user-message boundary                |
  | Prior-summary chaining              | Yes — passes previous summary as context              | Yes — passes previous summary as context | No (single-shot replacement)                 |
  | Auto-continue after compaction      | Yes — replay or synthetic continue message            | No explicit replay                       | Bash-job reminder injection                  |
  | Plugin hooks                        | 3 hooks for context/prompt/auto-continue              | Extension system via hooks               | No plugin system                             |
  | Compaction as message in DAG        | Yes — compaction part on user message                 | Yes — CompactionEntry in entry array     | No — inline markers in transcript            |
  | Override during compaction overflow | Stops with error                                      | Not handled explicitly                   | Invalidates session, restores old transcript |
  | Event system                        | Dual events (started/ended) with recent tail          | No events                                | No events                                    |

## Implications for cake

- **The `usable()` + reserved-buffer pattern** is a clean overflow check cake could adopt. It respects both the model's `context` limit and an independent `input` limit (some providers separate them). The `min(20K, maxOutputTokens)` reserved default is a sensible heuristic.
- **Tool-output pruning** is a cheap, no-LLM-call first defense before full compaction. Cake could implement this independently of summarization; the `time.compacted` marker pattern is straightforward to persist.
- **The turn-boundary selection with mid-turn splitting** is more nuanced than pi's approach. The explicit `splitTurn` function that walks forward through a turn until remaining messages fit the budget handles edge cases where a single reasoning turn dominates context.
- **Compaction as a message in the DAG** (compaction part on user message, summary on assistant message) is a natural fit for cake's session format. The `tail_start_id` reference is useful for knowing which messages survived verbatim.
- **Auto-continue with replay** is a powerful UX improvement that pi and ds4 lack. Cake should consider it if adding compaction.
- **Previously-compacted turn hiding** (`completedCompactions` → `hidden` set) prevents redundant re-summarization — important for sessions with multiple compaction cycles.
- **The `buildPrompt` structured template** is more prescriptive than ds4's free-form instruction. The "every section must be present, even if empty" rule reduces variance. Cake could adapt this template or adopt its own.
- **Plugin hooks at three phases** is more than cake needs today, but the pattern of letting the prompt be customized is worth noting.
- **Core/session layer separation** is worth considering if cake wants the prompt/selection logic testable independently of the session orchestration.

## Follow-ups

- Cross-reference with existing cake notes:
  - `topics/context-compaction.md`
  - `topics/context-management.md`
  - `sources/pi-compaction.md`
  - `sources/ds4-agent-compaction.md`
- Consider whether opencode's `tail_turns` (default 2) vs. pi's `keepRecentTokens` (default 20K) vs. ds4's `~10%/50K cap` suggests a sweet spot for cake's eventual design.
- The auto-continue replay pattern is worth a standalone design note — it's the most distinct UX improvement over pi and ds4.
- Open question: opencode uses `Effect` (Effect-TS) for all orchestration. Its `structuredClone` + `select` pattern for building the summary input suggests careful attention to not mutating the original message objects — worth noting for cake's Rust ownership model.
