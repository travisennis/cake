# Cache Break Detection: After-the-Fact Algorithm

This document describes the algorithm used by [pi](https://github.com/earendil-works/pi-mono) to detect prompt-cache breaks after each turn completes. The detection is purely **reactive**: it infers breaks from the provider's reported usage metrics rather than predicting them. It works for **Chat Completions API** and **Responses API** (OpenAI-style caching, with or without Anthropic-style `cache_control` markers).

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## Concepts

Every provider turn returns a usage object with three token categories:

  | Token category             | Chat Completions field (OpenAI)                  | Responses API field                             |
  | -------------------------- | ------------------------------------------------ | ----------------------------------------------- |
  | **Cache read (hit)**       | `usage.prompt_tokens_details.cached_tokens`      | `usage.input_tokens_details.cached_tokens`      |
  | **Cache write (creation)** | `usage.prompt_tokens_details.cache_write_tokens` | `usage.input_tokens_details.cache_write_tokens` |
  | **Regular input**          | Everything else in `usage.prompt_tokens`         | Everything else in `usage.input_tokens`         |

Normalization (common internal representation):

```
input    = prompt_tokens - cached_tokens - cache_write_tokens
cacheRead  = cached_tokens
cacheWrite = cache_write_tokens
promptTokens = input + cacheRead + cacheWrite
```

Truth table for a typical session (Anthropic-style, one user turn after the system prompt and tools were set up):

  | Turn | `input` | `cacheRead` | `cacheWrite` | `promptTokens` | What happened                                  |
  | ---- | ------- | ----------- | ------------ | -------------- | ---------------------------------------------- |
  | 1    | 0       | 0           | 100k         | 100k           | First turn: all writes, nothing cached yet     |
  | 2    | 0       | 95k         | 5k           | 100k           | Healthy: most read from cache, small new write |
  | 3    | 100k    | 0           | 0            | 100k           | Full miss: nothing was cached                  |
  | 4    | 5k      | 95k         | 0            | 100k           | Healthy again                                  |

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## Data Structures

```rust
/// The last request seen by the scanner; everything in its prompt should be cached.
struct PreviousRequest {
    /// Total prompt tokens (input + cacheRead + cacheWrite) of the previous turn.
    prompt_tokens: u64,
    /// Provider/model identifier string (e.g. "openai/gpt-4o").
    model_key: String,
    /// Timestamp of the previous turn (ms since epoch).
    timestamp: u64,
    /// Whether any earlier turn in this scan segment reported non-zero cache
    /// activity. Distinguishes a total miss on a cache-reporting provider from
    /// a provider that never reports caching at all.
    reported_cache: bool,
}

/// A single detected cache miss on one assistant message.
struct CacheMiss {
    /// Prompt tokens that were in the previous turn's prompt but not read from cache.
    missed_tokens: u64,
    /// Extra dollars paid vs. a full cache hit; 0 when pricing is unknown.
    missed_cost: f64,
    /// Milliseconds since the previous request (which last refreshed the cache).
    idle_ms: u64,
    /// True when the model changed relative to the previous request.
    model_changed: bool,
}

/// Aggregate cache waste across a session.
struct CacheWasteTotals {
    missed_tokens: u64,
    missed_cost: f64,
    /// Number of counted misses (turns above the noise floor).
    miss_count: u64,
}

/// Minimal pricing lookup. Cost is $/million tokens.
trait ModelPriceSource {
    fn get_model(&self, provider: &str, model_id: &str)
        -> Option<ModelPrice>;
}

struct ModelPrice {
    /// Price per 1M tokens for cache-read (hit) tokens.
    cache_read: f64,
}
```

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## Core Algorithm

### 1. Compute the previous request state from one message

```rust
fn as_previous_request(
    message: &AssistantMessage,
    reported_cache_so_far: bool,
) -> Option<PreviousRequest> {
    let usage = &message.usage;
    let prompt_tokens = usage.input + usage.cache_read + usage.cache_write;

    if prompt_tokens == 0 {
        return None; // no meaningful prompt, skip
    }

    Some(PreviousRequest {
        prompt_tokens,
        model_key: format!("{}/{}", message.provider, message.model),
        timestamp: message.timestamp,
        reported_cache: reported_cache_so_far
            || usage.cache_read > 0
            || usage.cache_write > 0,
    })
}
```

### 2. Detect a miss for one message vs. the previous request

```rust
const NOISE_FLOOR_TOKENS: u64 = 1024;

fn detect_miss(
    prev: Option<&PreviousRequest>,
    message: &AssistantMessage,
    prices: &dyn ModelPriceSource,
) -> Option<CacheMiss> {
    let usage = &message.usage;
    let prompt_tokens = usage.input + usage.cache_read + usage.cache_write;

    let prev = match prev {
        Some(p) => p,
        None => return None, // first turn; nothing to compare against
    };

    // No prompt or zero cache activity with no prior cache signal -> skip.
    if prompt_tokens == 0 {
        return None;
    }
    if usage.cache_read + usage.cache_write == 0 && !prev.reported_cache {
        return None; // provider never reports caching
    }

    // The key question: of this turn's prompt tokens, how many were in the
    // *previous* turn's prompt and should have been cache reads?
    let missed_tokens =
        std::cmp::min(prev.prompt_tokens, prompt_tokens).saturating_sub(usage.cache_read);

    if missed_tokens <= NOISE_FLOOR_TOKENS {
        return None; // noise
    }

    // Extra cost = missed tokens billed at the actual paid rate instead of
    // the cache-read rate. Missed tokens can only land in input or cacheWrite
    // buckets, so the paid rate comes from this message's own cost breakdown.
    let paid_tokens = usage.input + usage.cache_write;
    let paid_per_token = if paid_tokens > 0 {
        (usage.cost.input + usage.cost.cache_write) / paid_tokens as f64
    } else {
        0.0
    };

    let read_per_token = if usage.cache_read > 0 {
        usage.cost.cache_read / usage.cache_read as f64
    } else {
        // Fall back to the model's listed cache-read price
        prices
            .get_model(&message.provider, &message.model)
            .map_or(0.0, |p| p.cache_read / 1_000_000.0)
    };

    let idle_ms = (message.timestamp as i64 - prev.timestamp as i64).max(0) as u64;
    let model_changed = format!("{}/{}", message.provider, message.model) != prev.model_key;

    Some(CacheMiss {
        missed_tokens,
        missed_cost: missed_tokens as f64 * (paid_per_token - read_per_token).max(0.0),
        idle_ms,
        model_changed,
    })
}
```

### 3. Scan a sequence of session entries

The scan iterates through the session history in chronological order, tracking the "previous request" state and accumulating misses. Two entry types reset the state:

- **Compaction** (context was summarized -- the next turn legitimately has new content, so no miss is counted)
- **Branch summary** (same reasoning as compaction)

```rust
#[derive(Clone)]
enum SessionEntry {
    Message { message: AssistantMessage },
    Compaction,
    BranchSummary,
}

fn scan(
    entries: &[SessionEntry],
    prices: &dyn ModelPriceSource,
) -> (Option<PreviousRequest>, CacheWasteTotals, Vec<(usize, CacheMiss)>) {
    let mut prev: Option<PreviousRequest> = None;
    let mut totals = CacheWasteTotals { missed_tokens: 0, missed_cost: 0.0, miss_count: 0 };
    let mut misses: Vec<(usize, CacheMiss)> = Vec::new();

    for (i, entry) in entries.iter().enumerate() {
        match entry {
            SessionEntry::Compaction | SessionEntry::BranchSummary => {
                // Context legitimately changed. The next turn's prompt is new
                // content, not re-billed content. Model switches are NOT exempt
                // and will be caught when they happen.
                prev = None;
            }
            SessionEntry::Message { message } => {
                if let Some(miss) = detect_miss(prev.as_ref(), message, prices) {
                    totals.missed_tokens += miss.missed_tokens;
                    totals.missed_cost += miss.missed_cost;
                    totals.miss_count += 1;
                    misses.push((i, miss));
                }
                prev = as_previous_request(message, prev.map_or(false, |p| p.reported_cache))
                    .or(prev);
            }
        }
    }

    (prev, totals, misses)
}
```

### 4. Public API functions

```rust
/// Cumulative cache waste across a session.
fn compute_cache_waste(
    entries: &[SessionEntry],
    prices: &dyn ModelPriceSource,
) -> CacheWasteTotals {
    scan(entries, prices).1
}

/// All counted cache misses across a session, indexed by the entry position.
fn collect_cache_misses(
    entries: &[SessionEntry],
    prices: &dyn ModelPriceSource,
) -> Vec<(usize, CacheMiss)> {
    scan(entries, prices).2
}

/// Detect a cache miss on a just-completed assistant message.
/// `entries` must not yet contain `message`.
fn detect_cache_miss_for_new_message(
    entries: &[SessionEntry],
    message: &AssistantMessage,
    prices: &dyn ModelPriceSource,
) -> Option<CacheMiss> {
    // Scan all existing entries to compute the current `prev` state.
    let (prev, _, _) = scan(entries, prices);
    detect_miss(prev.as_ref(), message, prices)
}
```

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## What Causes a Cache Break (per the Detection Logic)

  | Trigger                    | Detected by                | Mechanism                                                                                           |
  | -------------------------- | -------------------------- | --------------------------------------------------------------------------------------------------- |
  | **Model switch**           | `model_key` changed        | The full prompt is re-billed; `cacheRead` is 0 or near 0; `missed_tokens` is high                   |
  | **Idle timeout**           | `idle_ms` high             | Provider's cache TTL expired between turns; typically 5 min for Anthropic, varies per OpenAI policy |
  | **Compaction happened**    | `prev` reset to `None`     | Compaction requests use `cacheRetention: "none"`, which skips cache markers; next user turn misses  |
  | **Context restructured**   | `prev` reset to `None`     | Tool results, system prompt changes, or message pruning that changes the prompt structure           |
  | **Provider doesn't cache** | `reported_cache` never set | All turns show `cacheRead = cacheWrite = 0`; misses are never reported                              |

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## Usage for Notifications

The algorithm is used to display a per-turn **cache miss notice** (shown to the user after a turn completes). Thresholds from the pi implementation:

```rust
/// Only show a cache miss notice if it's economically significant.
const DISPLAY_MISSED_TOKENS_THRESHOLD: u64 = 20_000;
const DISPLAY_MISSED_COST_THRESHOLD: f64 = 0.10; // $0.10

fn should_show_notice(miss: &CacheMiss) -> bool {
    miss.missed_tokens >= DISPLAY_MISSED_TOKENS_THRESHOLD
        || miss.missed_cost >= DISPLAY_MISSED_COST_THRESHOLD
}
```

Label the notice with the root cause:

```rust
const CACHE_TTL_MS: u64 = 5 * 60 * 1000; // Anthropic default, adjust per provider

fn label_notice(miss: &CacheMiss) -> String {
    if miss.model_changed {
        "Cache miss after model switch"
    } else if miss.idle_ms >= CACHE_TTL_MS {
        format!("Cache miss after {}m idle", miss.idle_ms / 60_000)
    } else {
        "Cache miss"
    }
}
```

Aggregate waste is reported in a session summary panel:

```
Tokens
  Input:      212,450
  Cached:     152,300 (71.7%)
  Uncached:    60,150 (4,200 written to cache)
  Output:      45,200
  Total:      257,650

Cost
  Total:       $0.834
  Wasted:      $0.241 (2 misses)
```

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## Provider-Specific Nuances for Your Rust Agent

### OpenAI Chat Completions (OpenAI / OpenRouter / Compatibles)

**Request side** (`prompt_cache_key` / `cache_control`):

```json
{
  "model": "gpt-4o",
  "messages": [...],
  "prompt_cache_key": "<session-id>",
  "prompt_cache_retention": "24h",
  "stream_options": { "include_usage": true }
}
```

- Some providers (Anthropic-compatible proxies via Chat Completions) support Anthropic-style `cache_control` markers on messages and the last tool. If your model compat indicates `cacheControlFormat: "anthropic"`, add `{ cache_control: { type: "ephemeral" } }` to:
  1. The system/developer message
  2. The last tool definition
  3. The last user or assistant message

- `prompt_cache_key` is typically derived from the session ID (e.g., SHA-256 hashed and base64-encoded, clamped to 64 characters). Use `clampOpenAIPromptCacheKey(session_id)`:
  ```rust
  fn clamp_openai_prompt_cache_key(session_id: &str) -> String {
      // Hash to fixed length and sanitize to alphanumeric + hyphens/underscores
      // The pi implementation uses SHA-256 hex truncated to 64 chars,
      // replacing non-alphanumeric chars with hyphens.
      let hash = sha256(session_id.as_bytes());
      hex::encode(&hash[..32]) // 64 hex chars
          .chars()
          .map(|c| if c.is_alphanumeric() { c } else { '-' })
          .collect()
  }
  ```

**Response side** (usage parsing):

```rust
struct Usage {
    prompt_tokens: u64,
    completion_tokens: u64,
    prompt_tokens_details: Option<PromptTokensDetails>,
    completion_tokens_details: Option<CompletionTokensDetails>,
}

struct PromptTokensDetails {
    cached_tokens: Option<u64>,        // cache reads (hits)
    cache_write_tokens: Option<u64>,   // cache writes (creations)
}

// Normalize:
let cached_tokens = details.cached_tokens.unwrap_or(0);
let cache_write = details.cache_write_tokens.unwrap_or(0);
let input = prompt_tokens.saturating_sub(cached_tokens + cache_write);

// OpenAI includes cached and cache-write tokens in prompt_tokens,
// so subtract both to get "fresh" input.
```

### OpenAI Responses API

**Request side**:

```json
{
  "model": "gpt-4o",
  "input": [...],
  "prompt_cache_key": "<session-id>",
  "prompt_cache_retention": "24h",
  "prompt_cache_options": { "mode": "explicit" }
}
```

- `explicit` mode: only content with `cache_control` markers gets cached.
- Older Responses API: implicit prompt caching based on `prompt_cache_key`.

**Response side**:

```rust
struct ResponseUsage {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
    input_tokens_details: Option<InputTokensDetails>,
    output_tokens_details: Option<OutputTokensDetails>,
}

struct InputTokensDetails {
    cached_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
}

// Normalize identically to Chat Completions:
let cached_tokens = details.cached_tokens.unwrap_or(0);
let cache_write = details.cache_write_tokens.unwrap_or(0);
let input = input_tokens.saturating_sub(cached_tokens + cache_write);
```

### Providers That Don't Report Cache Activity

If a provider never reports `cached_tokens` or `cache_write_tokens`, all turns will show `cacheRead = 0, cacheWrite = 0`. The algorithm handles this with the `reported_cache` latch: the first turn sees `reported_cache = false`, so it does not count a miss. All subsequent turns also see no cache activity and `reported_cache` remains false -- no misses are ever reported.

### Providers That Report Cache Reads but Not Cache Writes

OpenAI itself does not document or emit `cache_write_tokens` for Chat Completions. OpenRouter-compatible providers can include it. The algorithm handles this correctly:

- A turn with `cacheRead > 0, cacheWrite = 0` is healthy (reading from cache, but the write was not reported or happened on an earlier turn).
- A turn with `cacheRead = 0, cacheWrite = 0` but `reported_cache = true` from an earlier turn is a **total miss** -- the full prompt was re-billed.

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## When to Run Detection

### Option A: Per-turn (immediate feedback)

After an assistant message completes but before it is persisted to the session history:

```rust
let miss = detect_cache_miss_for_new_message(&entries, &new_message, &prices);
if let Some(miss) = miss {
    if should_show_notice(&miss) {
        show_cache_miss_notice(&miss);
    }
}
```

### Option B: Scanned from history (summary / replay)

On session resume or after compaction rebuild:

```rust
let (_, totals, misses) = scan(&entries, &prices);
if totals.missed_tokens > 0 {
    update_session_summary(&totals);
    // Re-emit cache miss notices (transcript rebuild).
    for (index, miss) in &misses {
        add_notice_to_transcript(*index, &miss);
    }
}
```

pi uses both: per-turn detection via `detectCacheMiss` (called during the `message_end` event callback), and history scanning via `computeCacheWaste` / `collectCacheMisses` (used for the session info panel and transcript rebuild after resume or compaction).

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## Edge Cases

1. **First turn of a session**: always returns `None` -- there is no previous request to compare against.

2. **Zero-prompt turn (aborted / empty)**: `promptTokens = 0`, skipped.

3. **Multiple consecutive tool result rounds**: tool results are folded into user messages; they don't create separate assistant entries that could be compared. Only assistant messages with usage data are checked.

4. **Compaction between turns**: resets `prev` to `None`, so the turn after compaction is treated as "first turn" and never counted as a miss. This is correct because compaction genuinely changes the prompt content.

5. **Model switch mid-session**: counted as a miss because the previous prompt (under the old model) is not cached for the new model. The `model_changed` flag lets you label the notice differently.

6. **Noise floor (1024 tokens)**: tiny misses from cache-granularity alignment are ignored. This prevents spurious notices on every turn.

7. **Zero-cost pricing fallback**: when a provider does not include cost in the usage response, `missed_cost` is 0. The `missed_tokens` count is still accurate for volume tracking.
