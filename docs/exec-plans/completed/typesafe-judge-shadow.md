# Evaluate TypeSafe judgments alongside the Bash judge

This ExecPlan is a living document maintained per docs/workflow/exec-plans.md. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective stay current throughout issue #602.

## Purpose / Big Picture

Cake users pay the latency of a generative safety judge for every Bash command. This first PR lets users evaluate whether Jev can identify commands eligible for fast approval, without letting Jev authorize execution. Users explicitly enable shadow mode, supply TYPESAFE_AI_API_KEY, and inspect metadata and benchmark results. Shadow means an additional judgment whose answer never controls execution. The existing judge remains authoritative. Production cascade approval is outside this issue.

## Progress

- [x] (2026-09-18) Inspected current judge, settings, corpus, telemetry, API contract, and repository workflow; created issue #602 and feature branch.
- [x] (2026-09-18) Recorded implementation scope and ADR 033 before code changes.
- [x] (2026-09-18) Implemented bounded TypeSafe client and default-off configuration with credential-free mock-provider tests.
- [x] (2026-09-18) Integrated concurrent opt-in shadow evaluation and metadata-only observations through the shared judge path.
- [x] (2026-09-18) Extended both corpus benchmark paths with grouped threshold reports and documented live smoke commands; verified report calculations and routing.
- [x] (2026-09-18) Completed native just check and three-pass preflight; prepared the review-ready handoff. Linux cross-check is blocked by missing x86_64-linux-gnu-gcc; paid live evaluation is intentionally unrun.

## Surprises & Discoveries

The existing benchmark already evaluates command text without executing it and supports independent labeled cases. The independent set contains 34 cases in 14 pair groups and no warnings, so the report must expose zero denominators and small samples. reqwest has implicit protocol retries by default; the new client explicitly disables them. Its historic latency commentary is not a current baseline. TypeSafe uses a dedicated typed-question endpoint rather than the existing Chat Completions/Responses protocol.

## Decision Log

On 2026-09-18 Trav clarified that rg -rn advisory warnings belong to hooks. Jev safety eligibility must not hard-exclude an otherwise observational command for this footgun. Keep the existing primary rubric unchanged in this PR and report legacy warning disagreement separately from unsafe approval; the hook path remains authoritative for its own warnings.

On 2026-09-18 Trav requested this first PR and a Luna implementer. Only off and shadow modes ship; reject unsupported cascade mode. Use TYPESAFE_AI_API_KEY, not the SDK's usual variable. Default off must neither resolve the credential nor contact TypeSafe. A pinned Jev version makes measurements reproducible. Use the current documented jev-1.13.0 default, configurable for later evaluation. Start with a configurable 1000 ms total observation deadline and no TypeSafe retries; this is a test setting rather than a latency promise. Errors become typed observation failures, never new command denials or approvals. Existing emergency bypass avoids both judges. A missing primary judge/configuration must still fail closed without being rescued by shadow evaluation.

## Outcomes & Retrospective

Delivered default-off TypeSafe client/configuration, shared concurrent shadow observation, linked metadata-only telemetry, and legacy/independent benchmark reporting at candidate thresholds 0.9, 0.95, 0.99 and 0.999. No threshold is selected and no TypeSafe result controls execution. Settings use the fixed TYPESAFE_AI_API_KEY credential variable. Profiles can configure TypeSafe without newly enabling primary judge policy overrides. Hook handling and the primary rubric are unchanged.

Luna implemented the initial integration, reports, and regression tests. Parent review replaced untyped response parsing, removed client-construction panic paths, explicitly disabled reqwest's implicit protocol retries, bounded response reads, simplified duplicate report counters, preserved independent pair grouping, and strengthened tests for opposite shadow/primary verdicts and metadata redaction. The implementation uses existing dependencies.

Validation passed: cargo check, cargo clippy --bin cake -- -D warnings, cargo test typesafe -- --nocapture (8 tests), cargo test shadow_report -- --nocapture (4 tests), and final just check (1542 unit tests plus integration suites, 2 paid live tests ignored, strict all-feature Clippy, formatting, complexity and script gates). The full gate includes deterministic corpus and benchmark suites, so separate final invocations of their subset recipes were unnecessary. Changed Markdown passed panache lint. just clippy-linux could not run because x86_64-linux-gnu-gcc is absent; no platform-specific sandbox source changed. No live provider request, performance claim, or safety calibration was made. The documented live benchmark is the next evaluation step, outside this implementation's credential-free acceptance.

