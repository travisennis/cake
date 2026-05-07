Here's a thorough technical summary of Aparna Dhinakaran's article on **Context Management in Agent Harnesses** (posted April 26, 2026 -- 737 likes, 94 retweets, 1,928 bookmarks, 137K views).

---

## Core Thesis

The context window is a fixed-size working set that must feel infinite. The real design question is no longer *what* goes into the prompt, but *how the harness actively manages context over time* -- keeping high-value state close, paging data on demand, building indexes (grep), and truncating content while hinting at what else can be accessed. The key philosophical divide: how much of that management happens inside the harness vs. how much the model is trusted to do for itself.

---

## File Read Strategies (Four Harnesses Compared)

All four support **offset/limit pagination**. The differences are in the defense layers:

| Harness | Pre-read Gate | Post-read Cap | Default Lines | Continuation Nudge | Distinctive |
|---------|--------------|---------------|---------------|-------------------|-------------|
| **Pi** | None | 2,000 lines / 50KB | 2,000 | Explicit: `[Showing lines 1-2000 of 50000. Use offset=2001 to continue.]` | Harness-first: protects first, teaches pagination second |
| **OpenClaw** | None | Pi's 2K/50KB + bootstrap caps (12K chars/file, 60K total) | 2,000 | Same as Pi | Defense in depth: Pi as layer 1, bootstrap caps as layer 2, tool result budgets (16K chars or 30% context window) as layer 3; head+tail split for important tails |
| **Claude Code** | 256KB byte cap via `stat` (reject before open) | 25,000 token budget post-read | 2,000 | Actionable error directing to offset/limit or grep | Two-layer gate; both limits remotely tunable via GrowthBook feature flags; file dedup (same file + range + mtime → stub returned); lines > 2K chars truncated; rich multi-paragraph tool prompt |
| **Letta** | 10MB via `stat` | 2,000 lines, lines capped at 2K chars | 2,000 | Continuation nudge + overflow file path appended | Stat-before-read matching Claude Code; per-tool caps (30K bash/subagent, 10K grep); middle truncation default; **MemFS**: git-backed persistent memory filesystem |

Letta's MemFS deserves special mention: agent memory lives as markdown files in a git repo. Files in `system/` are pinned to the system prompt (always in context). Others are visible by name/description in a tree listing but lazy-loaded. The agent manages its own progressive disclosure by moving files in/out of `system/`, reorganizing hierarchy, and updating descriptions. Memory edits auto-commit and sync to a git remote.

---

## Session Pruning / Compaction

This is where the article says "the real engineering is." Compaction policy determines whether long-running agents stay coherent or slowly degrade.

### Pi (pi-mono)
- **Trigger**: Estimated context tokens > `contextWindow - reserveTokens` (default reserve: 16,384)
- **Mechanism**: Walks backward keeping ~20K recent tokens; everything older → LLM summarization → synthetic user message prepended to kept tail
- **Safety**: Never cuts orphaned tool results; walks boundaries to keep tool-call/tool-result pairs intact

### OpenClaw
- **Trigger**: History > 50% of context window (`maxHistoryShare`, default 0.5)
- **Mechanism**: History split into equal-mass token chunks; oldest chunk dropped, rest kept; dropped content → staged multi-pass LLM summarization with merge step
- **Safety**: `repairToolUseResultPairing` fixes orphaned results; `splitMessagesByTokenShare` avoids cutting inside pairs
- **Pre-compaction flush**: Silent agentic turn lets agent persist state to memory files *before* history disappears
- **Second layer**: Non-destructive in-memory pruning of tool results (soft-trim → hard-clear) on a 5-minute cache TTL -- reclaims context for current request without touching persistent conversation

### Claude Code
- **Trigger**: Tokens > context window - 13,000 buffer (fires ~167K for 200K-context model)
- **Mechanism**: 9-section structured compaction prompt (primary request, key technical concepts, files and code, errors and fixes, problem solving, all user messages, pending tasks, current work, optional next step)
- **Post-compact restoration**: Up to 5 recently-read files re-attached to context within token budget
- **Summarizer safety**: Model produces analysis scratchpad + final summary in separate tagged XML blocks; scratchpad stripped before summary enters context (improves quality without bloat)
- **Fallback**: If compaction call itself hits context limit → deterministic head-drop removes oldest API-round groups (20% or enough to close token gap)
- **Pre-query optimization** (every API call, regardless of pressure): Oversized tool results persisted to disk → replaced with 2KB previews. Per-tool cap: 50K chars. Per-message aggregate cap: 200K chars. A 60KB grep result gets offloaded on the very first turn.

### Letta
- **Trigger**: Server-side (Letta API), streamed to client; client uses 4-bytes-per-token heuristic for local estimates
- **Mechanism**: Server runs LLM summarization using `letta/auto` model → streams summary message with condensed text + stats (tokens before/after, message counts)
- **Reflection subagents**: Triggered on compaction event (default) or step-count threshold (25 user messages if enabled). Gets transcript of recent conversation + snapshot of parent's memory → edits git-backed memory in a worktree → triggers system prompt recompile so parent picks up new memories. Budget-capped at 16K tokens.
- **Knowledge persistence**: Important state migrates from ephemeral conversation into durable memory files. "Information that would be lost in other harnesses gets persisted to files the agent can always access."

---

## Sub-Agent Context Management

All four isolate sub-agent sessions from the parent (none copy full parent history by default). Differences:

| Harness | Default Behavior | Fork Support | Workspace Inheritance | Distinctive |
|---------|-----------------|--------------|----------------------|-------------|
| **Pi** | New process, task string only | No | None | Simplest isolation |
| **OpenClaw** | Fresh isolated session | Yes (same-agent only) | Minimal allowlist (AGENTS.md, TOOLS.md, SOUL.md) | Filtered workspace context |
| **Claude Code** | Typed-agent: blank conversation, task only | Yes (full parent history for prompt cache sharing) | Skills eagerly preloaded as user messages; async agents get explicit tool allowlist | Two paths; synthetic assistant message + placeholder tool results in fork mode; skill preloading on spawn |
| **Letta** | Non-fork: fresh headless, task only | Yes (server-side copy via API fork endpoint) | Parent allow/deny propagated; skills as tagged blocks; existing API agents by ID | Seven built-in subagent types; richest taxonomy; agents-as-subagents with their own persistent memories |

---

## Convergent Design Patterns

The four harnesses independently arrived at the same solutions:

1. **Hard-cap file reads** with offset/limit pagination
2. **Cap tool result sizes** (persist oversized results to disk)
3. **Isolate sub-agent sessions** from parent
4. **LLM-powered compaction** triggered by token threshold
5. **Estimate context usage and detect pressure**
6. **Tool-call/result boundary safety** during compaction (Pi, OpenClaw, Claude Code)
7. **Head-truncation + continuation nudge** (Pi, OpenClaw)
8. **Forking parent transcripts into sub-agents** (three of four)

Arize's own **Alyx** assistant (data exploration, not coding) independently converged on the same playbook: token-budgeted tool results, binary search for dataset slicing, tool-call dedup, JSON payload splitting (LLM-visible preview + server-side full copy via jq), head+tail truncation, char/4 token estimation, forced checkpoints at 50K tokens with model-written state summaries, and sub-agent isolation.

---

## Framing

The article closes with an OS analogy: the best memory management is invisible -- registers, cache lines, page tables, swap, each managed by the system and invisible to the layer above. Agent harnesses are moving toward the same goal: not showing the model everything, but giving it the right working set at the right time and letting it dynamically decide how to manage its own context.
