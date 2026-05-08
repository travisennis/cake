# Split Agent Responsibilities

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds. This plan follows `.agents/PLANS.md`.

## Purpose / Big Picture

The `cake` CLI agent currently stores conversation history, calls the provider backend, retries HTTP failures, executes tools, runs hooks, streams JSON, persists session records, and tracks token usage from one large `src/clients/agent.rs` type. After this change, the same CLI behavior remains observable, but the responsibilities are split into smaller types so future changes can target one concern at a time. The result is demonstrated by the existing end-to-end agent loop test, focused unit tests, and the project `just ci` command.

## Progress

- [x] (2026-05-08T20:27:47Z) Read `.agents/TASKS.md`, `.agents/.tasks/index.md`, task `049.md`, `.agents/PLANS.md`, and the current `src/clients/agent.rs` shape.
- [x] (2026-05-08T20:27:47Z) Confirmed dependencies `047`, `048`, `050`, and `051` are completed in the task index.
- [x] (2026-05-08T20:28:00Z) Created the active ExecPlan and linked it from task `049.md` and `.agents/exec-plans/active/index.md`.
- [x] (2026-05-08T20:43:00Z) Extracted conversation history and usage accumulation into `src/clients/agent_state.rs` while preserving `Agent` public fields used by `src/main.rs`.
- [x] (2026-05-08T20:43:00Z) Extracted streaming, persistence, progress, and retry callbacks into `src/clients/agent_observer.rs`.
- [x] (2026-05-08T20:43:00Z) Extracted backend request and retry behavior into `src/clients/agent_runner.rs`.
- [x] (2026-05-08T20:47:00Z) Ran `cargo fmt`, `cargo test clients::agent`, and `just ci`.
- [x] (2026-05-08T20:49:00Z) Marked task `049` complete, updated the task indexes, moved this plan to completed, and prepared the commit.

## Surprises & Discoveries

- Observation: `Agent` public fields `session_id`, `task_id`, `total_usage`, and `turn_count` are read by `src/main.rs`, and tests in `src/clients/agent.rs` directly inspect private `history`.
  Evidence: `rg "\.history|\.total_usage|\.turn_count|\.session_id|\.task_id" -n src tests` shows uses in `src/main.rs` and many agent tests.

- Observation: Wiremock-backed agent tests cannot bind local ports in the default command sandbox.
  Evidence: `cargo test clients::agent` failed with `Failed to bind an OS port for a mock server: Operation not permitted`; rerunning with local-port permission passed all 40 focused tests.

- Observation: The repository's strict clippy settings require callback closure bounds to keep async futures `Send`, and prefer const helpers where possible.
  Evidence: the first `just ci` run failed on `clippy::future-not-send` and `clippy::missing-const-for-fn`; adding `Send + Sync` to the retry reporter and making usage helpers const resolved it.

## Decision Log

- Decision: Keep `Agent` as the public facade and preserve its public fields during this task.
  Rationale: The task is an internal responsibility split, and changing the CLI-facing surface would increase blast radius without making the refactor more observable.
  Date/Author: 2026-05-08 / Codex

- Decision: Extract behavior into repository-local modules under `src/clients/` instead of adding new dependencies.
  Rationale: Existing dependencies already cover HTTP, async, serialization, and tests. This refactor is about ownership boundaries, not new capability.
  Date/Author: 2026-05-08 / Codex

- Decision: Move assistant-message fallback resolution with the conversation state instead of keeping it in `src/clients/agent.rs`.
  Rationale: That logic only depends on conversation history, and moving it makes the state helper responsible for both storing and interpreting the history it owns.
  Date/Author: 2026-05-08 / Codex

## Outcomes & Retrospective

Completed on 2026-05-08. `Agent` remains the public entry point and still exposes the fields consumed by `src/main.rs`, but its major responsibilities now delegate to smaller collaborators: `ConversationState` for history and assistant-message resolution, `AgentObserver` for callback fan-out, and `AgentRunner` for backend requests and retry behavior. The focused agent tests and full CI passed after the refactor.

## Context and Orientation

