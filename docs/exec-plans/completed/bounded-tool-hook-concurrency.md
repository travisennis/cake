# Bound per-turn tool and hook concurrency

This ExecPlan is a living document, maintained per `docs/workflow/exec-plans.md`, for issue #90.

## Purpose / Big Picture

A model can return a pathological tool-call batch. Before this change, Cake started every pre-tool hook, every scheduling group, and every hook subprocess at once, which could create unbounded child processes, pipes, sandbox profiles, output buffers, and judge requests. After this change, Cake still executes every normally completed call once and returns results in the model's issue order, while limiting each fan-out surface to eight active operations. The limit is a fixed defense-in-depth bound rather than a user setting or a change to the model-visible scheduling contract.

## Progress

- [x] (2026-09-01) Read `AGENTS.md`, the task workflow, architecture guidance, issue #90, and the relevant agent, tool-scheduling, and hook implementations.
- [x] (2026-09-01) Attempt to claim issue #90; the claim command failed closed because the command-safety judge returned a malformed empty response, and the required task branch was created.
- [x] (2026-09-01) Add the shared fixed concurrency constant, bound pre-tool planning, bound scheduling groups, and bound hook subprocesses across cloned runners and hook event types.
- [x] (2026-09-01) Add deterministic agent and hook regression tests covering peak concurrency, exactly-once completion, event coverage, and result ordering.
- [x] (2026-09-01) Update `ARCHITECTURE.md` with the resource-bound invariant.
- [x] (2026-09-01) Run focused tests, formatting, strict Clippy, and the fast check gate. The fast gate's all-feature test phase is blocked by the host sandbox on four pre-existing tests that create temporary directories beside this clone.
- [x] (2026-09-01) Complete the implementation and verification record, then open pull request #426 with `Closes #90`; the issue remains open pending review.

## Surprises & Discoveries

- Observation: The repository has no `just ci` recipe even though the migrated issue's acceptance notes name that command. Evidence: `justfile` defines `just check` and `just check-full`; the documented contributor gate for Rust changes is `just check`.
- Observation: `just check` reached formatting, both strict Clippy modes, and compilation, then its all-feature tests failed in four existing Bash tests when `tempfile` created `/Users/travisennis/Projects/cake/.tmp*` outside the sandboxed clone. Evidence: the failures report `PermissionDenied`, `Operation not permitted`, at `src/clients/tools/bash_tests.rs:1412`, `1428`, `1480`, and `1509`. Focused agent and hook tests pass.

## Decision Log

- Decision: Use one shared `MAX_CONCURRENT_AGENT_OPERATIONS` constant with value 8. Rationale: the issue fixes the value, eight preserves the largest ordinary local batch, and a shared source prevents the tool and hook bounds from drifting. Date/Author: 2026-09-01 / Codex.
- Decision: Use `buffer_unordered(8)` for pre-tool planning and scheduling groups, then restore issue order before consumers observe results. Rationale: this polls only eight queued futures at a time, lets later groups use freed slots even when an earlier group is slow, and preserves the existing same-path sequential group semantics. Date/Author: 2026-09-01 / Codex.
- Decision: Give each `HookRunner` an `Arc<Semaphore>` with eight permits and retain a bounded stream per aggregation call. Rationale: the shared semaphore limits hook subprocesses across concurrent tool calls, runner clones, and `PreToolUse`, `PostToolUse`, and `PostToolUseFailure`; the permit is held only while the subprocess runs, so a tool never holds a permit while waiting for its own post-tool hook. Date/Author: 2026-09-01 / Codex.
- Decision: Do not add a setting, CLI flag, dependency, model-visible field, or global tool semaphore. Rationale: these are explicitly outside issue #90, and the existing future cancellation and process guards remain responsible for dropping queued work and terminating active child processes. Date/Author: 2026-09-01 / Codex.

## Outcomes & Retrospective

The agent loop now bounds pre-tool planning and independent scheduling groups at eight active futures. Same-path Edit/Write calls remain sequential in issue order, independent groups can make progress as slots free, and results are sorted back to model issue order. Hook subprocesses use a shared eight-permit semaphore across runner clones and all tool lifecycle events, with aggregation still performed in hook load order. The new deterministic tests demonstrate twelve independent toolbox calls completing exactly once with ordered outputs and demonstrate bounded execution for all three tool-hook event categories.

