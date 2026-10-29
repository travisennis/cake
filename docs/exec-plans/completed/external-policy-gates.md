# Extract Bash policy into an external cake-gate

This living ExecPlan is maintained per `docs/workflow/exec-plans.md`. Planning task: ahm 128a. Platform tracker: ahm 128. ADR 049 defines `cake-gate/1`; ADR 051 revises the extraction sequence. Cross-project handoff: `~/Projects/cake/initiatives/cake-gate/README.md`.

## Purpose / Big Picture

Cake will enforce Bash policy through the external gate interface and contain no in-process judge policy, provider client, rubric, or judge evaluation suite. Users can inspect configured gates with `cake gates list` and `cake gates doctor`, and use `cake bash check --json -- '<command>'` to see the result without executing the command. Offline tests prove protocol and enforcement behavior without a provider.

Extraction does not qualify the external judge's policy quality. Corpus parity, mandatory shadow comparison, live comparison, and one-release built-in availability are not prerequisites; shadow remains an optional protocol capability. Trav's external judge remains explicitly disabled, and this work makes no live or provider calls.

## Progress

Implementation completed and committed as `57a40d6` on `feat/external-gate-extraction` on 2026-10-29. No provider call, install, or user-settings change was made.

- [x] (2026-10-25) Freeze the `cake-gate/1` contract, schema, configuration/security documentation, and Cake gate seam with offline fake-gate coverage (slices 0--1; see ADR 049).
- [x] (2026-10-29) Decide extraction policy: remove parity, mandatory shadow, and one-release built-in prerequisites; retain no-gate disabling and explicit disablement (ADR 051).
- [x] (2026-10-29) Wire external gate configuration, lifecycle, diagnostics, CLI, and migration behavior in Cake (task 128e).
- [x] (2026-10-29) Delete in-process judge policy, telemetry implementation, corpus/evals, and benchmark paths from Cake; delegate `bash check` to configured gates (task 128f).
- [x] (2026-10-29) Pass the full local validation suite, including coverage/CRAP, dependency advisories, documentation, and a release build.
- [ ] Complete the standalone judge quality and enablement work as a separate plugin task; it is not an extraction gate.

## Surprises & Discoveries

- `[tools.bash.judge] enabled = false` is a supported explicit project setting that removes the judge. Migration must preserve that intent.
- `CAKE_JUDGE=off` is an explicit disablement that must remain effective through migration.
- Cake already has a generic shadow capability. Removing the rollout prerequisite does not require removing that capability.

## Decision Log

- Decision: `cake-gate/1` owns the protocol; Cake owns enforcement, context assembly, process lifecycle, composition, bypass, and telemetry. Date/Author: 2026-10-24, Trav (ADR 049).
- Decision: no enforce gate for `pre_tool` `bash.exec` means remove Bash and `BashSession`; only an explicit recorded bypass may override it. Date/Author: 2026-10-24, Trav (ADR 049).
- Decision: extraction does not wait on parity, mandatory shadow, or a one-release built-in runtime. Preserve explicit disabled values or fail with an actionable settings error; never silently enable. Date/Author: 2026-10-29, Trav (ADR 051).
- Decision: generic shadow mode remains optional; extraction checks are offline and make no provider calls. Date/Author: 2026-10-29, Trav (ADR 051).

## Outcomes & Retrospective

The Cake-side extraction is complete. An explicit `[tools.bash].gates` pipeline or the implicit `cake-safety-judge` safety gate now drives Bash preflight and `bash check`; an empty, off-only, shadow-only, or failed pipeline removes Bash and BashSession unless an explicit bypass is recorded. Legacy `[tools.bash.judge]` values are migration inputs only, and `enabled = false`/`CAKE_JUDGE=off` remain bypasses. Cake no longer contains the in-process judge, observer, rubric, TypeSafe client, corpus, or benchmark recipes. Session metrics read the generic `gate_startup`/`gate_decision` records directly and keep historical `judge_attempt`/`type_safe_shadow` readers for old sidecars. Offline tests prove allow, warn, block, failure modes, explicit disablement, no-gate removal, descriptor credential names, and oneshot/stdio lifecycles without a provider. The separate `cake-safety-judge` project remains responsible for policy quality and any live evaluation.

