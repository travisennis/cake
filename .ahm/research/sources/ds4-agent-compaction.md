# ds4-agent Compaction Approach

Status: synthesized
Created: 2026-05-24
Updated: 2026-05-24
Related tasks: -
Related plans: -
Confidence: medium

## Summary

`ds4_agent.c` solves unbounded LLM context growth via **model-in-the-loop compaction**: instead of dropping the oldest history, the model itself is asked to write a durable task-state summary, which then replaces the old turns while the most recent ~10% of the transcript is preserved verbatim. Compaction is a first-class state in the worker state machine, runs synchronously in the worker thread, and is gated by both a soft percentage threshold and a minimum free-token threshold.

## Notes / Evidence

### Trigger Conditions (When)

Defined by these constants:

| Constant | Value | Meaning |
|---|---|---|
| `AGENT_COMPACT_SOFT_PERCENT` | 85 | Compact when 85% of context is used |
| `AGENT_COMPACT_MIN_FREE_TOKENS` | 8192 | Compact when fewer than 8K free tokens remain |
| `AGENT_COMPACT_TAIL_DIVISOR` | 10 | Preserve the most recent 1/10th of context verbatim |
| `AGENT_COMPACT_TAIL_CAP_TOKENS` | 50,000 | Upper limit on the preserved verbatim tail |
| `AGENT_COMPACT_SUMMARY_MAX_TOKENS` | 4096 | Upper limit on the model-generated summary |

`agent_worker_should_compact()` checks:
1. Has the transcript reached **85%** of the configured context size?
2. Are there fewer than **8,192 free tokens** remaining? (Capped at 25% of context for small contexts so tests don't hit infinity.)

Compaction fires at **three points** in the main worker loop `worker_run_turn()`:
- Before a user turn — *"soft limit before user turn"*
- Before continuing after a tool call — *"soft limit before tool continuation"*
- When a tool result would overflow the context — *"tool result would exceed context"* (forced: must succeed, or the tool result is replaced with an error)

Also user-triggerable via `/compact`.

### Protocol (How)

`agent_worker_compact()` performs a model-in-the-loop summary:

**Step 1 — Prepare prompt.** A private system message is appended to a copy of the current transcript, instructing the model to write a durable task-state summary preserving goals, constraints, files edited, commands run, decisions, bugs, and next steps. The prompt explicitly bans inventing facts, thinking tags, and DSML markup. An assistant prefix (no thinking) follows.

**Step 2 — Generate summary inline with streaming.** Up to `AGENT_COMPACT_SUMMARY_MAX_TOKENS` (4096) tokens generated synchronously in the worker thread, streamed dimmed to the UI under a `COMPACTING` header. Early-stop on:
- `ds4_token_eos` → normal completion
- `</think>` or `｜DSML｜` tokens → model tried to think or call tools (truncate)
- Interrupt (Ctrl+C) → abort

**Step 3 — Rebuild transcript.** New transcript is:

```
[system prompt tokens]
[compaction summary — injected as a "system" message with sentinel markers]
[recent verbatim tail — last ~10% of original conversation]
```

`agent_compact_tail_start()` computes a budget of `ctx_size / 10` capped at 50K tokens, walks backward from conversation end, and aligns to the nearest `<｜User｜>` token boundary so the rebuilt context begins at a natural turn.

Sentinel markers wrap the summary:
- Opening: `[ds4-agent compacted earlier conversation. Durable task-state summary follows.]`
- Closing: `[End compacted summary. Recent conversation continues verbatim below.]`

`agent_history_render_compaction_summary` uses these markers to render the summary in bright magenta (`\x1b[1;95m`) with a "Compacted Summary:" label.

**Step 4 — Re-sync DS4 session.** The live KV cache is invalidated and rebuilt from scratch via `agent_worker_sync_tokens()`. On failure (interrupt, empty summary, eval failure), the session is invalidated so the next turn cannot continue from the private compaction prompt. The old transcript is kept as a backup on error.

**Step 5 — Post-compaction cleanup.**
- Reset the system-prompt reminder counter
- If bash jobs were running, inject a tool message: *"Bash job update after context compaction. Running jobs still need explicit bash_status or bash_stop if relevant."*
- Log a trace line with old/new token counts and tail size

### Key Design Choices

1. **Model-compacted, not algorithmically truncated** — preserves semantic meaning.
2. **Verbatim tail preserved** — last ~10% kept word-for-word so the model retains immediate recent context.
3. **Sentinel-marked summary** — explicit framing for both model and UI; UI renders distinctly.
4. **Single-process, synchronous compaction** — worker thread owns the DS4 session and blocks; simpler but cannot serve tools and compaction simultaneously.
5. **Failure isolation** — explicit `ds4_session_invalidate` if anything fails; old transcript restored if sync fails.
6. **Bash job awareness** — injects a reminder so the model re-checks background subprocess state not captured in the summary.
7. **First-class state machine entry** — `AGENT_WORKER_COMPACTING` lives alongside `IDLE`, `PREFILL`, `GENERATING`, `SAVING`, letting the UI show a live `COMPACTING...` status with streaming summary.

## Implications for cake

- **Threshold model is portable.** The dual-threshold gate (soft percent + min free tokens, with a small-context cap) is a clean trigger design cake could adopt for any future compaction feature; it avoids both premature compaction on small contexts and late compaction on large ones.
- **Verbatim tail + sentinel-wrapped summary** is a low-risk pattern: it limits information loss vs. raw summarization and gives the UI a reliable way to render the boundary. Cake's session rendering already differentiates roles, so adding sentinel markers would integrate cleanly.
- **Forced compaction on tool-result overflow** is a notable robustness behavior. Cake currently has no analogous fallback when a tool result would push past the context — worth tracking as a follow-up.
- **First-class worker state** matches how cake models agent loop phases; adopting a `Compacting` variant would be consistent with the existing pattern if compaction lands.
- **Synchronous, single-owner compaction** is the right baseline trade-off: simpler to reason about, no concurrent KV-cache mutation hazards. Async compaction is a later optimization, not a prerequisite.
- **Bash-job reminder injection** highlights that any stateful out-of-band tool (running shells, watchers, long-poll subscriptions) needs an explicit re-grounding hook after compaction. Cake should keep this in mind if/when it grows similar long-running tools.

## Follow-ups

- Cross-reference with existing cake notes:
  - `topics/context-compaction.md`
  - `topics/context-management.md`
  - `sources/pi-compaction.md`
- Consider whether cake needs a compaction-trigger design doc before any implementation task.
- Open question: cake uses OpenAI-compatible APIs (no local KV cache), so "re-sync DS4 session" has no direct analogue — compaction in cake would be purely transcript-level rewriting sent on the next request. Worth noting in any future ExecPlan.
