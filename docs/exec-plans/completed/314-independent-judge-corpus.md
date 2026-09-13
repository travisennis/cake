# Build an independent Bash judge evaluation corpus

This ExecPlan is maintained per docs/workflow/exec-plans.md.

## Purpose / Big Picture

Add an independently authored evaluation set and reproducible reporting for judge safety, including authorization and missing evidence. Existing regression labels are not a gold standard for these dimensions. Contributors can validate the new corpus offline and explicitly select it for live benchmarking without changing production judge requests or policy.

## Progress

- [x] (2026-09-13) Read issue #314 and examples in #315, #294, #320, #545; claimed Ready issue and created feature branch.
- [x] (2026-09-13) Authored 34 command scenarios and four provider fixtures with schema/contradiction tests.
- [x] (2026-09-13) Integrated opt-in evaluation, version fingerprints, safety metrics, and benchmark telemetry.
- [x] (2026-09-13) Documented limitations, passed checks and three preflight passes; plan ready for archival and PR handoff.

## Context and Orientation

src/clients/judge_corpus_tests.rs owns the legacy command corpus. src/clients/judge_benchmark_tests.rs evaluates it with real judge calls and deterministic fake-provider tests. The production judge sees command, cwd, repository digest and untrusted reason only. Its warn verdict executes a command, so both warn and allow are false negatives on gold block cases. Baseline reference is f8fd404; a new live baseline has not been authorized.

## Plan of Work

Add a separate versioned JSONL gold corpus in src/clients/tools/corpus/ and test-only Rust support. Gold labels express independently authored scenario expectations, not current rubric agreement. Record user scope, payload, destination, repository evidence, sandbox scope, completeness, provenance and unresolved policy explicitly. Unknown policy is unscored, not guessed. Preserve the legacy corpus and default benchmark mode.

Extend benchmark selection with an explicit independent corpus mode. Reuse provider evaluation, trials, latency, consistency and usage accounting. Add false-allow/block, high-risk false-negative, authorization-decision accuracy and injection-resistance metrics with denominators. Direct authorization classification is unsupported because the production judge emits no authorization label; report null and distinguish the decision proxy. Record omitted context dimensions for every trial using the report projection and linked scenario metadata rather than forwarding them through reason. Report provider failures separately from semantic blocks.

## Milestones

First, add schema validation and cases with paired commands, including cleanup authorization, opaque scripts, denial history and hook-requested journal writes. Run just judge-corpus-check; invalid labels, tags and contradictory combinations must fail offline.

Second, select the corpus with CAKE_JUDGE_BENCH_CORPUS=independent just judge-bench. Add deterministic metric tests to just judge-bench-check. Reports identify corpus, rubric fingerprint, model/provider and unsupported dimensions. No command in a fixture is executed.

Third, update corpus and benchmark contributor documentation, run just check and just cc-check, complete three local preflight passes and deliver a PR. Live evaluation requires separate provider/spend authorization and is not a completion gate for this implementation.

## Validation and Acceptance

From the repository root run just judge-corpus-check, just judge-bench-check, just check, just cc-check, targeted Markdown formatting/lint and git diff --check. Normal tests require no provider credentials or external network. Mock-provider tests use loopback. Verify wrong decisions, warn-as-executable, provider failures and empty metric denominators with synthetic trials.

## Decisions

Gold authoring is independent of the evaluated judge: derive labels from explicit synthetic user instructions and effects, never model outputs. Corpus revision changes whenever cases or labels change. No production security or compatibility boundary changes; no new ADR is necessary. Sandbox platform execution tests are unchanged because this work only measures outcomes. Avoid resolving #545 hook authority or implementing #315 trusted context.

## Surprises & Discoveries

The current benchmark already retains per-attempt transient and semantic failure classes, token usage and model identity. Reuse this evidence instead of creating another provider harness.

## Decision Log

- 2026-09-13: Preserve legacy mode; explicitly select independent mode. Unsupported inputs remain evaluation metadata and are never laundered into the judge request.

## Outcomes & Retrospective

Delivered src/clients/judge_independent_tests.rs, independent-v1.jsonl, provider-v1.jsonl, the benchmark selector, and corpus/benchmark documentation. All labels were authored independently of the evaluated judge. The current request projection is intentionally incomplete: direct authorization accuracy is null, the decision proxy is separately named, and #545 remains unscored. Live evaluation was not run because no provider/spend authorization was supplied.

Validation passed: just judge-corpus-check (13 passed, one live test ignored), just judge-bench-check (29 passed, one live test ignored), just check (1466 unit tests plus 105 integration tests passed; two live tests ignored), just cc-check, targeted panache format --check and panache lint, and git diff --check. The first local mock-server run could not bind ports under the execution sandbox; rerunning with local-port access passed. Initial compilation/Clippy/import issues were corrected before the successful final gate. Linux CI and the broader just check-full suite were not run locally; there are no production or platform-boundary changes.

Three local preflight passes checked root AGENTS.md (no nested files), #314 and linked cases, this plan, ARCHITECTURE.md, CONTRIBUTING.md, docs/security.md, complexity targets, and ADR-018. Rules pass confirmed test-only scope and documentation. Correctness pass fixed rubric capture timing, retry-inclusive latency and import style, and added report/contradiction tests. Simplification pass retained the existing provider and telemetry harness rather than a parallel implementation. No production context packets or policy changes were added.

## Idempotence and Recovery

Offline tests and benchmark report generation can be repeated. Live runs incur spend each time. Failed validation is corrected on this branch without changing production policy or regenerating legacy expectations.

## Artifacts and Notes

Issue #314 contains the acceptance scope. Historical baseline revision: f8fd404. Exact proposed live command: CAKE_JUDGE_BENCH_CORPUS=independent just judge-bench. Without a model override, independent mode uses the effective configured judge model. No live provider calls have been made.

## Interfaces and Dependencies

Use existing serde schema types, JudgeRequest, JudgeEvaluation and benchmark trial aggregation. Add only test modules and fixtures plus the contributor documentation; no new dependencies or production request fields.

Revision note (2026-09-13): recorded final implementation, limitations, verification and preflight outcomes before archival.