Verification on 2026-10-29: `just check-full` passed end to end: the fast gate (1,733 unit tests plus integration and script fixtures), Linux compatibility check, coverage and CRAP gates at 95.34% total coverage with no exceedances, `cargo deny check advisories`, rustdoc with warnings denied, `just docs-check`, and a release build. Focused session-metrics tests passed (165), and no provider call, install, settings change, or live evaluation occurred. The two known timing-sensitive agent telemetry tests were stabilized by making their raw test servers read the complete request and by bounding retries in the body-timeout case; they now pass repeatedly in the full suite.

## Context and Orientation

A gate is a trusted executable that answers whether a pre-tool action may proceed, should proceed with a warning, must stop, or is outside its policy. The v1 action is `bash.exec`. `enforce` decisions affect authorization; `shadow` records a would-be decision without authority. NDJSON is one JSON object per line over standard input/output. Cake assembles trusted context and enforces decisions; the gate owns policy and any model calls.

`src/gates/` contains Cake's protocol, runner, composer, context, and configuration-driven runtime. `src/clients/tools/bash.rs` invokes the preflight. `src/cli/bash.rs` implements `cake bash check`; `src/session_telemetry.rs` writes generic gate records and the compatibility `judge_*` events. `src/config/settings.rs` retains legacy judge settings only for migration into an external gate. The protocol and config authorities are `docs/integrations.md`, `docs/configuration.md`, `docs/security.md`, and `docs/schemas/cake-gate-1.schema.json`.

The deleted Cake policy material was `src/clients/judge.rs`, `judge_observer.rs`, `judge_rubric.rs`, `judge_rubric.md`, `typesafe.rs`, their tests and snapshot, `src/gates/judge_gate.rs`, `src/clients/tools/corpus/`, `scripts/judge-bench/`, and the judge corpus/benchmark recipes. The standalone `cake-safety-judge` project owns the rubric and quality work. Cross-project implementation still follows the workspace rule: each task edits one repository.

## Plan of Work

### Milestone 1: wire the external gate in Cake

Implement the `[gates]` settings path, process lifecycle, phase/action coverage, composition, `cake gates list`, and `cake gates doctor`. Resolve explicit gate configuration and legacy settings without changing the no-gate contract. Preserve configured legacy values where they map to the external form. If a value cannot be represented, return a settings error that names it and the migration action. In particular, `[tools.bash.judge] enabled = false` and `CAKE_JUDGE=off` must remain disabling signals; migration must not auto-detect or configure an enabled external judge over them. No enforce gate means Bash and `BashSession` are absent unless an explicit, visible bypass was selected.

Run offline fake-gate tests and Cake's focused configuration, CLI, Bash, and gate checks. Acceptance: list/doctor report resolution and coverage; the fake gate demonstrates allow/warn/block and configured failure behavior; disabled settings remain disabled; absent enforcement removes Bash; no provider is contacted.

### Milestone 2: remove built-in policy and evaluations

In task 128f, delete Cake's in-process judge, TypeSafe provider path, rubric copy, judge-specific tests, corpus/evaluation code, and recipes that run those evaluations. Keep the generic gate protocol, runner, composer, telemetry, and optional shadow behavior. Make `cake bash check` run the configured pipeline and preserve its documented document fields and exit classes. Update docs and session analysis so current gate records are authoritative while old sidecars remain readable where required.

Run offline protocol/enforcement tests, the focused Bash and CLI suites, and the project's required check suite. Acceptance: Cake builds and tests without in-process judge symbols; configured external decisions drive preflight and `bash check`; disabled or missing enforce gates cannot silently enable Bash; no provider call is required for verification.

### Milestone 3: independent judge quality enablement

Track policy quality, corpus comparison, rollout, and any desired provider-backed evaluation as a separate task in the plugin repository. This milestone is not a prerequisite to complete or delete Cake's in-process implementation. Any live/provider-backed run requires its own explicit authorization. Trav's personal external judge configuration stays disabled unless separately changed by Trav.

