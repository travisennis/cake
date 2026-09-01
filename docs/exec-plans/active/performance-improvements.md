# Performance Improvements Plan

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows the ExecPlan workflow ([docs/workflow/exec-plans.md](../../workflow/exec-plans.md)). It was migrated from the former `.agents/.plans/` location and remains active for separate session-persistence and provider-path investigations; the first representative agent-loop workload and one measured source-level optimization are now complete.

## Purpose / Big Picture

Cake should get faster and easier to profile based on measured bottlenecks rather than guesses. After this work, a contributor can run a repeatable profiling workflow, compare representative workloads, and apply source-level improvements only when profiling shows they matter.

The behavior is observable by running documented profiling commands, collecting before-and-after timings or profiler output for representative workloads, and preserving any accepted optimizations with tests or benchmarks.

## Progress

- [x] (2026-05-07 18:49Z) Confirmed the historical note records that `panic = "abort"` was added and reduced binary size from about 6.6 MB to 5.8 MB.
- [x] (2026-05-07 18:49Z) Migrated this plan to `docs/exec-plans/active/performance-improvements.md` and added the required ExecPlan lifecycle sections.
- [x] (2026-08-28 00:00Z) Added the `profiling` Cargo profile, the `just profile` recipe, and a local fake-provider agent-loop workload with a verified Samply CPU path and optional exploratory Instruments allocation path.
- [x] (2026-08-29 00:00Z) Confirmed the current release profile and recorded the native `aarch64-apple-darwin` binary baseline at 8,891,552 bytes in `ci/binary-size-baseline.json`.
- [x] (2026-09-01 00:00Z) Profiled and inspected the representative tool-heavy agent-loop workload. Samply was installed but could not attach from this restricted parent (`Unknown(1100)`); the macOS `sample` fallback identified Read execution, path validation, blocking-pool scheduling, and request serialization as the Cake-owned areas worth investigating. No separate workload was needed because this profile provided an accepted hotspot.
- [x] (2026-09-01 00:00Z) Implemented and measured a narrow agent-loop optimization: skip post-hook output cloning when hooks are disabled, skip skill-path parsing/canonicalization when no skills are configured, and pre-size tool-call, scheduling-group, and result vectors.
- [x] (2026-09-01 00:00Z) Ran `just check-full`: all Rust, fixture, coverage, dependency, documentation, and release-build checks passed (1,394 tests plus all auxiliary suites; 94.23% coverage).

## Surprises & Discoveries

- Observation: This note already contains some decisions and a binary-size measurement. The first agent-loop profile and source-level optimization are now recorded above; separate benchmark infrastructure and session/allocation workloads remain intentionally out of scope for this issue. Evidence: the standard workload supplied a sufficient hotspot and a valid before/after timing comparison without adding a permanent benchmark suite.
- Observation: A standalone compiled Cake binary is the useful profiling target. The repeatable workload therefore starts a local fake Responses API and a temporary fixture from `scripts/profile-agent-loop.py` instead of profiling a Cargo test harness. Evidence: The workload makes two localhost requests, executes `Read`, and checks the expected tool-output turn before accepting the profile.
- Observation: One `Read` call completed too quickly for useful CPU sampling: four real Samply captures contained only 41-46 total samples over 8-9 ms. A single response containing 5,000 independently identified `Read` calls preserved the two-request agent-loop shape while producing 14,242 Cake samples over 184 ms in the verification capture. Evidence: `profiling/artifacts/verification-batched-5000.jslb.gz` on macOS 26.6.2 with samply 0.13.1.
- Observation: Instruments is not reliable enough to be the primary allocation workflow for this short-lived workload. Evidence: after developer mode was enabled and the full command was run from an ordinary terminal, `xctrace` waited for its 30-second limit and reported `Failed to attach to target process`; the same workload normally completes under Samply in about 200 ms.
- Observation: The documented `just profile --output profiling/artifacts/issue-47-before.jslb.gz` command could not record in this restricted parent. Samply 0.13.1 returned `Unknown(1100)` (`BOOTSTRAP_NOT_PRIVILEGED`), and no artifact was accepted. This is the same macOS parent/bootstrap limitation described by the runbook, so the result was recorded as unavailable rather than treated as a profile.
- Observation: The built-in macOS sampler inspected `/tmp/cake-issue-47-sample-20000.txt`, a 20,000-call amplification used only to make this short-lived process observable. Its call graph was dominated by the Cake `execute_read_tool`/`run_group` path, with `validate_path`/filesystem canonicalization and Tokio blocking-pool synchronization below it; request serialization appeared but was much smaller. The corresponding post-change fallback sample was too short to produce a useful call graph, so it was not used for the timing comparison.
- Observation: The current native release binary is 8,891,552 bytes (8.48 MiB) with the release profile. `cargo bsize` reports 10.6 MiB for its debuginfo analysis artifact and emits 168 missing-object warnings for `aws_lc_sys`; the warned members exist in the archive and build output, so the issue is incomplete native-object attribution rather than missing linked code.
- Observation: The source inspection confirmed two avoidable operations in the standard no-hook/no-skill workload: every successful tool call cloned its output solely to prepare a post-hook result that could never be consumed, and every `Read` call parsed and canonicalized its argument even when the skill-location map was empty. Five valid 5,000-call runs after the change had a median of 0.201759 s versus 0.271972 s for the three-run pre-change warm baseline (about 26% lower); every run still made exactly two localhost provider requests and passed the fixture validation.
- Observation: Full validation passed after the ExecPlan was formatted with panache. The gate reported 94.23% total coverage, no CRAP regression, no complexity exceedances, and successful docs/dependency/release checks.