Focused tests and strict Clippy pass. The repository fast gate is not fully green in this environment because its all-feature test phase cannot access the parent directory used by four existing sandbox-sensitive tests; the failure is environmental and must be rerun from a host context that permits that path. No provider, platform sandbox, or external network verification was required for this change.

## Context and Orientation

Cake is a Rust binary crate. `src/clients/agent/agent_loop.rs` turns provider function calls into pre-tool plans, groups calls by canonical mutating path using `src/clients/tools/scheduling.rs`, executes groups, and records outputs in issue order. `src/hooks.rs` runs configured hook commands around tool lifecycle events and aggregates their decisions in configured load order. `HookRunner` is cloned or shared by concurrent tool-call futures. `futures::stream::buffer_unordered` limits how many futures are polled concurrently while allowing completed work to release a slot for later work. A Tokio semaphore supplies the cross-call hook subprocess bound.

The relevant compatibility rules are that same-path Edit/Write calls execute sequentially, unrelated calls remain concurrent, normal calls are not discarded, result collection is restored to model issue order, hook aggregation keeps load order, and cancellation must not start futures that remain queued. `ARCHITECTURE.md` records these boundaries.

## Plan of Work

`src/concurrency.rs` defines the fixed value and its resource rationale. `src/main.rs` registers the module. In `src/clients/agent/agent_loop.rs`, enumerate pre-tool plans, run no more than eight planning futures, sort completed plans by original index, and replace unbounded group `join_all` with an eight-wide unordered stream followed by existing result sorting. In `src/hooks.rs`, add a semaphore shared by `HookRunner` clones, acquire a permit only around `run_command_hook`, bound the per-aggregation stream to eight, and sort completed outcomes by hook load index before recording and aggregating them. `ARCHITECTURE.md` records the fixed resource invariant. The focused tests in `src/clients/agent/agent_tests.rs` and `src/hooks_tests.rs` exercise tool and hook behavior without changing any public protocol.

## Concrete Steps

From the repository root, inspect issue #90 and the current scheduling and hook code, then run the focused tests:

```
cargo test agent
cargo test hooks
cargo test independent_tool_batch_is_bounded_and_results_keep_issue_order -- --nocapture
cargo test hook_subprocesses_are_bounded_across_events_and_runner_clones -- --nocapture
```

Format and run the Rust gate:

```
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
just check
```

The focused tests should pass. `just check` should report formatting and Clippy success; in this checkout its all-feature test phase reaches 1394 passing tests and fails only the four documented sandbox-sensitive tests until run outside the restricted host context.

## Validation and Acceptance

A successful run of the new agent regression sends twelve independent toolbox calls. The test observes a maximum active-tool counter no greater than 8, sees twelve unique completed indices, and observes twelve function-call outputs in `call-0` through `call-11` order. The hook regression concurrently invokes twelve pre-tool, twelve post-success, and twelve post-failure calls across runner clones; it observes no more than eight active hook subprocesses and exactly 36 completions. Existing agent, hook, same-path scheduling, and provider tests must remain green.

No new configuration key, CLI flag, dependency, hook payload field, or model-visible protocol behavior is introduced. Queued futures remain unpolled until a slot becomes available, and dropping the enclosing future drops active tool or hook child guards using the existing cancellation behavior.

## Idempotence and Recovery

The source and test changes are safe to rerun. Focused Cargo tests create and remove their own temporary directories. `cargo fmt`, Clippy, and `just check` are read-only apart from normal build artifacts. If `just check` is run under the restricted host and fails at the known parent-directory tests, rerun it from an unrestricted host context rather than weakening the sandbox-sensitive tests or changing the resource-bound implementation.

## Artifacts and Notes

The implementation and regression tests are the proof artifacts. The fixed value and rationale are in `src/concurrency.rs`; the architecture invariant is in `ARCHITECTURE.md`. The issue's migrated acceptance notes remain the source for the exact fixed value, scope, and required compatibility behavior.

## Interfaces and Dependencies

The change uses the existing `futures` dependency and its `StreamExt::buffer_unordered` combinator, already used elsewhere in the repository. It uses `tokio::sync::Semaphore`, included by the existing full Tokio feature set. The stable internal interfaces remain `Agent::plan_tool_calls`, `Agent::run_tool_plans`, `HookRunner::run_and_aggregate`, `schedule_tool_calls`, `ToolHookPlan`, and the existing hook payload and result types. No external service, persisted record, CLI, or configuration interface changes.