The `Agent` facade lives in `src/clients/agent.rs` and is re-exported from `src/clients/mod.rs`. `src/main.rs` constructs `Agent`, configures callbacks, sends a user message, and reads `session_id`, `turn_count`, and `total_usage` for CLI summaries and JSON output. A `ConversationItem` is a typed record of messages, tool calls, tool outputs, and reasoning in `src/clients/types.rs`. A `SessionRecord` is the append-only session record shape. A `StreamRecord` is the JSON streaming shape. The provider backend abstraction lives in `src/clients/backend.rs` and dispatches to either the Responses API or Chat Completions API.

The key tests for this task are in the test modules at the bottom of `src/clients/agent.rs`. They cover usage accounting, session record streaming and persistence, tool execution with skill activation, retry behavior, and an end-to-end loop through a wiremock backend that requests a tool call and then returns a final assistant message.

## Plan of Work

First, add a `ConversationState` helper near the agent implementation or in a dedicated module. It owns the conversation history and knows how to initialize prompt messages, append developer context, merge restored history without stale prompt context, append user messages, append turn items, append tool outputs, and resolve the final assistant message. Because tests currently inspect `agent.history`, preserve a `history` field on `Agent` for now and make `ConversationState` an internal helper that centralizes mutation methods. Also add a small `UsageTracker` or equivalent helper for token accumulation, with `Agent` keeping its public `total_usage` and `turn_count` mirrors.

Next, add an observer type that owns the callback boxes for stream JSON, persistence, progress, and retry. Move `stream_record`, `persist_record`, `stream_item`, `report_progress`, and `report_retry` behind that type. `Agent` builder methods continue to be named the same, but delegate to observer setters.

Then, move `complete_turn` retry behavior into a backend runner/client type in `src/clients/agent_runner.rs` or a similarly named file. It should own the `Backend` value and reusable `reqwest::Client`. Its public method takes immutable request inputs and a retry callback closure, returns `TurnResult`, and preserves existing retry behavior including connection reuse disabling. The existing wiremock retry tests should continue to pass.

Finally, update task and plan indexes. If all validation passes, mark task `049` completed, move this plan to `.agents/exec-plans/completed/`, and commit all scoped changes with a Conventional Commit message.

## Concrete Steps

From `/Users/travisennis/Projects/cake`, run:

    cargo fmt
    cargo test clients::agent
    just ci

Expected outcomes are successful formatting, passing agent tests, and a passing full CI check.

Observed outcomes:

    cargo test clients::agent
    test result: ok. 40 passed; 0 failed; 0 ignored; 0 measured; 454 filtered out

    just ci
    test result: ok. 494 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
    test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
    test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
    Import lint passed!
    All checks passed!

## Validation and Acceptance

Acceptance requires that `cargo test clients::agent` passes and continues to prove the end-to-end agent loop: the model returns a `Read` tool call, cake executes it, includes the tool output in the next provider request, and receives the final assistant message. Acceptance also requires `just ci` to pass because this task changes code.

The task is complete when `src/clients/agent.rs` no longer directly owns every major responsibility and the task metadata says `Completed`.

## Idempotence and Recovery

The refactor is source-only and can be retried by re-running formatting and tests. If a test fails, inspect the smallest moved responsibility first: observer fan-out failures should affect stream or persist tests, state failures should affect history and usage tests, and runner failures should affect wiremock backend tests. Do not reset unrelated worktree changes; this repository may have user edits.

## Artifacts and Notes

Relevant starting facts:

    src/clients/agent.rs has 2457 lines.
    src/main.rs reads Agent session_id, total_usage, and turn_count.
    The task queue lists 049 as the next ready P1 XL task with all dependencies complete.

## Interfaces and Dependencies

The public interface remains `crate::clients::Agent`. Internally, the refactor should introduce small types with responsibilities similar to:

    struct AgentObserver { ... callbacks ... }
    struct AgentRunner { backend: Backend, client: reqwest::Client }
    struct ConversationState { history: Vec<ConversationItem> }

Any exact names may vary if the surrounding code suggests a clearer local pattern, but the final code must preserve current CLI behavior and test expectations.

## Revision Notes

- 2026-05-08 / Codex: Created the plan because task `049` is `Effort: XL` and requires an ExecPlan before implementation.
- 2026-05-08 / Codex: Updated the plan at completion with implemented files, validation evidence, surprises, and retrospective, then moved it to the completed ExecPlan index.