## Context and Orientation

Cake is a Rust binary. src/clients/judge.rs owns JudgeClient, JudgeContext, JudgeRequest and evaluate_command_observed. That shared function evaluates commands for Bash, cake bash check, and evaluation harnesses, then applies the exact raw-command allowlist. src/clients/judge_observer.rs owns primary provider attempts. src/config/settings.rs resolves global/project/profile settings. src/session_telemetry.rs holds metadata-only records. src/clients/judge_benchmark_tests.rs and src/clients/judge_independent_tests.rs implement existing evaluation reports. src/clients/tools/corpus/commands.jsonl and independent-v1.jsonl contain labeled examples. src/clients/tools/bash.rs executes commands after the judge under the unchanged OS sandbox. docs/adr/033-typesafe-judge-shadow-evaluation.md records the new observational extension, without replacing ADR 018's execution gate.

## Plan of Work

### Milestone 1: Typed client and opt-in settings

Add src/clients/typesafe.rs (and focused test modules as appropriate), using existing reqwest, serde, and tokio dependencies. POST https://api.typesafe.ai/v1/systemone with Authorization Bearer from TYPESAFE_AI_API_KEY and JSON containing model, state and questions. State contains the exact command, cwd, compact repo digest and explicitly untrusted reason. Include effective embedded and custom rubric guidance in the eligibility question. A question named eligible uses type noul, natural-language instructions, and true/false criteria; advisory-only footgun warnings are not safety failures; its answer is answers.eligible.type = noul and answers.eligible.noul, a finite number in [0,1]. Validate required fields, model identity and answer type. Record returned model and usage when valid. Disable redirects, bound body size and total request/body deadline, and classify failures without retaining provider bodies or credential-bearing transport text. Keep any mock endpoint injection
private/test-only unless a real product need emerges.

Add a nested [tools.bash.judge.typesafe] configuration containing mode (off/shadow), model, and timeout_ms. Read the fixed credential variable TYPESAFE_AI_API_KEY rather than adding a configurable api_key_env field. Preserve global/project/profile precedence. No production approval threshold is required in shadow mode: the benchmark owns candidate thresholds. Test defaults, overlays, invalid settings, malformed answers, timeout, HTTP failures, redirects, and redaction with local mock servers; no external credentials are needed.

### Milestone 2: Shadow observation without authority

Integrate through evaluate_command_observed so all caller facades share semantics. Keep the existing judge result authoritative, including errors, allowlist overrides and emergency bypass. Run the bounded shadow request concurrently with primary evaluation; do not detach tasks past operation completion. The join may add up to the bounded shadow deadline if the primary finishes faster; document and measure this rather than claiming zero latency. Preserve existing cancellation and deadline behavior. Carry a typed optional shadow observation in the evaluation and persist metadata through the existing telemetry sink, avoiding a new session JSONL record or changes to completion JSON. Off emits no new request or shadow event. Shadow failure cannot change allow/block/warn, spawn a command, or suppress a primary failure.

Enumerated failure classes to defend before implementing: command/reason prompt injection affecting execution via shadow leakage; accidental bypass of primary judge or allowlist; custom rubric omission; credentials in Debug/error logs; redirect credential forwarding; unbounded response/deadline/retries; provider-controlled model strings or bodies leaking raw content to normal telemetry; changed session/output contracts; and slow/cancelled shadow tasks escaping their lifetime. Preserve macOS/Linux sandbox grants and execution paths. Treat provider metadata as untrusted, using established digest/redaction conventions where needed.

### Milestone 3: Evaluation and user documentation

Extend the existing benchmark rather than introduce an independent CLI/framework. Reports should retain per-case probability and distinguish independently labeled destructive approvals, advisory warning disagreements, candidate approval coverage, disagreement with primary verdict, missing observations, TypeSafe latency and failures. Candidate threshold sweeps are offline report calculations, not execution policy. Separate tuning and held-out summaries using deterministic documented case grouping that keeps related pair IDs together; do not claim a tuned threshold is validated on its tuning data. Represent absent or unscored labels explicitly, and expose denominators so failures cannot artificially improve accuracy. Do not treat primary judge agreement as ground truth. Parallel shadow durations cannot establish sequential cascade p95: any simulated saving must be labeled estimated, with actual cascade measurement deferred.