## Decision Log

- Decision: Classify this plan as active during the ExecPlan migration. Rationale: It has a partial historical decision but no evidence that the profiling, benchmark, and measured optimization milestones have been completed. Date/Author: 2026-05-07 / Codex
- Decision: Use a temporary workspace and a localhost fake Responses API for the first repeatable workload. Rationale: Profiling the compiled Cake binary keeps the result focused on Cake while avoiding production latency, credentials, and a committed session fixture. Date/Author: 2026-08-28 / Cake
- Decision: Amplify the default tool-heavy workload to 5,000 `Read` calls in one provider response and require every output to contain the complete fixture markers. Rationale: Repeating Cake-owned work in one process yields a useful sample population, while unique call IDs and content validation prevent failed or missing tool calls from being accepted as profiles. The batch is a comparison workload, not a claim about typical model behavior. Date/Author: 2026-08-28 / Codex
- Decision: Treat Samply as the supported primary workflow and Instruments as an optional exploratory secondary path. Rationale: the Samply capture is repeatable and measured, while Instruments remains macOS-only, authorization-sensitive, and unable to attach to the verified short-lived workload. Issue #383 will evaluate `dhat-rs` and alternatives as a portable allocation workflow and decide whether Instruments remains useful. Date/Author: 2026-08-28 / Codex
- Decision: Accept the agent-loop optimization rather than pursuing speculative request or session rewrites. Rationale: the fallback profile identified the tool-heavy Read path as the dominant Cake-owned work, and source inspection found redundant no-op cloning and skill detection in that exact path. The capacity hints are allocation reductions in the same path; no API, tool ordering, or sandbox semantics change. Date/Author: 2026-09-01 / Cake
- Decision: Use the exact native release artifact size as the committed baseline, not the larger `target/bsize` artifact or the bsize report's estimated shipped size. Rationale: the baseline must represent the file distributed to users; target and toolchain metadata make comparisons explicit, while separate release targets require separate baselines. Date/Author: 2026-08-29 / Codex
- Decision: Keep the standard 5,000-call workload as the before/after timing contract. Rationale: larger amplifications were useful for sampling but intermittently exceeded the local helper's two-request reliability envelope; timing comparisons therefore use only valid default-size runs. Date/Author: 2026-09-01 / Cake

## Outcomes & Retrospective

- The representative workload now has an inspected hotspot record and a repeatable valid timing comparison. The accepted change removes work that is provably unnecessary when lifecycle hooks and skills are absent, while preserving both paths when they are configured.
- The Read-only tool workload remains behaviorally unchanged: it still validates every requested path, executes calls concurrently, emits outputs in model order, and performs hook/skill handling when those features are active.
- Samply and Instruments remain unavailable as reliable profilers in this restricted macOS environment. The runbook's recovery instructions and the artifact paths above provide the handoff for an unrestricted follow-up; no generated profiles are committed.
- Session-load, allocation-count, and compiler-level experiments were not pursued because the first profile supplied a useful agent-loop hotspot and the issue explicitly defers unmeasured compiler changes.

## Context

cake is an I/O-bound CLI (network requests to LLM APIs, subprocess execution for tools, JSON serialization of conversation history). This is fundamentally different from seqpacker's compute-bound bin-packing. The article's methodology still applies: profile first, fix in source, skip compiler heroics.

Current state: - Release binary: 8.48 MiB (with `lto = true`, `codegen-units = 1`, `strip = true`) - Native `aarch64-apple-darwin` size baseline is committed in `ci/binary-size-baseline.json` - Profiling infrastructure exists - No benchmarks exist

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## Phase 0: Establish Baselines

**Goal:** Measure before changing anything.

### 0.1 --- Release Profile Audit

