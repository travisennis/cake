# tact: Comparison and Port Candidates

Study of the `tact` coding agent (`github.com/rust-infra/tact`, MIT, Rust 2024) against cake, and an assessment of which of its subsystems are worth porting.

This document is self-contained: constants, algorithms, and control flow from tact are reproduced here so implementation does not require reading the tact source. Where a design decision is left open for cake, it is called out explicitly rather than assumed.

Related existing research, which this complements rather than repeats:

- `topics/context-compaction.md` — Pi's append-only compaction model. Still the better *architectural* reference for cake, because Pi separates persistence from context materialization the way cake's append-only sessions require.
- `topics/context-management.md` — cross-harness survey (Pi, OpenClaw, Claude Code, Letta) of read strategies and pruning policy.
- `sources/opencode-compaction.md`, `sources/ds4-agent-compaction.md`, `inbox/grok-build-compaction.md` — other compaction approaches.

tact's value over those notes is that it is Rust, structurally close to cake, and additionally a working reference for **subagents** and **background execution**, which the existing corpus does not cover.

## What tact is

A terminal-first agent, TUI (ratatui) with a `tact-ui headless` subcommand. Six-crate workspace:

  | Crate                  | Contents                                                       |
  | ---------------------- | -------------------------------------------------------------- |
  | `protocol`             | Wire types shared between runtime and UI (`AgentUpdate`, …)    |
  | `tact`                 | Agent runtime: loop, tools, hooks, permissions, MCP, compaction |
  | `tact-ui`              | Binary; wires `tact` + `tui`; TUI and headless entry points    |
  | `tact_llm`             | Provider adapters                                              |
  | `tui`                  | Terminal UI                                                    |
  | `tool_refactor_macros` | `#[tool]` proc macro generating schemas from Rust signatures   |

Four native providers (Anthropic Messages, OpenAI Chat Completions + Responses, DeepSeek, Kimi), SQLite session store via `sqlx`, ~30 tools, MCP client via `rmcp`, a skill-only plugin marketplace, and three permission modes (`default` / `plan` / `auto`) built around interactive approval.

The TUI is the center of gravity. Headless mode exists but is not where the design effort went — which is the inverse of cake, and the main reason most of tact's surface area is not portable.

## Where cake is already ahead

Recording this so the rest of the document is not misread as a deficiency list.

- **OS sandboxing.** cake's Seatbelt/Landlock enforcement (ADR-014, ADR-016) and the `bash_safety` parser/checks (ADR-015) have no tact equivalent. tact relies on interactive permission prompts, which do not exist in a headless context; its `PermissionPolicy::ShellCommand` classifier is a `starts_with("sudo ")` string check.
- **Per-path tool scheduling.** cake's `schedule_tool_calls` (ADR-013) groups mutating calls by canonical path and serializes within a group. tact has a coarser `ResourcePolicy::Barrier`.
- **Machine-readable contracts.** `--output-format json` / `stream-json`, `--output-schema` (ADR-012), documented exit codes, append-only versioned session records (ADR-004), telemetry sidecar (ADR-007). tact has none of these.
- **Conversation repair on resume.** `ConversationState::with_restored_history` synthesizes failed outputs for function calls a previous process abandoned, and queues those repairs for persistence. tact's SQLite restore has no equivalent.
- **Trusted-extension boundary.** cake's toolbox contract (ADR-017) is a narrower, better-documented trust boundary than tact's plugin/MCP loading.

## Gap 1: cumulative context management

### What cake does today

cake bounds every *individual* tool result:

  | Surface | Cap                                                                | Overflow behavior                                                                |
  | ------- | ------------------------------------------------------------------ | -------------------------------------------------------------------------------- |
  | Bash    | `BASH_OUTPUT_MAX_BYTES` = 50 000 (read cap 100 000)                | Full text written to `secure_temp_dir()/bash_output_{uuid}.txt`, path returned    |
  | Read    | `MAX_OUTPUT_BYTES` = 100 000; default range lines 1–200            | Inline `[... output truncated at N bytes ...]` marker                            |
  | Toolbox | `MAX_OUTPUT_BYTES` = 50 000, stderr 10 000, stderr-in-error 2 000  | Inline `[... toolbox output truncated at N bytes ...]` marker; process killed     |

