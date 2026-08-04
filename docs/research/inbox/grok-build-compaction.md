# Context Compaction in grok-build

An architectural reference for implementing context compaction in a coding agent.

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## Table of Contents

1. [What Is Context Compaction](#1-what-is-context-compaction)
2. [Trigger Model](#2-trigger-model)
3. [Compaction Strategies](#3-compaction-strategies)
4. [Conversation Preparation (Stripping)](#4-conversation-preparation-stripping)
5. [The Full-Resume Loop](#5-the-full-resume-loop)
6. [Two-Pass (Prefire) Compaction](#6-two-pass-prefire-compaction)
7. [Safety, Guards, and Recovery](#7-safety-guards-and-recovery)
8. [Fallback: Simple Token Budget Truncation](#8-fallback-simple-token-budget-truncation)
9. [Configuration Reference](#9-configuration-reference)
10. [User-Facing Events](#10-user-facing-events)

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## 1 What Is Context Compaction

Coding agents accumulate tokens rapidly: tool call definitions, bash output, file reads, edit diffs, error logs. The model's context window is finite (typically 128K--200K tokens). Once the conversation exceeds \~85% of the window, the agent will hit a `400 context_length_exceeded` error and fail.

**Compaction** is the process of shrinking the conversation history by:

1. Sending the existing conversation to an LLM (the "compaction model") with a summarization prompt
2. Receiving a condensed summary
3. Replacing the full conversation (or parts of it) with `[system prompt + summary + recent tail]`
4. Injecting an auto-continue prompt so the agent resumes seamlessly

The goal is to keep the agent running indefinitely without losing important context.

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## 2 Trigger Model

### 2.1 Default Threshold

**85%** of the model's context window.

The threshold is a percentage (0--100) rather than an absolute token count so it works across models with different context windows.

### 2.2 When Triggering Happens

Compaction is checked at **three points** in the agent loop:

  | Trigger Point    | Where                                           | Why                                                             |
  | ---------------- | ----------------------------------------------- | --------------------------------------------------------------- |
  | **Pre-sampling** | Before building each sampling request           | Avoid sending an oversized prompt; compact before it would fail |
  | **On error**     | When the sampler returns a context-length error | Recover from a 400 without losing the session                   |
  | **Manual**       | User invokes `/compact`                         | Explicit user request                                           |

### 2.3 Trigger Logic (Pre-Sampling)

```
fn check_auto_compact_needed() -> Option<TriggerInfo>:
    1. If a memory flush is in progress, return None (wait for it)
    2. Get estimated_total_tokens from chat state
    3. Get context_window from sampling config (model's max)
    4. Call exceeds_threshold(total_tokens, context_window, threshold_percent)
    5. If true, return TriggerInfo { tokens_used, context_window, percentage }
    6. Otherwise return None
```

The `exceeds_threshold` function:

```
fn exceeds_threshold(tokens, window, threshold_pct) -> bool:
    return (tokens * 100 / window) >= threshold_pct
```

### 2.4 Trigger Logic (On Error)

```
fn should_compact_on_error(error) -> bool:
    1. If auto-compact is suppressed, return false
    2. If error has no model_metadata.context_window, return false
    3. Get estimated_total_tokens from chat state
    4. Return estimated_total_tokens > context_window
```

On true, this also updates the session's stored `context_window` to match the model's reported value (in case of drift), then runs compaction and returns `CompactAndResubmit` to retry the sampling call.

### 2.5 Trigger Suppression

After a compaction attempt fails with a deterministic error, auto-compaction is suppressed to avoid retry loops:

  | Suppression Reason      | Scope                        | Cleared By                    |
  | ----------------------- | ---------------------------- | ----------------------------- |
  | Credit block            | Until a successful model 200 | Next successful API call      |
  | Auth failure            | Until credentials recover    | Login / token refresh         |
  | Conversation too large  | Sticky                       | Only on context-budget change |
  | Schema validation error | Sticky                       | Only on context-budget change |
  | Other                   | Per-turn                     | Next turn automatically       |

When suppressed, the session emits an `AutoCompactFailed` notification with a user-facing message.

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## 3 Compaction Strategies

### 3.1 Full-Replace (Default / Legacy)

**What it does**: Sends the entire conversation (minus stripped parts) to a compaction LLM with a summarization prompt. Replaces the conversation with:

```
[system instruction] + [LLM summary] + [auto-continue prompt]
```

The "recent tail" is kept: assistant messages and tool results after the last user turn are preserved verbatim and appended after the summary.

**When**: Auto-compact trigger or manual `/compact`.

### 3.2 Intra-Compaction (Granular Modes)

A newer system with four modes (`IntraCompactionMode`):

  | Mode               | What Gets Compacted                            | What's Preserved            |
  | ------------------ | ---------------------------------------------- | --------------------------- |
  | `FullReplace`      | Everything                                     | Nothing (like legacy)       |
  | `StepsOnly`        | Current loop's accumulated step turns          | Prior history + recent tail |
  | `HistoryOnly`      | Prior conversation history                     | Current loop's steps        |
  | `HistoryThenSteps` | History first, then steps if still over budget | Recent tail                 |

**Configuration** (each field has a default):

```rust
struct IntraCompactionConfig {
    enabled: bool,                    // default: false
    mode: IntraCompactionMode,        // default: FullReplace

    // Trigger
    trigger_threshold_percent: u8,    // default: 85
    min_steps_before_compact: u32,    // default: 3 (partial modes only)
    min_compactable_tokens: u32,      // default: 5000

    // Reduction guard
    max_reduction_ratio: f64,         // default: 0.8 (20% min reduction)

    // LLM call
    compaction_model_name: Option<String>, // None = use session model
    sampling_timeout_secs: u64,       // default: 120
    max_attempts: u32,                // default: 2
    retry_delay_secs: u64,            // default: 3

    // Target usage after compaction (partial modes)
    target_threshold_percent: u8,     // default: 50

    // HistoryThenSteps only
    steps_trigger_ratio: f64,         // default: 0.3
}
```

### 3.3 Manual `/compact`

User-triggered compaction via slash command. Works identically to auto-compact (calls `run_compact()`) but includes an optional user context string and is never suppressed.

### 3.4 Orchestration Flow

```
apply_intra_compaction(stream, sampler, config, trigger, token_counter, observer):
    match config.mode:
        FullReplace     -> apply_full_replace_compaction(...)
        StepsOnly       -> apply_steps_compaction(...)
        HistoryOnly     -> apply_history_compaction(...)
        HistoryThenSteps -> apply_history_compaction(...)
                            then if steps tokens > history * steps_trigger_ratio:
                                apply_steps_compaction(...)
```

Each `apply_*_compaction` function follows the same skeleton:

1. **Select** --- Choose which turns to compact (based on token budget)
2. **Sample** --- Send the selected turns to the compaction LLM for a summary
3. **Guard** --- Validate the summary (not degenerate, sufficient reduction)
4. **Commit** --- Replace the compacted turns with the summary in the conversation

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## 4 Conversation Preparation (Stripping)

Before sending the conversation to the compaction LLM, the following transformations are applied:

### 4.1 `prepare_conversation_for_summarization()`

```rust
fn prepare_conversation_for_summarization(conversation):
    1. strip_tool_messages()
       - Drop all ToolResult items
       - For each Assistant item with tool_calls:
         - Replace tool_calls with a text annotation:
           "[Called tools: edit, bash, read]"
         - Clear tool_calls list
       - Rationale: Tool results are bulky; the summary only needs to know
         what tools were called, not the full outputs

    2. strip_reasoning_blocks()
       - Drop all Reasoning items
       - Rationale: The text mutation in step 1 invalidates signed thinking
         blocks; strict providers reject them with 400

    3. strip_images()
       - Replace every ContentPart::Image with "[image]"
       - Rationale: Base64 image data can be megabytes; the summary doesn't
         need the pixel data

    Note: Step order matters — images before reasoning because reasoning
    removal changes item indices
```

### 4.2 System Tag Stripping

When extracting a user query from a message, these XML-style tags are stripped:

```
<user_info>, <project_layout>, <git_status>, <fork-context>,
<system-reminder>, <agent-memory>, <system_reminder>,
<background_context>, <command-name>, <command-message>, <command-args>
```

Any text wrapped in `<user_query>...</user_query>` is extracted as the real query; everything outside is discarded.

### 4.3 Synthetic Turn Detection

A user turn is **not a real user turn** (and thus eligible for merging/skipping during compaction) if any of:

- It has `synthetic_reason: Some(...)` set (e.g., system reminder)
- It's an image-only prompt with no text
- Its extracted query text is empty, `__auto_continue__`, or the exact auto-continue prompt text

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## 5 The Full-Resume Loop

After compaction succeeds, the agent must resume seamlessly. Here's the sequence:

### 5.1 Auto-Continue Prompt

```rust
const AUTO_CONTINUE_PROMPT: &str =
    r#"Continue the conversation from where it left off without asking
the user any further questions. Resume directly - do not acknowledge the
summary, do not recap what was happening, do not preface with "I'll continue"
or similar. Pick up the last task as if the break never happened."#;
```

This is injected as a `User` conversation item after the compacted history.

### 5.2 The Full Sequence

```
1. Pre-sampling check fires (usage > 85%)
2. Emit CompactionStarted notification to the UI
3. Optionally run memory flush (model summarizes important info to persistent memory)
4. Prepare conversation for summarization (strip tools, reasoning, images)
5. Call compaction LLM with summarization prompt, get summary back
6. Validate the summary:
   - Not empty
   - Tokens_after / tokens_before < max_reduction_ratio (e.g., 0.8)
   - Not a "degenerate" summary (repeating phrases, truncation artifacts)
7. Replace conversation: [system + preserved_prefix + summary + recent_tail]
8. Append auto-continue prompt as a User item
9. Emit CompactionCompleted notification with token before/after and timing
10. Build the next sampling request (which now fits in the context window)
11. Model resumes without any observable interruption to the user
```

### 5.3 Degenerate Summary Detection

The function `is_degenerate_summary()` checks for patterns like:

- Repeated single characters or punctuation (`.......`, `----`)
- Very short summaries (below a minimum length)
- Truncation artifacts (e.g., ending mid-sentence without completion)

If the summary is degenerate, compaction is retried or aborted.

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## 6 Two-Pass (Prefire) Compaction

An optimization to reduce perceived compaction latency:

### 6.1 How It Works

**Pass 1** (background, triggered at \~75% usage --- threshold minus `prefire_lead_percent`):

1. Split the conversation at a safe boundary (usually at the earliest real user turn that keeps usage under target)
2. Send the *prefix* to the compaction LLM and cache the resulting NOTE₁ summary
3. The conversation is *not* modified yet

**Pass 2** (triggered at 85% usage, the normal threshold):

1. Verify NOTE₁ is still valid (fingerprint the prefix to detect edits/rewinds since pass 1)
2. Summarize the *recent tail* (everything after the split point) as NOTE₂
3. Concatenate: `NOTE₁ + NOTE₂` as the final summary
4. Replace and resume as normal

The benefit: pass 1 runs in the background while the agent is working, so pass 2 only has to summarize the (small) recent tail. Total compaction time drops from seconds to milliseconds in the best case.

### 6.2 Prefix Fingerprint

```rust
fn fingerprint_prefix(items: &[ConversationItem]) -> u64 {
    // Hash the length, variant tags, and text content of each item
    // If this doesn't match the pass-1 cache, NOTE₁ is stale
}
```

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## 7 Safety, Guards, and Recovery

### 7.1 Pre-Compaction Memory Flush

Before compaction, the agent may ask the model to write important context to persistent memory. This ensures information isn't lost when the conversation is summarized.

Configuration:

```rust
struct CompactionPolicy {
    memory_flush_enabled: bool,  // default: false
    // ...
}
```

The flush rate is throttled: it runs at most once per `N` compaction cycles (configurable).

### 7.2 Reduction Guard

If the compaction summary didn't shrink the conversation by at least `max_reduction_ratio` (default 0.8 = 20% minimum reduction), the compaction is discarded and treated as failed. This prevents wasting tokens on summaries that barely help.

### 7.3 Minimum Compactable Size

Compaction is skipped entirely if the reducible tokens are below `min_compactable_tokens` (default 5000). Below this threshold, the LLM overhead outweighs the savings.

### 7.4 Compaction LLM Call Safety

- **Timeout**: Default 120 seconds wall-clock timeout
- **Retries**: 2 attempts total (first try + one retry), with 3-second delay between retries
- **Empty response**: Any retry that returns empty is retried; all empty = `EmptyResponse` error
- **Stream errors**: Treated the same as empty responses

### 7.5 Context Too Large (Last Resort)

If the conversation still exceeds the context window after compaction (e.g., because the summary itself is too large, or the compaction model hallucinated), the session emits a `ContextTooLarge` event and shows an actionable message to the user. This should never happen in practice --- it's a safety net.

### 7.6 Two-Pass Cache Invalidation

If the conversation prefix changes between pass 1 and pass 2 (due to an edit, rewind, or branch), the cached NOTE₁ is dropped and pass 2 falls back to summarizing everything.

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## 8 Fallback: Simple Token Budget Truncation

When compaction itself fails (LLM unavailable, timeout, degenerate output), a simpler fallback exists:

### 8.1 `fit_conversation_to_budget()`

Located in `xai-chat-state/src/compaction_utils.rs`.

```rust
fn fit_conversation_to_budget(conversation, max_tokens) -> Vec<ConversationItem> {
    // 1. If total <= max_tokens, return as-is
    // 2. Keep the System item (first item, if present)
    // 3. Walk backward from the end, dropping turns until within budget
    // 4. Never split a ToolResult from its owning Assistant (tool_use)
    // 5. If the last turn doesn't fit, truncate its text content with
    //    a "[... truncated N bytes ...]" marker
}
```

### 8.2 Text Truncation

```rust
fn truncate_text_to_bytes(s: &str, max_bytes: usize) -> Option<Arc<str>> {
    // Reserve 64 bytes for a truncation marker
    // Keep a prefix of the text at the byte boundary
    // Append "[... truncated {N} bytes to fit the compaction window ...]"
}
```

This is a lossy fallback --- it drops entire turns of conversation without LLM summarization. It should only be used when the LLM-based compaction path is unavailable.

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## 9 Configuration Reference

### 9.1 CompactionPolicy

```rust
struct CompactionPolicy {
    /// Percentage of context window that triggers auto-compaction (0-100).
    /// Default: 85
    auto_compact_threshold_percent: u32,

    /// Model to use for summarizing the conversation.
    /// None = use the session's current model.
    compact_model: Option<String>,

    /// Whether to run a memory flush before each compaction.
    /// Default: false
    memory_flush_enabled: bool,

    /// Wall-clock timeout per compaction generation (seconds).
    /// Default: 300
    wall_clock_budget_secs: u64,

    /// Enable two-pass (prefire) compaction.
    /// Default: false
    two_pass_enabled: bool,
}
```

### 9.2 Resolution Priority

For `auto_compact_threshold_percent`, the value is resolved from:

1. Environment variable (highest priority)
2. Session config file (model-specific)
3. Global config
4. Default: **85**

### 9.3 Key Constants

  | Constant               | Value | Purpose                            |
  | ---------------------- | ----- | ---------------------------------- |
  | Default threshold      | 85%   | Trigger auto-compact               |
  | Prefire lead           | 10%   | Start background pass-1 at 75%     |
  | Min reduction ratio    | 0.8   | Discard compaction if <20% savings |
  | Min compactable tokens | 5000  | Skip compaction below this         |
  | Compaction timeout     | 120s  | Per LLM call                       |
  | Max attempts           | 2     | First try + one retry              |
  | Retry delay            | 3s    | Between compaction attempts        |
  | Segment max tokens     | 3000  | Per-segment in multi-pass          |

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## 10 User-Facing Events

The UI (scrollback) shows these events during compaction:

  | Event                                    | Display                                                   |
  | ---------------------------------------- | --------------------------------------------------------- |
  | `CompactionStarted { 85% }`              | "Context 85% full. Compacting…"                           |
  | `CompactionCompleted { 45K, 12K, 2.3s }` | "Context compacted: 45K → 12K tokens (2.3s)"              |
  | `CompactionFailed { error }`             | "Compaction failed: {error}"                              |
  | `CompactionCancelled`                    | (silent — turn was cancelled)                             |
  | `ContextTooLarge`                        | Prominent: "Conversation too large — start a new session" |
  | `RetryFailed`                            | Shows error + error type                                  |
  | `ReAuthRequired`                         | Prompts user to re-authenticate                           |

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## Implementation Checklist

To implement context compaction in a new coding agent:

1. **Token tracking**: Maintain an accurate running token estimate of the conversation
2. **Threshold check**: Before each sampling call, check `tokens / context_window >= threshold`
3. **Conversation preparation**: Strip tool results, reasoning blocks, and images before summarization
4. **Summarization LLM call**: Send the prepared conversation with a prompt like "Summarize this conversation concisely, preserving key decisions, errors, and current task state"
5. **Validation**: Check the summary isn't degenerate, is sufficiently smaller than the original
6. **Replace and resume**: Swap the conversation, inject auto-continue prompt, retry the sampling call
7. **Error recovery**: On 400 context-length errors, auto-trigger compaction and resubmit
8. **Suppression**: Add sticky suppression for deterministic failures to avoid infinite retry loops
9. **UI events**: Surface compaction progress to the user so they understand the pause
10. **Prefire optimization**: Optionally start background pass-1 before hitting the threshold