The current release profile uses `lto = true`, which is Cargo's fat-LTO setting. The article calls out whole-program visibility as useful for cross-module inlining; measure any future profile change against the committed binary baseline.

Also, `panic = "abort"` is already set in the release profile. It removes unwind machinery at the cost of no stack unwinding or panic recovery.

> **Question:** Is there any scenario where cake needs to catch panics (e.g., `std::panic::catch_unwind`)? If not, `panic = "abort"` is free.

### 0.2 --- Add a Profiling Profile

Create a `[profile.profiling]` that inherits from release but keeps debug symbols:

```toml
[profile.profiling]
inherits = "release"
debug = true
strip = false
```

This lets profiling tools (samply, flamegraph) resolve function names without affecting the release build.

### 0.3 --- Add a `just profile` Recipe

```just
profile *ARGS:
    cargo build --profile profiling
    python3 scripts/profile-agent-loop.py {{ARGS}}
```

The recipe uses a local fake Responses API, so it does not require provider credentials or production network access. Samply is the supported primary CPU workflow. The helper also exposes an optional macOS Instruments allocation path, whose replacement or removal is tracked in issue #383.

### 0.4 --- Identify Representative Workloads

Define 2-3 reproducible workloads for profiling. Candidates:

1. **Session load + replay**: Load a large saved session (many turns), measure deserialization time
2. **Tool-heavy turn**: A prompt that triggers multiple tool calls (bash, read, edit) in a single agent loop iteration
3. **Long conversation**: A multi-turn session with growing history, measuring how JSON serialization scales

> **Question:** Which of these workloads matters most to you? The profiling results will differ significantly. Session load/save is likely the most measurable locally since network latency dominates the agent loop.

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## Phase 1: Profile with Flamegraph

**Goal:** Identify actual hotspots before making any code changes.

### 1.1 --- Generate Flamegraph

Run `just profile` against each workload. The Firefox Profiler UI will show: - Where wall-clock time is spent - Call stacks with time attribution - Whether time is in cake's code vs. dependencies (reqwest, serde, tokio)

### 1.2 --- Categorize Hotspots

Expected hotspot categories for cake (hypothesized, to be validated by profiling):

  | Category             | Likely Location                                                                           | Article Analog                |
  | -------------------- | ----------------------------------------------------------------------------------------- | ----------------------------- |
  | JSON serialization   | `to_api_input()`, `StreamRecord`/`SessionRecord` serialization, request body construction | N/A (cake-specific)           |
  | JSON deserialization | `parse_response()` in both backends                                                       | N/A                           |
  | String cloning       | `.clone()` calls on conversation items in the agent loop                                  | Heap allocation in inner loop |
  | Session I/O          | JSONL read/write for session persistence                                                  | N/A                           |
  | HTTP overhead        | reqwest connection setup, TLS                                                             | N/A (external)                |

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## Phase 2: Code-Level Fixes (Informed by Profiling)

Only pursue these after Phase 1 confirms they're actual hotspots. Each maps to a pattern from the article.

### 2.1 --- Reduce Cloning in the Agent Loop

The `send()` method clones data in several places: - `message.content.clone()` for user messages - `id.clone(), call_id.clone(), name.clone(), arguments.clone()` when collecting function calls - `content.clone()` in `resolve_assistant_message()`

If profiling shows string cloning is significant, consider: - Borrowing instead of cloning where lifetimes allow - Using `Arc<str>` for conversation content that gets shared across callbacks and history

> **Question:** How large do conversation histories typically get in practice? If sessions routinely have 100+ turns with large tool outputs, the cloning cost compounds. If sessions are typically short (5-10 turns), this is noise.

### 2.2 --- Pre-allocate Vectors (Article Pattern #1)

The agent loop builds vectors without capacity hints: - `function_calls` in `send()` --- could use `with_capacity` based on `turn_result.items.len()` - `results` from tool execution --- size is known from `function_calls.len()`

These are small wins but essentially free to implement.

### 2.3 --- JSON Construction Overhead

`to_api_input()` still exists as a test helper around the typed Responses API DTO, while production request, stream, and session output use typed serde DTOs. If profiling shows serialization is hot: - Consider direct serialization with `#[derive(Serialize)]` on purpose-built request structs instead of building `Value` trees - The `Request`, `StreamRecord`, and `SessionRecord` structs in `types.rs` already use derive, so focus on measured allocation costs rather than replacing hand-built JSON paths that no longer exist

> **Question:** Is there a reason `to_api_input()` builds `serde_json::Value` dynamically instead of using typed structs with `#[derive(Serialize)]`? If not, this is both a performance and maintainability improvement.

### 2.4 --- Session Serialization

