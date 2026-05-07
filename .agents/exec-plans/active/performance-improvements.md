# Performance Improvements Plan

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `.agents/PLANS.md` from the repository root. It was migrated from the former `.agents/.plans/` location and remains active because it contains a broad performance investigation with some decisions recorded but no completed validation of profiling, benchmarks, or source-level optimization work.

## Purpose / Big Picture

Cake should get faster and easier to profile based on measured bottlenecks rather than guesses. After this work, a contributor can run a repeatable profiling workflow, compare representative workloads, and apply source-level improvements only when profiling shows they matter.

The behavior is observable by running documented profiling commands, collecting before-and-after timings or profiler output for representative workloads, and preserving any accepted optimizations with tests or benchmarks.

## Progress

- [x] (2026-05-07 18:49Z) Confirmed the historical note records that `panic = "abort"` was added and reduced binary size from about 6.6 MB to 5.8 MB.
- [x] (2026-05-07 18:49Z) Migrated this plan to `.agents/exec-plans/active/performance-improvements.md` and added the required ExecPlan lifecycle sections.
- [ ] Confirm the current release profile and binary-size baseline in the working tree.
- [ ] Add or verify a profiling profile and repeatable profiling command.
- [ ] Profile representative workloads before changing hot-path code.
- [ ] Implement only measured source-level improvements, then document results and run `just ci`.

## Surprises & Discoveries

- Observation: This note already contains some decisions and a binary-size measurement, but it does not identify completed profiling artifacts or accepted benchmark infrastructure.
  Evidence: The original `Decisions Made` section records `panic = "abort"` and workload priority, while the profiling and benchmark sections remain instructions rather than completed outcomes.

## Decision Log

- Decision: Classify this plan as active during the ExecPlan migration.
  Rationale: It has a partial historical decision but no evidence that the profiling, benchmark, and measured optimization milestones have been completed.
  Date/Author: 2026-05-07 / Codex

## Outcomes & Retrospective

This performance plan is still open. At completion, update this section with the profiled workloads, concrete measurements, source changes made or rejected, benchmark results, and the final `just ci` outcome.