What cake does **not** bound is the accumulation. `ConversationState.history` grows monotonically for the life of a run. The only response to context pressure is `apply_context_overflow_override` in `src/clients/retry.rs`, which:

1. parses the provider error body, requiring both `"context limit"` and `"max_tokens"` in the message (`parse_context_overflow`);
2. computes `available_output = context_limit - input_tokens`, applies a 1024-token safety buffer, floors at `MIN_CONTEXT_OVERFLOW_OUTPUT_TOKENS` = 256;
3. retries once with a reduced `max_output_tokens`, setting `RequestOverrides.context_overflow_retry_used`.

After that single retry the run fails. Fifty tool calls at 50 KB each is 2.5 MB of history against a 200 K-token window, and the failure mode is a hard stop partway through a task with the work only partly done. For a tool whose stated purpose is unattended CI use, this is the ceiling that matters.

Note also that `parse_context_overflow` is string-matching one provider's phrasing. Providers that word the error differently skip the override entirely and fail on the first overflow.

### tact's three tiers

Source: `crates/tact/src/compact/mod.rs`, `crates/tact/src/agent/mod.rs`, `crates/tact/src/recovery.rs`.

All constants are compile-time in tact; none are configurable.

```
Tier 1  Large output persist   any tool result > 30 000 chars  ->  disk + preview envelope
Tier 2  Micro-compact          before every LLM call           ->  stub old tool results
Tier 3  Full compaction        80% of window, or overflow      ->  transcript + LLM summary + rebuild
```

#### Tier 1 — large output persist

```rust
const PERSIST_THRESHOLD: usize = 30_000;   // characters, not bytes
const PREVIEW_CHARS: usize = 2_000;
const MAX_COMPACT_ARTIFACTS: usize = 100;
```

If `output.chars().count() > PERSIST_THRESHOLD`, write the full text to `<workdir>/.tact/tool-results/{tool_use_id}.txt`, prune the directory to the 100 newest files (mtime order, never removing the file just written), and return:

```
<persisted-output>
Full output saved to: /abs/path/.tact/tool-results/{id}.txt
Preview:
{first 2000 chars}
</persisted-output>
```

The XML tags are for the model, not for runtime parsing — they mark the block as a system envelope so the metadata is not mistaken for command output. This is deliberately distinct from the Tier 2 stub wording so the model can tell "recoverable at a path" from "gone, re-run the tool."

Applied to all native and MCP tool results **except** `read_file`, which already returns a line/token-bounded page with its own continuation marker.

#### Tier 2 — micro-compaction

```rust
const KEEP_RECENT_TOOL_RESULTS: usize = 12;
const COMPACTED_TOOL_RESULT: &str =
    "[Earlier tool result compacted. Re-run the tool (e.g., read_file) for full content.]";
const _: () = assert!(COMPACTED_TOOL_RESULT.len() < 120);
```

The const assertion is load-bearing: the stub must be shorter than the 120-char threshold below, or repeated passes would re-stub already-stubbed content.

```rust
pub fn micro_compact(messages: &mut [Message], enabled: bool) {
    if !enabled { return; }
    // (message_idx, block_idx) for every ToolResult block inside a User message,
    // in chronological order.
    let positions = collect_tool_result_positions(messages);
    if positions.len() <= KEEP_RECENT_TOOL_RESULTS { return; }

    let compact_until = positions.len() - KEEP_RECENT_TOOL_RESULTS;
    for (message_idx, block_idx) in positions.into_iter().take(compact_until) {
        // ... resolve the ToolResult block ...
        if tool_content.chars().count() > 120 {
            *tool_content = COMPACTED_TOOL_RESULT.to_string();
        }
    }
}
```

No LLM call, no I/O, pure mutation of the message slice. Runs at the top of every agent-loop iteration when enabled. Short results are left alone — high information density per token, not worth the churn.

tact's own documented weaknesses, worth inheriting as known limitations rather than rediscovering:

- Recency-and-length is a crude selector. A critical file read at position 13 is stubbed while twelve trivial `ls` outputs survive.
- The model may not notice the stub and will confabulate the missing content, or attempt an edit against text it can no longer see.
- **Prefix cache invalidation.** Rewriting an old tool result changes the prompt prefix, so auto-caching providers (OpenAI, DeepSeek) miss the cache on every affected turn. Anthropic is less affected because explicit `cache_control` breakpoints sit before tool results. tact runs this unconditionally every turn and lists "gate on context usage, e.g. only above 50% of window" as its top planned fix.

