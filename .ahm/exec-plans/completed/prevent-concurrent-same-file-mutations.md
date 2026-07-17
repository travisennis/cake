# Prevent Concurrent Same-File Mutating Tool Calls

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds. This document follows `.agents/PLANS.md`.

## Purpose / Big Picture

After this change, cake will no longer allow two mutating tool calls in the same assistant turn to edit or overwrite the same file at the same time. A user can observe the behavior through tests that construct same-turn `Edit` and `Write` calls: the first same-path mutation remains eligible to run, and later same-path mutations receive a deterministic tool result telling the model to re-read the file and retry sequentially. Reads and mutations to distinct files still remain parallelizable.

## Progress

- [x] (2026-05-29T22:06Z) Read task 163, `.agents/TASKS.md`, `.agents/PLANS.md`, and the current agent/tool execution code.
- [x] (2026-05-29T22:06Z) Started task 163 with `ahm task start 163`.
- [x] (2026-05-29T22:20Z) Added path-target detection for `Edit` and `Write` through `ToolRegistry::mutating_target`.
- [x] (2026-05-29T22:20Z) Added same-turn duplicate mutation rejection in the agent execution scheduler after pre-tool hooks and before concurrent execution.
- [x] (2026-05-29T22:20Z) Added tests for duplicate `Edit`/`Edit`, duplicate `Edit`/`Write`, distinct-file mutations, and non-mutating repeated `Read` calls.
- [x] (2026-05-29T22:35Z) Deslop review added coverage for relative and absolute spellings of the same canonical file.
- [x] (2026-05-29T22:20Z) Ran `cargo check --tests`, `cargo fmt`, `just coverage-check`, and the non-index CI component recipes.
- [x] (2026-05-29T22:21Z) Completed task 163 acceptance notes and moved this ExecPlan to completed storage.

## Surprises & Discoveries

- Observation: `Agent::send` currently runs pre-tool hooks concurrently, then maps each allowed tool plan into a future and awaits `futures::future::join_all`, so result order is deterministic but execution of same-file `Edit` and `Write` calls can overlap.
  Evidence: `src/clients/agent.rs` builds `futures` from `tool_plans.into_iter()` and then awaits `join_all(futures)`.

- Observation: `just ci` currently stops at `task-index-check` even after running `ahm index` and `ahm --force index`; `ahm --dry-run index` reports all generated index files.
  Evidence: `just ci` prints `Task indexes are stale. Regenerate with ahm index.` followed by `.agents/.research/index.md`, task indexes, and ExecPlan indexes. The remaining CI recipes pass when run directly.

## Decision Log

- Decision: Reject the second and later same-turn mutating tool calls targeting a canonical equivalent path instead of serializing them.
  Rationale: Rejection preserves parallel execution for safe independent calls, makes the model's invalid batch visible in the transcript, and forces the model to re-read changed state before issuing a follow-up edit.
  Date/Author: 2026-05-29 / Codex

- Decision: Detect duplicate mutation targets after pre-tool hooks have run.
  Rationale: Hooks can block or rewrite a tool call's arguments. The guard should apply to the actual arguments that would be executed, and hook-blocked calls do not mutate files.
  Date/Author: 2026-05-29 / Codex

## Outcomes & Retrospective

Implemented the duplicate mutation guard. `Edit` and `Write` now report their canonical mutation target to the agent scheduler, and the scheduler rejects second and later same-turn mutations to the same path with an actionable tool output that tells the model to wait, re-read, and retry sequentially. The guard runs after pre-tool hooks so rewritten arguments are evaluated and hook-blocked calls are left alone. Focused tests cover the required duplicate and non-duplicate cases, including relative and absolute spellings of the same canonical file. Full `just ci` is blocked by the generated-index freshness check described above, but `cargo check --tests`, `cargo fmt`, `just clippy-strict`, `just test`, `just lint-imports`, `just lint-module-size`, `just rust-version-check`, and `just coverage-check` pass.

## Context and Orientation

The agent loop lives in `src/clients/agent.rs`. `Agent::send` sends a user message to the selected backend, receives typed `ConversationItem` values, persists function calls to conversation history, runs pre-tool hooks, executes allowed tool calls, and appends `FunctionCallOutput` items in the same order as the model's function calls.

The tool registry lives in `src/clients/tools/mod.rs`. It defines `ToolRegistry`, `ToolEntry`, and each default tool's executor. `Edit` is implemented in `src/clients/tools/edit.rs`; it mutates exactly one existing file identified by the JSON `path` argument. `Write` is implemented in `src/clients/tools/write.rs`; it creates or overwrites exactly one file identified by the JSON `path` argument. `Read` and `Bash` are not classified by this task as same-file mutations, even though `Bash` can run arbitrary commands, because this task is scoped to the model-facing `Edit` and `Write` tools already described in the prompt.

