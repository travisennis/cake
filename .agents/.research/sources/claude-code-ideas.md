# Claude Code Ideas

A collection of insights and findings from analyzing Claude Code's source code and behavior.

---

## How Claude Code Handles Memory: The 8 Phases

_Contributed by the team at [@mem0ai](https://x.com/mem0ai)_

User Input -> Context Assembly -> History System -> API / Query -> Response -> Summary

### Phase 1: Session Initialization

Before the first render, three things happen in sequence:
- Hooks are registered
- The memory cache is warmed
- Async directory walks are kicked off in the background

This ensures the system is primed and ready before any user interaction occurs.

### Phase 2: Memory Discovery

Memory is discovered in a strict priority order:
1. Managed enterprise policy
2. User global
3. Project VCS
4. Local per-directory auto-generated
5. Team shared

### Phase 3: Context Assembly Pipeline

Three parallel pipelines merge into every API call:
- System prompt
- Memory section
- User context

**Relevance prefetch**: A separate Sonnet side-call selects up to 5 memory files for relevance-based retrieval.

### Phase 4: Direct Memory Access

The model can directly read and write memory files using:
- `FileReadTool`
- `FileWriteTool`
- `FileEditTool`

Background extractor and model writes are mutually exclusive to prevent race conditions.

### Phase 5: Post-Response Processing

After every response, three background agents fire simultaneously:
- `extractMemories` — a forked agent running in parallel, capped at 200 lines / 25kb
- `sessionMemory`
- `autoDream`

### Phase 6: Context Compaction

When context fills up, old messages are summarized using a "skipped summarizer":
- Minimum 10k tokens preserved
- Minimum 5 text-block messages preserved

This ensures critical context is retained while making room for new interactions.

### Phase 7: Memory Storage Locations

Memory is persisted across multiple locations:
- `~/.claude/`
- Project root
- `sessions/`
- `agent-memory/`

**Policies**:
- Auto memory is git-ignored
- Team memory is VCS-tracked

### Phase 8: Self-Improving Loop

Memory improves across sessions through multiple consolidation mechanisms:
- Within-turn writes
- End-of-turn extracts
- Session memory
- Auto-dream consolidations

Full consolidation occurs every 24+ hours.

### Memory Touchpoints

Memory is touched at these key moments:
- Launch
- Query
- Response
- Background agents
- Shutdown
- Next session

---

## The Technical Recipe Behind Claude Code's Memory Architecture

_Contributed by [@claude code](https://x.com/claudeai)_

Claude Code's memory system is not a "store everything" approach. It uses constrained, structured, and self-healing memory that does several non-obvious things well.

### Core Principles

#### Memory = Index, Not Storage

- `MEMORY.md` is always loaded, but it contains only pointers (~150 chars per line)
- Actual knowledge lives outside, fetched only when needed

#### 3-Layer Design (Bandwidth Aware)

| Layer | Behavior |
|-------|----------|
| Index | Always loaded |
| Topic files | On-demand |
| Transcripts | Never read directly, only searched via grep |

#### Strict Write Discipline

- Write to file, then update index
- Never dump content into the index directly
- This prevents entropy and context pollution

#### Background Memory Rewriting (autoDream)

The autoDream agent continuously:
- Merges and deduplicates entries
- Removes contradictions
- Converts vague statements to absolute facts
- Aggressively prunes stale content

**Key insight**: Memory is continuously edited, not appended.

#### Staleness is First-Class

- If memory conflicts with reality, the memory is wrong
- Code-derived facts are never stored (they can be re-derived)
- Index is forcibly truncated when stale

#### Isolation Matters

- Consolidation runs in a forked subagent
- Limited tool access prevents corruption of main context

#### Retrieval is Skeptical, Not Blind

- Memory is treated as a hint, not truth
- Model must verify before using stored information

### What They Don't Store: The Real Insight

Notable exclusions from Claude Code's memory:
- No debugging logs
- No code structure descriptions
- No PR history

**Rule**: If something is derivable, don't persist it.

---

## Reading Leaked Claude Code Source Code

_Source: [lr0.org](https://lr0.org/blog/p/claude-code-source/) by larrasket, March 2026_

Anthropic accidentally leaked a source map file containing ~132,000 LOC of TypeScript. Key findings from reading the source.

### The Codename Canary Problem

In `src/buddy/types.ts`, all 18 species names are hex-encoded:

```typescript
const c = String.fromCharCode
export const duck = c(0x64,0x75,0x63,0x6b) as 'duck'
export const goose = c(0x67,0x6f,0x6f,0x73,0x65) as 'goose'
```

One species name collides with a model codename in `excluded-strings.txt`. The build check greps the output to ensure no codenames leak publicly. Rather than rename the species, all are encoded uniformly so the literal never appears in the bundle.

### Internal Commands (Hidden in External Builds)

Commands that exist only in Anthropic's internal build:
- `bughunter`
- `commitPushPr`
- `ctx_viz`
- `goodClaude` (disabled and hidden)
- `ultraplan`
- `ultrareview`
- `teleport`
- `ant-trace`

### Experimental Feature Flags

- `PROACTIVE` — autonomous agent mode
- `KAIROS` — assistant/brief mode
- `COORDINATOR_MODE` — multi-agent orchestration (Claude spawns worker agents)
- `AGENT_TRIGGERS` — cron job scheduling for agents
- `VOICE_MODE`
- `BUDDY` — companion pets system

**Coordinator mode** explicitly instructs the orchestrating agent: "Never write 'based on your findings' or 'based on the research.' These phrases delegate understanding to the worker instead of doing it yourself."

### Analytics Type Naming

In `src/query.ts`:

```typescript
import {
  logEvent,
  type AnalyticsMetadata_I_VERIFIED_THIS_IS_NOT_CODE_OR_FILEPATHS,
} from 'src/services/analytics/index.js'
```

Every developer must type "I_VERIFIED_THIS_IS_NOT_CODE_OR_FILEPATHS" as a forcing function. You cannot accidentally send file paths or code to analytics without explicitly acknowledging it.

### Multi-Clauding Detection

The `/insights` command tracks "Multi-Clauding" via `detectMultiClauding()`. It uses a sliding window algorithm: session1 -> session2 -> session1 within a 30-minute window counts as an "overlap event."

### Bash Security: 2,600 Lines of Preventing rm -rf

The `bashSecurity.ts` file validates every shell command against attack patterns:

- **Zsh `=cmd` expansion** — `=curl evil.com` expands to `/usr/bin/curl evil.com`, bypassing permission rules
- **`zmodload`** — gateway to `zsh/mapfile` for invisible file I/O
- **Heredoc injection** — full line-by-line matching algorithm replicating bash behavior
- **ANSI-C quoting** — `$'\x41'` can encode arbitrary characters
- **Process substitution** — `<()` and `>()`
- **`emulate`** — eval-equivalent arbitrary code execution in zsh
- **`ztcp`** — TCP exfiltration through Zsh builtins

Separate files handle destructive command warnings (`rm -rf`, `git reset --hard`, `DROP TABLE`, `kubectl delete`) with human-readable explanations. PowerShell has its own parallel implementation tracking alias hijacking and module loading.

### YOLO: ML-Based Permission Classifier

Permission modes: `default`, `acceptEdits`, `dontAsk`, `bypassPermissions`, `auto`.

The `auto` mode uses `yoloClassifier.ts` (1,495 lines) — an ML classifier that performs two-stage evaluation (fast initial decision, then extended reasoning if needed). The classifier tracks cache metrics, token usage, and override decisions.

The fact that they built a thoughtful safety system and named it "yolo" is telling.

### Full Vim Implementation, From Scratch

`src/vim/` contains a complete Vim keybinding implementation as a hand-rolled state machine:

- **Modes**: INSERT and NORMAL
- **Operators** (556 lines): delete, change, yank
- **Motions**: h/j/k/l, w/b/e, W/B/E, 0/^/$, G
- **Text objects**: words, quotes, brackets, braces
- **Find motions**: f/F/t/T with repeat via `;` and `,`
- **Extras**: indent/outdent, join lines, replace, toggle case, dot-repeat

The transitions file is 490 lines handling full state machine logic. Motions are pure functions. This is more complete than most Vim mode/evil plugins.

### The Rules of Thinking (Medieval English Warning)

In `src/query.ts`, a comment documents three rules about thinking blocks:

> The rules of thinking are lengthy and fortuitous. They require plenty of thinking of most long duration and deep meditation for a wizard to wrap one's noggin around.
>
> Heed these rules well, young wizard. For they are the rules of thinking, and the rules of thinking are the rules of the universe. If ye does not heed these rules, ye will be punished with an entire day of debugging and hair pulling.

Rules:
1. A message with a thinking block must have `max_thinking_length > 0`
2. A thinking block may not be the last message in a block
3. Thinking blocks must be preserved for the duration of an assistant trajectory

The line directly below this comment is `const MAX_OUTPUT_TOKENS_RECOVERY_LIMIT = 3` — irony noted.

### Buddy: The Companion Pet System

`src/buddy/` implements companion pets with:

**Species** (18 total, hex-encoded): duck, goose, blob, cat, dragon, octopus, owl, penguin, turtle, snail, ghost, axolotl, capybara, cactus, robot, rabbit, mushroom, chonk

**Rarity tiers**: common 60%, uncommon 25%, rare 10%, epic 4%, legendary 1%

**Stats**: DEBUGGING, PATIENCE, CHAOS, WISDOM, SNARK — one peak stat, one dump stat (D&D-style character generation). Legendary has a floor of 50 in all stats.

**Hats**: crown, tophat, propeller, halo, wizard hat, beanie, tinyduck — only uncommon+ rarity can roll hats, and "none" is in the array too.

**Architecture**:
- **Bones** (species, rarity, stats, eyes, hat, shininess) — deterministic from user ID hash, cannot be faked
- **Soul** (name, personality) — generated once by the model and stored

**Shiny companion**: 1% chance, like shiny Pokemon.

**Sprites**: 5x12 ASCII art grids with 3 animation frames. The cat goes:

```
   /\_/\
  ( ·   · )
  (  ω  )
  (")_(")
```

Dragons breathe little tildes in frame 3. Octopuses alternate tentacle positions. The companion sits beside the input box and occasionally comments in a speech bubble. The system prompt tells Claude to "stay out of the way: respond in ONE line or less" when addressed.

The whole system is behind the `BUDDY` feature flag.

### Other Notable Details

**Loading spinner verbs** (186 total): includes "Clauding", "Flibbertigibbeting", "Boondoggling", "Prestidigitating", "Photosynthesizing", "Shenaniganing", "Whatchamacallititing", "Tomfoolering". Past tense completions include "Sautéed" and "Baked".

**Stickers command**: opens browser to `stickermule.com/claudecode`. That's the entire implementation.

**Team memory sync conflict resolution**: described as "the lesser evil" — local edits overwrite server version, but silently discarding local work is worse.

**Keyboard parser**: handles both Kitty and XTerm extended key protocols, with special code paths for SSH tunnels where `TERM_PROGRAM` isn't forwarded.

---

## The Claude Code Source Leak: Fake Tools, Frustration Regexes, Undercover Mode

_Source: [Alex Kim's blog](https://alex000kim.com/posts/2026-03-31-claude-code-source-leak/) by Alex Kim, March 2026_

Anthropic accidentally shipped a `.map` file in their npm package, exposing ~132,000 LOC. Key findings from reading the leaked source.

### Anti-Distillation: Injecting Fake Tools to Poison Copycats

In `claude.ts`, a flag called `ANTI_DISTILLATION_CC` sends `anti_distillation: ['fake_tools']` in API requests when enabled. This tells the server to silently inject decoy tool definitions into the system prompt.

**Purpose**: If someone records Claude Code's API traffic to train a competing model, the fake tools pollute that training data.

**Gating**: Requires all four conditions — the `ANTI_DISTILLATION_CC` compile-time flag, the `cli` entrypoint, a first-party API provider, and the `tengu_anti_distill_fake_tool_injection` GrowthBook flag.

**Second mechanism**: Server-side connector-text summarization buffers assistant text between tool calls, summarizes it, and returns it with a cryptographic signature. Subsequent turns can restore original text from the signature.

**Workarounds**: A MITM proxy stripping the `anti_distillation` field bypasses it. Setting `CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS` disables the whole thing. Third-party API providers or SDK entrypoints never trigger the check. The real protection is probably legal, not technical.

### Undercover Mode: AI That Hides Its AI

`undercover.ts` (90 lines) strips all traces of Anthropic internals when Claude Code is used in non-internal repos. It instructs the model to never mention internal codenames like "Capybara" or "Tengu," internal Slack channels, repo names, or the phrase "Claude Code" itself.

**Line 15**: "There is NO force-OFF. This guards against model codename leaks."

You can force it ON with `CLAUDE_CODE_UNDERCOVER=1`, but there is no way to force it off. In external builds, the entire function gets dead-code-eliminated to trivial returns. This is a one-way door.

**Implication**: AI-authored commits and PRs from Anthropic employees in open source projects will have no indication that an AI wrote them.

### Frustration Detection via Regex

In `userPromptKeywords.ts`:

```typescript
/\b(wtf|wth|ffs|omfg|shit(ty|tiest)?|dumbass|horrible|awful|
piss(ed|ing)? off|piece of (shit|crap|junk)|what the (fuck|hell)|
fucking? (broken|useless|terrible|awful|horrible)|fuck you|
screw (this|you)|so frustrating|this sucks|damn it)\b/
```

An LLM company using regexes for sentiment analysis is peak irony, but also: a regex is faster and cheaper than an LLM inference call just to check if someone is swearing at your tool.

### Native Client Attestation: DRM for API Calls

In `system.ts`, API requests include a `cch=00000` placeholder. Before the request leaves the process, Bun's native HTTP stack (written in Zig) overwrites those five zeros with a computed hash. The server validates the hash to confirm the request came from a real Claude Code binary, not a spoofed one.

**Technical detail**: They use a placeholder of the same length so replacement doesn't change the Content-Length header or require buffer reallocation. The computation happens below the JavaScript runtime, invisible to anything in the JS layer.

**Context**: This is the technical enforcement behind the OpenCode legal fight. Anthropic doesn't just ask third-party tools not to use their APIs; the binary cryptographically proves it is the real Claude Code client.

**Bypasses**: The `cch=00000` placeholder only gets injected when the `NATIVE_CLIENT_ATTESTATION` compile-time flag is on. The header can be disabled with `CLAUDE_CODE_ATTRIBUTION_HEADER` or a GrowthBook killswitch. The Zig-level hash replacement only works inside the official Bun binary. Running on stock Bun or Node, the placeholder survives as five literal zeros.

### 250,000 Wasted API Calls Per Day

A comment in `autoCompact.ts`:

> BQ 2026-03-10: 1,279 sessions had 50+ consecutive failures (up to 3,272) in a single session, wasting ~250K API calls/day globally.

**The fix**: `MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES = 3`. After 3 consecutive failures, compaction is disabled for the rest of the session.

### KAIROS: Unreleased Autonomous Agent Mode

References throughout the codebase describe `KAIROS` as an unreleased autonomous agent mode that includes:
- A `/dream` skill for nightly memory distillation
- Daily append-only logs
- GitHub webhook subscriptions
- Background daemon workers
- Cron-scheduled refresh every 5 minutes

The scaffolding for an always-on, background-running agent is present but heavily gated.

### Terminal Rendering Optimization

The terminal rendering in `ink/screen.ts` and `ink/optimizer.ts` borrows game-engine techniques:
- `Int32Array`-backed ASCII char pool
- Bitmask-encoded style metadata
- Patch optimizer that merges cursor moves and cancels hide/show pairs
- Self-evicting line-width cache

The source claims "~50x reduction in stringWidth calls during token streaming."

### Prompt Cache Economics

`promptCacheBreakDetection.ts` tracks 14 cache-break vectors with "sticky latches" that prevent mode toggles from busting the cache. One function is annotated `DANGEROUS_uncachedSystemPromptSection()`. When paying for every token, cache invalidation stops being a computer science joke and becomes an accounting problem.

### Multi-Agent Coordinator

The orchestration algorithm in `coordinatorMode.ts` is a prompt, not code. It manages worker agents through instructions like:
- "Do not rubber-stamp weak work"
- "You must understand findings before directing follow-up work. Never hand off understanding to another worker."

### Codebase Rough Spots

- `print.ts` is 5,594 lines with a single function spanning 3,167 lines and 12 levels of nesting
- Uses Axios for HTTP (which was compromised on npm during the same week)

### On the Leak Itself

Anthropic acquired Bun at the end of last year, and Claude Code is built on top of it. A Bun bug filed March 11 reports that source maps are served in production mode even though Bun's docs say they should be disabled. The issue was still open.

As one Twitter reply put it: "accidentally shipping your source map to npm is the kind of mistake that sounds impossible until you remember that a significant portion of the codebase was probably written by the AI you are shipping."

---

## Feature Idea: Background/Headless Agent Mode

_Sourced from HN thread analysis on Claude Code source leak, April 2026_

### The Problem with Claude Code

Claude Code currently requires a desktop or laptop to remain turned on. This is a significant limitation:

> "Currently the Anthropic implementation forces a desktop (or worse, a laptop) to be turned on instead of working headless as far as I understand it." — HN user

### The KAIROS Scaffold

Claude Code's unreleased `KAIROS` mode includes scaffolding for an always-on, background-running agent:

- **A `/dream` skill** for nightly memory distillation
- **Daily append-only logs**
- **GitHub webhook subscriptions**
- **Background daemon workers**
- **Cron-scheduled refresh every 5 minutes**

The infrastructure exists but is heavily gated behind feature flags.

### Third-Party Implementation: Clappie.ai

An open-source project (clappie.ai) demonstrates this pattern:

- **Telegram Integration** -> connects to a running Claude Code session
- **Crons** -> scheduled task execution
- **Animated ASCII companion** -> similar to Claude Code's unreleased Buddy system

The creator noted: "Tmux and claude and telegram is a really powerful combo!"

### Opportunity for cake

cake could implement:
- Daemon mode that runs in the background
- Multiple interface options (CLI, Telegram, web, etc.)
- Scheduled task execution (cron-style)
- Webhook integration for event-driven workflows
- Persistent connection that survives terminal closes

---

## Feature Idea: Session Persistence & Multi-Session Awareness

_Sourced from Claude Code source analysis and HN thread discussion, April 2026_

### Multi-Clauding Detection

Claude Code tracks "Multi-Clauding" — when users have multiple agent sessions running simultaneously.

**Implementation** (`detectMultiClauding()`):
- Uses a sliding window algorithm
- Tracks: session1 -> session2 -> session1 patterns
- A 30-minute window defines an "overlap event"
- Exposed via the `/insights` command

**Purpose**: Understanding user workflow patterns and potential context fragmentation.

### Session Persistence in Claude Code

From the documented memory architecture:

- **Session initialization**: Hooks registration, memory cache warming, async directory walks
- **Memory touchpoints**: Launch, Query, Response, Background agents, Shutdown, Next session
- **Cross-session continuity**: Memory improves through consolidation across sessions
- **Full consolidation** occurs every 24+ hours

### Opportunity for cake

cake could implement:

**Session Awareness**:
- Detect when users are running multiple agents on the same project
- Warn about potential context fragmentation
- Provide a dashboard showing active sessions and their focus areas

**Cross-Session Memory**:
- Remember what you were working on across sessions
- Detect when returning to an abandoned task
- Surface relevant context from previous sessions on startup

**Background Continuity**:
- Track ongoing tasks between sessions
- Notify when background work completes
- Resume interrupted work with full context

---

## Deep Dive: Compaction Architecture

_Sourced from HN thread discussion on Claude Code source leak, April 2026_

### JSONL Preservation After Compaction

One of the most interesting aspects of Claude Code's compaction is that the full pre-compaction conversation is preserved in the session JSONL file. Messages are filtered before being sent to the API, but the original data is never deleted.

**Key mechanisms**:

1. **JSONL is append-only** — old pre-compaction messages are never deleted. New messages (boundary marker, summary, attachments) are appended after compaction.

2. **Message flags control API visibility**:
   - `isCompactSummary: true` — marks the AI-generated summary message
   - `isVisibleInTranscriptOnly: true` — prevents a message from being sent to the API
   - `isMeta` — another filter for non-API messages
   - `getMessagesAfterCompactBoundary()` — returns only post-compaction messages for API calls

3. **After compaction, the API sees only**:
   - The compact boundary marker
   - The summary message
   - Attachments (file refs, plan, skills)
   - Any new messages after compaction

**Recovery**: Even after compaction, if you think something was lost, you can tell Claude Code to "look in the session log files to find details about what we did with XYZ".

**Opportunity for cake**: Implement recoverable compaction where full history is preserved but filtered from API calls. This provides a safety net for context recovery without sacrificing cost savings.

### Three Compaction Types

Claude Code implements three distinct compaction strategies:

| Type | Description | Cost |
|------|-------------|------|
| **Full compaction** | API summarizes all old messages | Expensive |
| **Session memory compaction** | Uses extracted session memory as summary | Cheaper |
| **Microcompaction** | Clears old tool result content when cache is cold (>1h idle) | Moderate |

### Microcompaction Economics

This is where Claude Code's engineering gets interesting from a cost perspective:

1. **Anthropic's API has a server-side prompt cache with a 1-hour TTL**
2. When actively using a session, each API call reuses the cached prefix — you only pay for new tokens
3. After 1 hour idle, that cache is guaranteed expired
4. Your next message re-sends and re-processes the entire conversation from scratch — every token, full price

So if you have 150K tokens of old Grep/Read/Bash outputs sitting in the conversation, you're paying to re-ingest all of that even though it's stale context the model probably doesn't need.

**Microcompaction says**: "Since we're paying full price anyway, let's shrink the bill by clearing the bulky stuff."

**What's preserved vs lost**:
- The tool_use blocks (what tool was called, with what arguments) — kept
- The tool_result content (the actual output) — replaced with `[Old tool result content cleared]`
- The most recent 5 tool results — kept

So Claude can still see "I ran Grep for foo in src/" but not the 500-line grep output from 2 hours ago.

**Does it affect quality?** Yes, somewhat. But without it, you're paying potentially tens of thousands of tokens to re-ingest stale tool outputs that the model already acted on. And if the conversation is long enough, full compaction would have summarized those messages anyway.

**Critical detail**: This is disabled by default (`enabled: false` in `timeBasedMCConfig.ts:31`). It's behind a GrowthBook feature flag that Anthropic controls server-side.

**Opportunity for cake**: Implement time-based compaction that activates after context cache expires. When the user returns after idle, automatically clear or summarize old tool outputs to reduce re-ingestion costs.

### Context Cache and Token Economics

The HN discussion highlights an important insight about how LLM APIs work:

> "Remember that every time you send a new message to the LLM, you are actually sending the _entire conversation_ again with that added last message to the LLM."

> "Remember that LLMs are fixed functions, the only variable is the context input (and temperature, sure). Naively, this would lead to quadratic consumption of your token quota, which would get ridiculously expensive as conversations stretch into current 100k-1M context windows."

To solve this, AI providers cache the context on the GPU and only charge for the delta. But they won't keep that GPU cache warm forever, so it times out after inactivity.

This means **idle time has real cost implications**. When you step away for lunch, your context cache has been flushed, and you pay full price to restart.

**Opportunity for cake**: Track context cache health and warn users about cost implications of long idle times. Consider proactive microcompaction before returning to a session after extended idle.

---

_If you have insights to add, submit a PR with your findings._