#### Tier 3 — full compaction

```rust
const AUTO_COMPACT_THRESHOLD_PERCENT: usize = 80;
const COMPACT_REBUILD_HEADROOM_PERCENT: usize = 20;
pub const KEEP_USER_MESSAGE_TOKENS: usize = 20_000;   // ~80k ASCII chars
pub const SUMMARY_PREFIX: &str =
    "This conversation was compacted so the agent can continue working.";
const OMITTED_IMAGE: &str = "[Earlier image attachment omitted during compaction.]";

// in agent/mod.rs
const COMPACT_SUMMARY_MAX_TOKENS: u32 = 2_000;
const COMPACT_SUMMARY_OUTPUT_PERCENT: usize = 20;
const COMPACT_SUMMARY_HEADROOM_PERCENT: usize = 10;
```

**Token estimation.** Deliberately conservative and cheap — no tokenizer dependency:

```rust
fn approx_text_tokens(text: &str) -> usize {
    let (ascii, non_ascii) = /* count chars by class */;
    ascii.div_ceil(4).saturating_add(non_ascii)   // non-ASCII counted 1 token per char
}

fn estimate_context_tokens(messages: &[Message]) -> usize {
    match serde_json::to_string(messages) {
        Ok(s) => approx_text_tokens(&s),
        Err(_) => usize::MAX / 2,   // fail toward compacting, never toward overflow
    }
}
```

Estimating over the *serialized JSON* rather than the text content is what makes this safe — it counts structural overhead, tool arguments, and ids, which a naive content-only estimate misses.

**Trigger:**

```rust
fn should_auto_compact(
    last_token_total: u32,          // total from the most recent provider usage report
    model_context_window: usize,
    estimated_context_tokens: usize,
    incoming_turn_tokens: usize,    // a turn not yet appended to context
    max_tokens: usize,              // reserved output
) -> bool {
    if model_context_window == 0 { return false; }
    let threshold = model_context_window
        .saturating_mul(AUTO_COMPACT_THRESHOLD_PERCENT).div_ceil(100);

    // Primary: trust reported usage when available.
    if last_token_total > 0
        && (last_token_total as usize)
            .saturating_add(incoming_turn_tokens)
            .saturating_add(max_tokens) >= threshold
    { return true; }

    // Fallback: local estimate.
    estimated_context_tokens
        .saturating_add(incoming_turn_tokens)
        .saturating_add(max_tokens) >= threshold
}
```

Called twice per cycle: once at loop entry with `incoming_turn_tokens` set to the size of the user turn about to be pushed (so a large prompt cannot overflow immediately after append), and once per iteration after micro-compaction with `incoming_turn_tokens = 0`.

Also triggered by: provider prompt-too-long error, and a successful explicit `compact` tool call.

**Procedure** (`compact_history_with_mode`):

1. **Write transcript.** `<workdir>/.tact/transcripts/transcript_{unix_nanos}_{collision}.jsonl`, one JSON message per line, opened `create_new` with a collision counter `0..100`, then pruned to `MAX_COMPACT_ARTIFACTS`. The full pre-compaction context is preserved on disk before anything is discarded.

2. **Budget the summary call.**
   ```
   summary_max_tokens   = min(window * 20%, 2_000).max(1)      // 2_000 if window == 0
   summary_input_limit  = window - summary_max_tokens - (window * 10%)
   ```
   If the instruction text alone exceeds `summary_input_limit`, bail: the window is too small to compact.

3. **Build the summarizer prompt.** Each addition is budget-checked and dropped with a `tracing::warn!` rather than truncated:
   ```
   COMPACT_SUMMARY_INSTRUCTIONS
   + "\n\nFocus to preserve next: {focus}"            (optional, from the compact tool)
   + "\n\nRecent files to reopen if needed:\n- {p}"   (up to 5, LRU)
   + "\n\n" + recent_messages_json
   ```
   The instruction text verbatim:
   ```
   Summarize this coding-agent conversation so work can continue.
   Preserve:
   1. The current goal and what has been accomplished
   2. Important findings, decisions, and architectural insights
   3. Files read or changed (with key code structures like types, signatures, APIs if relevant)
   4. Remaining work and next steps
   5. User constraints and preferences
   6. Any errors encountered and their causes
   Be compact but concrete. Preserve exact file paths, function names, and type
   signatures when they are important for continuing the work.
   ```
   `recent_files` is the last 5 distinct paths from successful `read_file` / `write_file` / `edit_file` / non-dry-run `apply_patch`, deduped LRU. It is injected into the prompt *and* appended to the final summary, so the path list survives even if the model's prose omits it.