Document exact runnable configuration and existing benchmark commands in docs/configuration.md and the corpus README or established benchmark runbook. Update docs/security.md and docs/integrations.md for opt-in data transmission and additive telemetry only, with architecture changes limited to the new observational path. Existing configured judge credentials remain necessary. Setting TYPESAFE_AI_API_KEY alone enables nothing. Document that raw command/context are sent to TypeSafe when shadow is enabled, while local telemetry remains metadata-only. Do not run paid provider calls automatically; produce mock-backed proof and exact instructions for a separately authorized live run.

### Milestone 4: Verification and review-ready handoff

Run focused tests while developing, then just check and applicable documentation gates. Apply the preflight skill's three sequential passes: rules/contracts, correctness, and simplification. Record precise passing or blocked checks and avoid unrelated cleanup. Update issue acceptance and documentation impact, fill this plan's outcomes, and git mv it to docs/exec-plans/completed/typesafe-judge-shadow.md before opening the PR. Stage explicit paths, use Conventional Commits, push the feature branch and open the labeled PR referencing Closes #602. Do not merge.

## Concrete Steps

All commands run from /Users/travisennis/Projects/cake/cake-0 on feat/typesafe-judge-shadow. The branch already exists; do not create a second branch. Inspect issue with gh issue view 602. During development run cargo test typesafe and relevant judge/report tests; just judge-corpus-check and just judge-bench-check are available focused subsets of the final full test gate. Final gate is just check; documentation gate is just docs-check after tracking new Markdown files so the checker includes them. Expect successful exits and no provider calls. Consult CONTRIBUTING.md for exact routed gates and report any environment failures.

For evaluation after implementation, export TYPESAFE_AI_API_KEY through the user's secret environment without printing it, enable mode = "shadow" under tools.bash.judge.typesafe, and run the documented existing benchmark with CAKE_JUDGE_BENCH_CORPUS=independent just judge-bench and a bounded case/repetition selection. The implementation must document the exact resulting report paths, threshold selection and split semantics before this step is considered usable. Never execute corpus commands.

## Validation and Acceptance

Mock tests demonstrate identical primary allow, warn, block, error and allowlist outcomes with shadow enabled versus off, including positive Jev judgments for primary-denied commands. Off and bypass produce zero TypeSafe calls. Missing key, malformed/out-of-range answers, server errors, timeouts and redirects remain observation failures only. Tests check effective custom rubric and exact command context, concurrent bounded lifecycle, and no raw command/key/body leakage in normal telemetry. Benchmark fixture tests establish threshold boundary behavior, failure denominators, label disagreement, pair-preserving split, and latency calculations. Existing settings and output snapshots remain compatible except intentional additive documented telemetry/configuration behavior. No shell parser or OS grant changes belong here.

## Idempotence and Recovery

Tests and offline benchmarks can be repeated without command side effects. Live benchmarks consume provider tokens and must be intentionally invoked. Disabling shadow restores the prior request path immediately. A TypeSafe failure falls back to the already-running authoritative judge; it never retries or authorizes. Preserve unrelated working-tree changes. If a gate cannot run, inspect the prerequisite and record exact evidence rather than claiming a pass.

## Artifacts and Notes

Issue #602 is the acceptance record. Keep local benchmark artifacts in the existing ignored results directory. Commit no credentials, raw live transcripts, or generated benchmark outputs. Implementation remains the source of truth for precise module names and tests; revise this plan if discovery changes them.

## Interfaces and Dependencies

Use existing dependencies only unless a specific gap is demonstrated. A typed TypeSafe observation should represent success probability, elapsed time and safe model identity/usage, or a bounded failure class. JudgeEvaluation carries the optional observation; it cannot convert it into JudgeOutcome. Configuration belongs in src/config/settings.rs, provider I/O in src/clients/, and serialization in the existing telemetry boundary. Keep per-function complexity within repository targets and use absolute crate imports.

Revision note (2026-09-18): Completed implementation and review, incorporated Trav's hook-warning correction, recorded validation evidence and Linux/live-evaluation limitations, and prepared archival for the first PR.