A "same assistant turn" means one backend response that contains multiple `ConversationItem::FunctionCall` items before the agent sends any tool outputs back to the model. A "canonical path" means a normalized filesystem path after resolving existing parent directories and symlinks where possible, so relative and absolute spellings of the same target are treated as the same file.

## Plan of Work

First, extend the tool layer with a small path-target API. Add a `MutatingToolTarget` or equivalent helper in `src/clients/tools/mod.rs` and a `ToolRegistry` method that returns `Some(Ok(path))` for `Edit` and `Write`, `Some(Err(message))` when their arguments are invalid enough that the target cannot be determined, and `None` for non-mutating tools. Reuse the existing argument structs and validation rules where possible. For `Write`, expose a side-effect-free helper that resolves non-existent files by validating their deepest existing parent, matching the write tool's existing behavior without creating directories.

Second, update `Agent::send` after pre-tool hook results are collected and before execution futures are built. Iterate over the ordered plans and remember the first mutating canonical path seen in that turn. If a later executable `Edit` or `Write` targets the same path, replace that execution with a synthetic tool result whose output starts with `Error:` and explains that the tool call was rejected because another `Edit` or `Write` for the same file was already issued in this assistant turn. Include the path and tell the model to wait for this result, re-read the file, and issue one follow-up mutation if needed. Keep blocked hook results unchanged. If the target cannot be determined due to malformed arguments, let the tool execute normally so the existing tool-specific validation error remains authoritative.

Third, add focused tests. Unit tests should exercise the scheduling helper directly so they do not need live HTTP calls. The tests must prove that duplicate `Edit` calls to the same file reject the second call, an `Edit` plus `Write` targeting the same file rejects the second call, mutations to different files remain executable, and `Read` calls to the same file are not rejected. Where useful, add a tool-registry test that proves `Write` resolves an existing file and a new file under an existing directory to a stable canonical target.

Finally, run project validation in this repository root. Use `cargo check --tests` before relying on normal builds because this change touches test-only code. Run `cargo fmt`, `just coverage-check`, and `just ci` before final handoff. If `just ci` cannot run, record the exact failure and the narrower commands that passed.

## Concrete Steps

Work from `/Users/travisennis/Projects/cake`.

1. Update `src/clients/tools/mod.rs`, `src/clients/tools/edit.rs`, and `src/clients/tools/write.rs` with mutating target detection.
2. Update `src/clients/agent.rs` to classify same-turn tool plans before building execution futures.
3. Add or update tests in `src/clients/agent.rs` and, if needed, `src/clients/tools/mod.rs`.
4. Run:

        cargo check --tests
        cargo fmt
        just coverage-check
        just ci

Observed result: `cargo check --tests`, `cargo fmt`, `just coverage-check`, and the non-index `just ci` components pass. `just ci` itself stops at `task-index-check` because `ahm --dry-run index` reports generated indexes stale immediately after regeneration.

## Validation and Acceptance

Acceptance is met when a same assistant turn with two `Edit` calls for the same canonical path produces an executable first call and a rejected second call, a same assistant turn with `Edit` and `Write` for the same canonical path also rejects the second call, same-turn mutations to different canonical paths are not rejected by this guard, and repeated `Read` calls for the same path are not rejected. The rejection output must be deterministic and actionable: it must name the same file, say that another `Edit` or `Write` for that file was already issued in this assistant turn, and instruct the model to re-read before trying another mutation.

The final verification commands are `cargo check --tests`, `cargo fmt`, `just coverage-check`, and `just ci`.

## Idempotence and Recovery

The code changes are additive and can be reapplied safely with normal source control review. If path detection rejects too broadly, inspect the helper tests first and compare the canonical paths being recorded. If `ahm index` is needed after task metadata edits, run it instead of editing generated index files by hand.

## Artifacts and Notes

The initial relevant execution code in `src/clients/agent.rs` uses concurrent execution:

        let futures = tool_plans.into_iter().map(|(call_id, name, plan)| { ... });
        let results = futures::future::join_all(futures).await;

The fix belongs between `tool_plans` construction and this `join_all` call.

## Interfaces and Dependencies

Keep using the existing Rust standard library path APIs and the existing `serde_json` argument parsing already used by the tools. No new runtime dependencies are needed.

The final tool-layer interface should be crate-private or narrower, such as:

        pub(super) fn mutating_target(
            &self,
            context: &ToolContext,
            name: &str,
            arguments: &str,
        ) -> Option<Result<PathBuf, String>>;

The final agent-layer helper should be testable without network access and preserve the input order of tool plans.

Revision note 2026-05-29: Created this ExecPlan before implementation because task 163 is effort L and changes agent-loop/tool-execution behavior.

Revision note 2026-05-29: Updated progress, discoveries, and outcomes after implementing the guard and running validation.

Revision note 2026-05-29: Moved this ExecPlan from active to completed because the implementation and retrospective are complete.

Revision note 2026-05-29: Deslop review added a canonical-path spelling test and updated the retrospective to mention it.