4. **Select the history window** (`recent_messages_for_summary`). Walk messages newest-first accumulating against the budget. A message that fits is cloned whole. A message that does not gets a text-only fallback view: text and thinking blocks concatenated, tool calls rendered `[Tool call: {name}]`, tool results `[Tool result {id}]\n{content}`, images replaced by `OMITTED_IMAGE`, then tail-truncated. Reverse to chronological, serialize, and if the serialized form still exceeds budget, drop the oldest and retry.

   The point of the fallback view is that **base64 and structured payloads are never sliced** — truncating raw JSON mid-value produces an invalid prompt.

   The conversation is passed as a *serialized JSON array inside a single user message*, not as chat history. Pi does the same thing with a text serialization. Either way the summarizer must not be able to mistake the old conversation for a conversation to continue.

5. **Call the model.** One `create_message`, `max_tokens = summary_max_tokens`. Transient failures retry up to `MAX_COMPACT_SUMMARY_RETRY_ATTEMPTS` = 3 with backoff.

6. **Validate, and fail closed.** `stop_reason` must be `None` or `EndTurn`; anything else bails. An empty summary bails. **On any bail the conversation is left completely untouched** — a failed compaction must never destroy context. This also governs the explicit `compact` tool: a failed call leaves history intact and returns an error to the model.

7. **Rebuild.** Not "summary replaces everything" — recent real user turns are kept verbatim, then the summary is appended:

   ```rust
   fn is_real_user_message(m: &Message) -> bool {
       matches!(m.role, Role::User) && match &m.content {
           Text { content } => !content.starts_with(SUMMARY_PREFIX),  // never stack summaries
           Blocks { content } => content.iter().any(|b| !matches!(b, ToolResult { .. })),
       }
   }
   ```
   Tool-result-only user messages and prior summaries are excluded. Budget:
   ```rust
   fn retained_user_message_token_budget(
       window: usize, max_output: usize, non_retained_input: usize,
   ) -> usize {
       if window == 0 { return KEEP_USER_MESSAGE_TOKENS; }
       let headroom = window.saturating_mul(COMPACT_REBUILD_HEADROOM_PERCENT).div_ceil(100);
       window
           .saturating_sub(max_output + non_retained_input + headroom)
           .min(KEEP_USER_MESSAGE_TOKENS)
   }
   ```
   `non_retained_input` is system prompt + tool schemas + the summary itself. `build_compacted_history` then walks the retained users newest-first, keeping whole messages while they fit; the first message that does not fit contributes its **tail** (`take_last_tokens`) and iteration stops. Result: `[recent real user turns…] + [SUMMARY_PREFIX + "\n\n" + summary]`.

   A final full-request size guard reduces the number of retained users until the assembled request fits.

8. **Persist and reset.** `replace_session_messages` writes the new context to SQLite, the message-id window resets, and `last_token_total = 0` so the next `should_auto_compact` falls back to the estimate rather than reusing a stale pre-compaction usage figure.

   Step 8 is where tact's design is *worse* than what cake needs. tact destructively replaces the stored session; the JSONL transcript is a side file with no link back. Pi's model — append a compaction checkpoint carrying `firstKeptEntryId`, and materialize context as a projection over the append-only log — is the correct shape for cake and is already documented in `topics/context-compaction.md`.

#### Recovery (`crates/tact/src/recovery.rs`)