## Concrete Steps

All Cake commands run from `/Users/travisennis/Projects/cake/cake`. During implementation, use the focused offline suites required by the code touched, then run `just check` before task completion. Use a fake gate fixture or a deterministic protocol fixture; do not invoke a real provider.

Expected behavior for a healthy configured gate: `cake gates list` shows the resolved pipeline, `cake gates doctor` exits 0 and lists phase/action coverage, and `cake bash check --json -- '<command>'` reports the decision without running the command. For explicit disablement, doctor/list report disabled state and the command registry has no Bash tools unless an explicit bypass is active. For no gate, startup says Bash is disabled and names the configuration remedy.

For migration failure, the settings error must identify the unsupported key/value and an actionable explicit choice. It must not substitute an enabled gate. Re-running offline tests is safe; there are no corpus mutations or provider calls in this plan.

## Validation and Acceptance

The migration is accepted when offline tests demonstrate all of the following: valid protocol frames are accepted and malformed or failed gate processes follow the configured failure mode; allow, warn, and block propagate through both Bash preflight and `bash check`; explicit legacy disablement is preserved; unmappable legacy settings produce an explicit error; no enforce gate removes Bash and `BashSession`; explicit bypass remains visible and recorded; and no model provider is contacted.

Before deletion, review these bypass and lifecycle classes against the implementation and add offline tests at the narrow boundary where each applies: migration losing an explicit disablement; an empty, off, or shadow-only pipeline treated as enforcement; a startup failure silently dropping a configured gate; a per-check failure override changing fail-closed behavior; undeclared environment variables or credential values crossing to the child; request/response ID mismatch; script evidence collection skipped or bypassed; and direct, resumed, or forked session setup omitting the configured gate. Legacy `[tools.bash.judge] enabled = false` is an explicit judge bypass, preserving Bash use under its sandbox. An absent, broken, empty, off-only, or shadow-only gate pipeline is not a bypass and must never authorize Bash.

The extraction is accepted when the required Cake checks pass after deleting the built-in judge modules, no production code constructs an in-process judge or TypeSafe client, and no Cake-owned policy corpus/evaluation path remains. Quality parity, shadow comparison, latency/cost reports, and live evaluation are explicitly outside these acceptance criteria.

## Idempotence and Recovery

Offline test runs are safe to repeat. Keep deletion in the same implementation task as the delegating gate path so each commit can build and the final diff is reviewable. If a legacy value cannot be represented, keep it disabled and surface a settings error until the user chooses an explicit migration; never repair by enabling a default. If an implementation regression appears, restore source from git and retain the no-gate safety rule. No session records or user settings should be rewritten in place by tests.

## Artifacts and Notes

The default no-gate diagnostic must name the absent enforce coverage and point to `cake gates doctor` and the `[gates]` configuration. Gate telemetry must include the instance name, mode, decision or failure class, and call-id digest; it must not contain the command, secrets, or unbounded gate text.

## Interfaces and Dependencies

The stable Cake boundary remains `crate::gates::protocol`, `crate::gates::runner::GateProcess`, and `crate::gates::composer::GatePipeline`. The tool layer consumes decisions, annotations, and failure classes; it does not consume rubric or provider types. Configuration resolution belongs in `crate::config` and CLI diagnostics in `crate::cli::gates`. The gate process receives only Cake-assembled action data and explicitly named environment variables; credential values are never placed in protocol frames.

The protocol source of truth remains `docs/schemas/cake-gate-1.schema.json`. No new Cake dependency or live provider is required to meet extraction acceptance.

## Revision notes

2026-10-29: ADR 051 replaced parity, mandatory shadow comparison, and one-release built-in runtime as extraction prerequisites. The plan now ends Cake work with external wiring and in-process deletion, while judge-quality enablement is separate. Offline protocol/enforcement checks are the acceptance evidence; explicit disablement and the no-gate rule remain mandatory.