Systematic plan for profiling and optimizing cake, adapted from the [seqpacker profiling article](https://alphakhaw.com/blog/seqpacker-profiling-rust-flamegraph-pgo-bolt).

---

## Context

cake is an I/O-bound CLI (network requests to LLM APIs, subprocess execution for tools, JSON serialization of conversation history). This is fundamentally different from seqpacker's compute-bound bin-packing. The article's methodology still applies: profile first, fix in source, skip compiler heroics.

Current state:
- Release binary: ~6.6 MB (with `lto = true`, `codegen-units = 1`, `strip = true`)
- No profiling infrastructure exists
- No benchmarks exist
- `[profile.release]` already has LTO and single codegen unit, but uses `lto = true` (thin) rather than `lto = "fat"`

---

## Phase 0: Establish Baselines

**Goal:** Measure before changing anything.

### 0.1 — Release Profile Audit

The current release profile uses `lto = true`, which defaults to **thin LTO**. The article calls out `lto = "fat"` as the single most impactful setting because it gives LLVM whole-program visibility for cross-module inlining. Switch to `lto = "fat"` and measure compile time and binary size difference.

Also consider adding `panic = "abort"` to the release profile. This removes unwind tables and reduces binary size. cake already denies `unwrap_used`/`expect_used`, so panic paths should be rare.

> **Question:** Is there any scenario where cake needs to catch panics (e.g., `std::panic::catch_unwind`)? If not, `panic = "abort"` is free.

### 0.2 — Add a Profiling Profile

Create a `[profile.profiling]` that inherits from release but keeps debug symbols:

```toml
[profile.profiling]
inherits = "release"
debug = true
strip = false
```

This lets profiling tools (samply, flamegraph) resolve function names without affecting the release build.

### 0.3 — Add a `just profile` Recipe

```just
profile *ARGS:
    cargo build --profile profiling
    samply record ./target/profiling/cake {{ARGS}}
```

Requires `cargo install samply` (macOS). samply uses dtrace under the hood and outputs to Firefox Profiler UI with no additional setup.

### 0.4 — Identify Representative Workloads

Define 2-3 reproducible workloads for profiling. Candidates:

1. **Session load + replay**: Load a large saved session (many turns), measure deserialization time
2. **Tool-heavy turn**: A prompt that triggers multiple tool calls (bash, read, edit) in a single agent loop iteration
3. **Long conversation**: A multi-turn session with growing history, measuring how JSON serialization scales

> **Question:** Which of these workloads matters most to you? The profiling results will differ significantly. Session load/save is likely the most measurable locally since network latency dominates the agent loop.

---

## Phase 1: Profile with Flamegraph

**Goal:** Identify actual hotspots before making any code changes.

### 1.1 — Generate Flamegraph

Run `just profile` against each workload. The Firefox Profiler UI will show:
- Where wall-clock time is spent
- Call stacks with time attribution
- Whether time is in cake's code vs. dependencies (reqwest, serde, tokio)

### 1.2 — Categorize Hotspots

Expected hotspot categories for cake (hypothesized, to be validated by profiling):

| Category | Likely Location | Article Analog |
|----------|----------------|----------------|
| JSON serialization | `to_api_input()`, `to_streaming_json()`, request body construction | N/A (cake-specific) |
| JSON deserialization | `parse_response()` in both backends | N/A |
| String cloning | `.clone()` calls on conversation items in the agent loop | Heap allocation in inner loop |
| Session I/O | JSONL read/write for session persistence | N/A |
| HTTP overhead | reqwest connection setup, TLS | N/A (external) |

---

## Phase 2: Code-Level Fixes (Informed by Profiling)

Only pursue these after Phase 1 confirms they're actual hotspots. Each maps to a pattern from the article.

### 2.1 — Reduce Cloning in the Agent Loop

The `send()` method clones data in several places:
- `message.content.clone()` for user messages
- `id.clone(), call_id.clone(), name.clone(), arguments.clone()` when collecting function calls
- `content.clone()` in `resolve_assistant_message()`

If profiling shows string cloning is significant, consider:
- Borrowing instead of cloning where lifetimes allow
- Using `Arc<str>` for conversation content that gets shared across callbacks and history

> **Question:** How large do conversation histories typically get in practice? If sessions routinely have 100+ turns with large tool outputs, the cloning cost compounds. If sessions are typically short (5-10 turns), this is noise.

### 2.2 — Pre-allocate Vectors (Article Pattern #1)

The agent loop builds vectors without capacity hints:
- `function_calls` in `send()` — could use `with_capacity` based on `turn_result.items.len()`
- `results` from tool execution — size is known from `function_calls.len()`

These are small wins but essentially free to implement.

### 2.3 — JSON Construction Overhead

`to_api_input()` and `to_streaming_json()` both build `serde_json::Value` trees dynamically with `serde_json::json!()`. If profiling shows these are hot:
- Consider direct serialization with `#[derive(Serialize)]` on purpose-built request structs instead of building `Value` trees
- The `Request` struct in `types.rs` already uses derive, but `to_api_input()` and `to_streaming_json()` bypass it

> **Question:** Is there a reason `to_api_input()` builds `serde_json::Value` dynamically instead of using typed structs with `#[derive(Serialize)]`? If not, this is both a performance and maintainability improvement.

### 2.4 — Session Serialization

Session persistence uses JSONL. If session load/save shows up in profiling:
- Consider `serde_json::to_writer` directly to avoid intermediate string allocation
- Pre-allocate the read buffer based on file size
- Consider whether `simd-json` would help (only if JSON parsing dominates)

### 2.5 — Cold Path Annotation (Article Pattern #5)

Mark error-handling and rare-path functions with `#[cold]`:
- Error formatting in `complete_turn()` (the non-success branch)
- Session recovery/migration paths
- Sandbox setup (runs once per bash invocation, not in the hot loop)

This tells LLVM to optimize the common path at the expense of cold paths.

---

## Phase 3: Compiler-Level Experiments

Only after code-level fixes are applied and measured.

### 3.1 — `lto = "fat"` vs `lto = true`

Measure the difference. The article found fat LTO was the single most impactful compiler setting. cake already uses thin LTO; the delta may be small or significant depending on cross-crate inlining opportunities (reqwest, serde, tokio are all separate crates).

### 3.2 — PGO (Measure, Probably Skip)

The article's key finding: PGO gives ~15% on unoptimized code but adds nothing after manual profiling fixes. For cake:
- The hot paths are mostly in dependencies (reqwest, serde_json, tokio), not cake's own code
- PGO adds CI complexity
- Likely not worth it

Run PGO once to confirm it adds nothing after Phase 2 fixes, then document the result and move on.

### 3.3 — `target-cpu=native` (Skip)

The article found this neutral-to-harmful for non-SIMD workloads. cake has no vectorizable loops. Skip this.

### 3.4 — BOLT (Skip)

The article found no improvement for small, cache-friendly binaries. cake at 6.6 MB is small. The critical path is I/O-bound. Skip this.

---

## Phase 4: Benchmark Infrastructure

### 4.1 — Micro-benchmarks with Criterion

Add benchmarks for the operations most likely to be hot:
- `ConversationItem::to_api_input()` serialization with varying history sizes
- Session JSONL loading with varying session sizes
- Request body construction for both API backends
- `build_messages()` in chat_completions with large histories

### 4.2 — End-to-End Timing

Add `--timing` or use the existing `duration_ms` in result messages to track full turn latency. Log per-phase timing (request build, API call, response parse, tool execution) behind a debug flag.

---

## Decisions Made

1. **`panic = "abort"`** — ✅ Added. Binary dropped from 6.6 MB to 5.8 MB (~12% smaller). Revert if crash diagnostics become a problem in practice.
2. **Workload priority** — Tool-heavy turns > long conversations > session load/save.
3. **Conversation history size** — Typical long session is 60-80 turns, expected to grow. Real sessions exist in `~/.cache/cake/` for measurement.
4. **`to_api_input()` dynamic JSON** — No reason for the current approach. Open to replacing with typed structs (perf + maintainability win).
5. **Binary size** — 5.8 MB is acceptable. Further reductions welcome if they don't sacrifice other wins.

---

## What NOT to Do (Lessons from the Article)

- **Don't add PGO to the build pipeline** — The article showed it's redundant after source-level fixes, and it adds CI complexity
- **Don't use `target-cpu=native`** — No SIMD-exploitable workloads, and it hurts portability
- **Don't reach for BOLT** — Binary is small, hot path is I/O-bound
- **Don't optimize without profiling** — The article's core lesson: profile first, then fix in source
- **Don't use `unsafe` for bounds-check elimination** — cake's hot path is I/O, not tight loops over arrays

## Revision Notes

- 2026-05-07 / Codex: Migrated this historical plan into the new active ExecPlan directory and added lifecycle sections required by `.agents/PLANS.md`. The original profiling notes above remain as the implementation context.