```rust
pub const MAX_COMPACT_ATTEMPTS: u32 = 3;               // prompt-too-long -> compact -> retry
pub const MAX_TRANSPORT_ATTEMPTS: u32 = 10;            // transient network
pub const MAX_CONTINUATION_ATTEMPTS: u32 = 3;          // max-tokens continuation
pub const MAX_COMPACT_SUMMARY_RETRY_ATTEMPTS: u32 = 3;

// min(1s * 2^attempt, 30s) + jitter in [0, 1)s
pub fn backoff_delay(attempt: u32) -> Duration;

pub fn is_prompt_too_long_error(t: &str) -> bool {
    (t.contains("prompt") && t.contains("long"))
        || t.contains("overlong_prompt")
        || t.contains("too many tokens")
        || t.contains("context length")
}
```

Classification is lowercased-substring matching across providers, which is broader than cake's `parse_context_overflow` requiring both `"context limit"` and `"max_tokens"`. Worth widening cake's matcher regardless of whether compaction lands.

The one piece of tact's recovery that is a strict improvement over cake's and costs almost nothing: **continuation escalates**.

```rust
pub const CONTINUATION_MESSAGE: &str =
    "Output limit hit. Continue directly from where you stopped. \
     No recap, no repetition. Pick up mid-sentence if needed.";

pub const CONVERGENCE_CONTINUATION_MESSAGE: &str =
    "Your response has been truncated repeatedly. Stop expanding the analysis and do not \
     revisit the same scenarios. Return only the final actionable result in a concise \
     structured format: conclusion, verified issues, and minimal fixes. Do not recap, \
     repeat, or speculate.";

pub fn continuation_message(attempt: u32) -> &'static str {
    if attempt <= 1 { CONTINUATION_MESSAGE } else { CONVERGENCE_CONTINUATION_MESSAGE }
}
```

cake's `SEMANTIC_RECOVERY_PROMPT` is fired exactly once, gated by a `semantic_recovery_used: bool` in `agent_loop.rs`. A model that truncates once often truncates again for the same reason — it is trying to write too much. Retrying with the identical prompt is a coin flip; switching to a convergence prompt changes the model's target.

### Mapping onto cake

**Placement.** A new `src/clients/compact/` module. Dependency direction (`just lint-deps`) requires it not import from `cli`; the agent owns compaction, and the CLI only renders progress. Production imports use absolute `crate::` paths (`just lint-imports`).

**Type mapping.** tact operates on `Vec<Message>` with `ContentBlock::ToolResult` nested inside user messages. cake's history is a flat `Vec<ConversationItem>`:

```rust
pub enum ConversationItem {
    Message { role, content: String, id, status, timestamp },
    FunctionCall { id, call_id, name, arguments, timestamp },
    FunctionCallOutput { call_id, output, timestamp },
    Reasoning { id, summary, encrypted_content, content, timestamp },
}
```

This makes micro-compaction *simpler* in cake than in tact — no nested block indexing. Collect indices of `FunctionCallOutput` items in order, keep the last N, rewrite `output` on the rest. Mutable access already exists via `ConversationState::history_mut()`.

Two cake-specific constraints tact does not have:

- `Reasoning` items carry `encrypted_content` that must be echoed back for reasoning models, and `content` that the router uses to reconstruct `reasoning_content` for Chat Completions providers like Moonshot. **Reasoning items must never be stubbed, dropped, or reordered.** This is a correctness constraint, not a quality one.
- `with_restored_history` guarantees every `FunctionCall` has a matching `FunctionCallOutput`. Any compaction that drops items must preserve that pairing, or resumed sessions break. Stubbing an output's *text* is safe; removing the item is not.

**Session records.** `SessionRecord` is append-only and versioned (ADR-004). Compaction should add a variant rather than rewrite:

```rust
Compaction {
    session_id: String,
    task_id: String,
    timestamp: DateTime<Utc>,
    summary: String,
    first_kept_index: usize,     // or a stable item id, if one is introduced
    tokens_before: u32,
    read_files: Vec<PathBuf>,
    modified_files: Vec<PathBuf>,
}
```

Then context materialization on resume reads the log, finds the latest `Compaction`, and emits `[summary] + [items from first_kept_index onward]`. This is Pi's `firstKeptEntryId` model. It also means `--resume` and `--fork` inherit compaction for free, and `cake debug` can still see the full pre-compaction history. Note that `ConversationItem` has no stable per-item id today — introducing one, or committing to positional indices, is a decision to make before writing the record type.