Session persistence uses JSONL. If session load/save shows up in profiling: - Consider `serde_json::to_writer` directly to avoid intermediate string allocation - Pre-allocate the read buffer based on file size - Consider whether `simd-json` would help (only if JSON parsing dominates)

### 2.5 --- Cold Path Annotation (Article Pattern #5)

Mark error-handling and rare-path functions with `#[cold]`: - Error formatting in `complete_turn()` (the non-success branch) - Session recovery/migration paths - Sandbox setup (runs once per bash invocation, not in the hot loop)

This tells LLVM to optimize the common path at the expense of cold paths.

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## Phase 3: Compiler-Level Experiments

Only after code-level fixes are applied and measured.

### 3.1 --- LTO experiments

Measure any future LTO change against the current fat-LTO release profile. The binary baseline is the comparison point for compiler experiments.

### 3.2 --- PGO (Measure, Probably Skip)

The article's key finding: PGO gives \~15% on unoptimized code but adds nothing after manual profiling fixes. For cake: - The hot paths are mostly in dependencies (reqwest, serde_json, tokio), not cake's own code - PGO adds CI complexity - Likely not worth it

Run PGO once to confirm it adds nothing after Phase 2 fixes, then document the result and move on.

### 3.3 --- `target-cpu=native` (Skip)

The article found this neutral-to-harmful for non-SIMD workloads. cake has no vectorizable loops. Skip this.

### 3.4 --- BOLT (Skip)

The article found no improvement for small, cache-friendly binaries. The historical estimate described cake at 6.6 MB; the current native release is 8.48 MiB. The critical path is I/O-bound.

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## Phase 4: Benchmark Infrastructure

### 4.1 --- Micro-benchmarks with Criterion

Add benchmarks for the operations most likely to be hot: - `ConversationItem::to_api_input()` serialization with varying history sizes - Session JSONL loading with varying session sizes - Request body construction for both API backends - `build_messages()` in chat_completions with large histories

### 4.2 --- End-to-End Timing

Add `--timing` or use the existing `duration_ms` in result messages to track full turn latency. Log per-phase timing (request build, API call, response parse, tool execution) behind a debug flag.

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## Decisions Made

1. **`panic = "abort"`** --- ✅ Added. The historical binary measurement dropped from 6.6 MB to 5.8 MB (about 12% smaller). Revert if crash diagnostics become a problem in practice.
2. **Workload priority** --- Tool-heavy turns > long conversations > session load/save.
3. **Conversation history size** --- Typical long session is 60-80 turns, expected to grow. Real sessions exist in `~/.cache/cake/` for measurement.
4. **`to_api_input()` dynamic JSON** --- No reason for the current approach. Open to replacing with typed structs (perf + maintainability win).
5. **Binary size** --- The earlier 5.8 MB decision is historical. The current native `aarch64-apple-darwin` release baseline is 8.48 MiB in `ci/binary-size-baseline.json`; further reductions are welcome if they do not sacrifice other wins.

----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------

## What NOT to Do (Lessons from the Article)

- **Don't add PGO to the build pipeline** --- The article showed it's redundant after source-level fixes, and it adds CI complexity
- **Don't use `target-cpu=native`** --- No SIMD-exploitable workloads, and it hurts portability
- **Don't reach for BOLT** --- Binary is small, hot path is I/O-bound
- **Don't optimize without profiling** --- The article's core lesson: profile first, then fix in source
- **Don't use `unsafe` for bounds-check elimination** --- cake's hot path is I/O, not tight loops over arrays

## Revision Notes

- 2026-05-07 / Codex: Migrated this historical plan into the new active ExecPlan directory and added lifecycle sections required by the ExecPlan workflow. The original profiling notes above remain as the implementation context.
- 2026-08-28 / Cake: Added the deterministic agent-loop profiling workflow for issue #50. The remaining profiling, measurement, and source-optimization milestones stay open.
- 2026-08-28 / Codex: Verified the real Samply path, recorded the insufficient sample population from the original one-call workload, amplified and validated the tool batch, and corrected the macOS xctrace control target. Representative hotspot analysis and source optimization remain open.
- 2026-08-28 / Codex: Demoted Instruments to an optional exploratory path after its full Cake recording still failed to attach with developer mode enabled, and opened issue #383 to evaluate a portable allocation profiler or remove Instruments. Samply remains the verified primary workflow.
- 2026-09-01 / Cake: Inspected the tool-heavy Read workload with the macOS sampler fallback, skipped redundant no-hook/no-skill work, added vector capacity hints, measured an approximately 25% warm-median improvement at the valid 5,000-call workload, and passed `just check-full`. Samply remained unavailable under the restricted parent.