**Stream-json.** Machine-readable stdout carries only its declared format. If compaction emits a progress event it needs a declared event type in the stream-json contract, or it must stay silent on stdout and go to the telemetry sidecar. Silent-plus-telemetry is the safer default.

**Telemetry.** `SessionTelemetry` already carries `RetryScheduledTelemetry` and `RetryReason`. Compaction events fit naturally: trigger reason, tokens before/after, retained item count, summary call duration and usage.

**Configuration.** cake has no `model_context_window` setting; the whole trigger depends on one. Options: add a per-model `context_window` field to `[[models]]` in settings (explicit, matches cake's existing model config style), or derive from the provider's overflow error (only works after the first failure). The former is the obvious choice; the latter is a fallback for unconfigured models. Note the interaction with `--output-schema`: a schema-constrained final response has its own token cost that should count toward `non_retained_input`.

**Porting caveats.** cake denies `unwrap`/`expect` in production and requires `#[expect(..., reason = "...")]` over `#[allow]`. tact's code uses `unwrap` freely in tests and carries bare `#[allow(dead_code)]`. `clippy::pedantic` and `nursery` are warn-level, so ported arithmetic will need `saturating_*` discipline that tact mostly already has.

### Suggested increments

Each is independently shippable and independently valuable.

1. **Widen `is_prompt_too_long` classification** and **escalate the continuation prompt.** Two constants and a match arm in existing recovery paths. No new module, no format change.
2. **Persist oversized `read` and toolbox output** the way Bash already does. cake truncates these inline with no recoverable artifact; Bash's `secure_temp_dir()` spill is the pattern, and the fail-closed behavior is already implemented there. Consider whether the artifact should move from the process temp dir to a durable per-session directory under `data_dir` so a resumed session can still read it.
3. **Micro-compaction over `FunctionCallOutput`.** Pure function, ~40 lines against cake's flat history, no LLM call, no serialized-format change. Gate it on measured context usage rather than running unconditionally, to limit prefix-cache damage.
4. **Full compaction** behind the append-only `Compaction` record. This is the ADR-worthy one — it touches the session format, the agent/CLI boundary, and resume semantics.

## Gap 2: subagents

`crates/tact/src/tool/subagent.rs`, ~215 lines including tests.

```rust
struct SubagentInput {
    prompt: String,
    description: Option<String>,   // display only
}
```

Flow:

1. Resolve client and model. If `settings.agent.subagent` is present, build a separate provider client with its own model, `max_tokens`, and `thinking_budget`; otherwise inherit the parent's.
2. System prompt is a single line: `"You are a coding subagent at {work_dir}. Complete the given task, then summarize your findings."`
3. Toolset is fixed and minimal — **5 tools**: `bash`, `read_file`, `write_file`, `edit_file`, `sleep`. No subagent spawning (no recursion), no task tools, no compaction tool.
4. Fresh permission manager from project-level settings, `PermissionMode::Default`.
5. New session row in SQLite with `ref_id` = parent session id, so child sessions are linked to the parent.
6. Run the child's `agent_loop` with the prompt as a user turn.
7. Return the **last assistant message text** from the child's context, or `"(no summary)"`.

The tool result the parent sees is that final summary — one paragraph — regardless of how many tool calls the child made. That is the entire point: a repo-wide search that would cost forty tool results in the parent's context costs one paragraph instead.

### Why this fits cake specifically

cake already has every piece:

- `Agent::new(config, initial_messages)` initializes with `default_tool_registry()`, and the builder chain (`with_tool_context`, `with_toolbox_tools`, `with_history`, `with_session_id`, `with_task_id`, `with_skill_locations`) is already the construction path used by `session_factory`'s three entry points.
- Registry subsetting already exists: `retain_read_safe_tools()` is called by `with_tool_context` under `SandboxPolicy::ReadOnly`, and `tools/mod.rs` already builds a read-only registry containing just `read`.
- `--fork` proves the session-cloning plumbing.

cake can also do something tact cannot: **pin the subagent's sandbox policy independently of the parent.** A "search and report" subagent can run under `ReadOnly` even when the parent is `WorkspaceWrite`, which under cake's existing rules drops `edit`, `write`, and all toolbox tools from the child registry automatically. That is a stronger isolation guarantee than tact's, and it comes free from ADR-014's existing enforcement.

### Decisions to make

- **Recursion.** tact forbids it by omitting the tool from the child registry. Do the same, or cap depth explicitly.
- **Session records.** Does the child get its own session file (tact's approach, with a parent ref), or do its records interleave into the parent's log with a `parent_call_id`? The latter keeps `cake --resume` seeing one coherent history; the former keeps files small. Either way `SessionMeta` needs a parent pointer.
- **Concurrency and scheduling.** `schedule_tool_calls` treats a call as mutating based on `mutating_target`. A subagent's mutations are invisible to that analysis, so a subagent call cannot be assumed non-mutating. Simplest correct answer: treat `subagent` as a scheduling barrier, or restrict subagents to read-only and let ADR-013 stay sound.
- **Interrupts and exit codes.** ADR-011 governs graceful shutdown. A SIGINT during a subagent must terminate the child and still write well-formed records for both.
- **Timeout and turn budget.** tact imposes neither; a runaway child can burn the whole budget. cake should cap child turns, and probably child token spend.
- **Hooks.** Do `PreToolUse` / `PostToolUse` hooks fire for the child's tool calls? Firing them is more consistent; not firing avoids a hook storm from a fan-out. Firing, with the child's session id in the payload, is the defensible default.

## Gap 3: background command execution

`crates/tact/src/background.rs`, ~237 lines, plus `background_run` / `check_background` tool wrappers.

```rust
enum BackgroundTaskStatus { Running, Completed, Error }

struct BackgroundTaskRecord {
    id: String,                        // 8 hex chars
    status: BackgroundTaskStatus,
    command: String,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    output: String,                    // combined stdout + stderr
}
```

- **Startup reconciliation.** On construction, every persisted record still marked `Running` is rewritten to `Error` with output `"Process interrupted (agent restarted)"`. Without this, a crashed run leaves phantom `Running` tasks forever.
- **`run(command, work_dir)`** — validates the command through the same shell validator as the foreground tool, allocates an id from an `AtomicU64` seeded with `Utc::now().timestamp_millis()`, persists a `Running` record, then `tokio::spawn`s:
  - `timeout(Duration::from_secs(120), Command::new("sh").arg("-c")…)`
  - `.kill_on_drop(true)`
  - combined stdout+stderr truncated to `MAX_OUTPUT_CHARS` = 50 000
  - status from `output.status.success()`; timeout yields `Error` with `"Error: Timeout (120s)"`
  - final record persisted
  Returns immediately: `"Background task {id} started: {command}"`.
- **`check(Option<&str>)`** — with an id, pretty JSON for that record; without, a `"{id}: {Status} {command}"` listing sorted by start time, lazily hydrated from the store.

The 120-second internal timeout is a real design flaw worth *not* copying — it is shorter than many of the builds this feature exists to run, and it is not the same knob as the foreground timeout. cake's foreground bash already exposes a timeout parameter; background should too, with a longer default.

### Why it matters for cake

cake's Bash is synchronous with a wall-clock timeout. A ten-minute test suite blocks the loop for ten minutes, and a suite that outruns the timeout produces nothing usable. Letting the model start a build, keep reading code, and poll is squarely aimed at cake's CI-oriented use case.

### Decisions to make

- **Process lifetime at exit.** This is the one cake has to answer that tact does not, because cake is a one-shot filter that exits. Options: (a) join all background tasks before the final response; (b) kill survivors at exit and record them as interrupted; (c) let them outlive the process. (a) is the only one that keeps exit codes and stream-json honest. `kill_on_drop(true)` plus an explicit join at shutdown, integrated with ADR-011's interrupt handling, is the likely shape.
- **Sandbox.** Background commands must run under the same `SandboxPolicy` as foreground Bash and through the same `bash_safety` checks. This is non-negotiable — a background path that skips the sandbox is a bypass class.
- **Session records.** Start and completion are lifecycle events; they belong in the append-only log and in stream-json (`--output-format stream-json` emits task events as they happen, and a background completion is exactly such an event).
- **Storage.** tact persists records to a file-backed collection store. cake could keep them in the session log alone and reconstruct on resume, avoiding a new store abstraction.

## Smaller items

**Session statistics** (`crates/tact/src/stats.rs`, 469 lines). Per-tool call counts split success/failure, per-tool cumulative and average duration, LLM call durations, cache hit/miss prompt tokens, reasoning tokens, thinking block count and size, compaction count. cake's telemetry sidecar (ADR-007) already records most of the underlying events — this is mostly a rollup view under `cake debug`, not new plumbing.

**Persistent memory** (`crates/tact/src/memory/mod.rs`, 377 lines). Typed markdown files under `.tact/memory/` with YAML frontmatter (`name`, `description`, `type` ∈ {user, feedback, project, reference}), plus a `MEMORY.md` index capped at 200 lines injected into the system prompt. A `save_memory` tool lets the agent write them.

cake covers most of this with `AGENTS.md` + skills. The genuinely new part is *agent-authored* facts surviving across sessions. The transferable asset is the guidance text more than the storage — it is what keeps the feature from degenerating into a junk drawer:

```
When to save:
- User states a preference -> type: user
- User corrects you -> type: feedback
- A project fact not inferable from current code alone (compliance rule, legacy
  constraint that must stay untouched for business reasons) -> type: project
- Where an external resource lives (ticket board, dashboard, docs URL) -> type: reference

When NOT to save:
- Anything derivable from code (signatures, file structure, directory layout)
- Temporary task state (current branch, open PR numbers, current TODOs)
- Secrets or credentials
```

Low priority. It also overlaps with `.ahm` for this repo specifically.

**`apply_patch` tool.** tact offers unified-diff application alongside `edit_file`, with a dry-run mode. cake's `edit` is exact-string replacement only. Multi-hunk edits currently cost one call per hunk. Worth considering, but it interacts with ADR-013's `mutating_target` analysis (a patch can touch several files in one call, so it cannot be reduced to a single canonical path — it would need to join every group it touches, or be treated as a barrier).

## Not worth porting

  | Feature                                     | Reason                                                                                                                                                                            |
  | ------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
  | TUI, themes, voice input, image attachments | Assume an interactive human; contradict cake's Unix-filter premise                                                                                                                |
  | `ask_user`, permission modes, plan mode     | Built on interactive approval dialogs. cake's equivalent guarantee is the sandbox, which is stronger headless                                                                      |
  | Teams (`spawn_teammate`, inbox, broadcast)  | Message-passing between long-lived agents; no headless story, and subagents cover the useful part                                                                                  |
  | Cron tools                                  | cake is a binary you invoke; the system already has cron                                                                                                                          |
  | Plugin marketplace                          | tact's loads only `skills/*/SKILL.md` from installed plugins — cake's existing skills system already does that without the marketplace, checkout, and revision-locking machinery |
  | Persistent task tools                       | `task_create` / `task_get` / `task_list` / `task_update` with dependency tracking. For this repo, `.ahm` already occupies the slot                                                 |
  | MCP client                                  | See below                                                                                                                                                                         |
  | Native Anthropic backend                    | See below                                                                                                                                                                         |

**MCP.** The honest framing is ecosystem compatibility, not capability. cake's `tb__*` toolbox already solves "add a tool," with a simpler trust story documented in `docs/security.md` and ADR-017. Adding MCP means a second trusted-extension boundary to specify and defend, plus `rmcp` and a child-process transport in a binary whose size you audit (`docs/runbooks/auditing-binary-size.md`). Worth doing only if consuming existing MCP servers is itself the goal.

**Native Anthropic backend.** tact has one; cake reaches Claude via OpenRouter. Adding it would put a non-OpenAI wire format inside a codebase whose stated invariant is one internal conversation representation with OpenAI-shaped backends at the edges. The narrower transferable idea — `body_hook_for` in `crates/tact_llm/src/hook_select.rs`, which selects provider-specific body mutations by both declared provider *and* base-URL/model heuristics — is something `src/clients/provider_strategy.rs` already does for OpenRouter headers and the Kimi `reasoning_content` fallback.

## If only one thing

Increments 1–3 of the context work: widen overflow classification, escalate the continuation prompt, persist oversized `read`/toolbox output, and micro-compact `FunctionCallOutput` items. Together they are a few hundred lines, require no LLM call, change no serialized format, need no new ADR, and directly extend how long a cake run survives before hitting the wall.

Full compaction and subagents are the follow-ups. Both deserve ADRs — compaction touches the session format and resume semantics; subagents touch the agent/tool boundary, per-path scheduling, and the sandbox model.
