# Refactor CodingAssistant Run Into Smaller Steps

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds. This plan follows `.agents/PLANS.md`.

## Purpose / Big Picture

`cake` starts every CLI invocation through `CodingAssistant::run` in `src/main.rs`. That method currently mixes environment setup, prompt input resolution, settings loading, skill discovery, session persistence, hook setup, progress rendering, agent execution, and final output formatting in one long function that needs `#[allow(clippy::too_many_lines)]`. After this refactor, the same user-visible CLI behavior remains, but the top-level run method reads as a short sequence of named steps. The observable proof is that the existing tests and full `just ci` command still pass, and clippy no longer needs a too-many-lines allow on `CodingAssistant::run`.

## Progress

- [x] (2026-05-09 00:00Z) Read `.agents/TASKS.md`, `.agents/.tasks/index.md`, task `054.md`, `.agents/PLANS.md`, and the current `src/main.rs` implementation.
- [x] (2026-05-09 00:00Z) Created this active ExecPlan and linked task `054.md` to it.
- [x] (2026-05-09 00:00Z) Refactored `CodingAssistant::run` into named helper steps while preserving behavior.
- [x] (2026-05-09 00:00Z) Ran focused test command `cargo test main`; it passed, though the filter only selected one matching test.
- [x] (2026-05-09 00:00Z) Ran `just ci`; it passed with 503 unit tests, 12 exit-code tests, 8 stdin tests, clippy, format, toolchain, and import-lint checks.
- [x] (2026-05-09 00:00Z) Updated task/index metadata and moved this ExecPlan to completed with retrospective notes.

## Surprises & Discoveries

- Observation: The session construction logic is already mostly extracted into `build_client_and_session`, `new_client_and_session`, `restored_client_and_session`, and `forked_client_and_session`.
  Evidence: `src/main.rs` has helper definitions before the `CmdRunner` implementation, so the remaining long method is primarily orchestration rather than complex business logic.
- Observation: `cargo test main` is not a useful focused test for the `main.rs` module in this repository because it filters by test name rather than file.
  Evidence: The command passed but selected only `clients::provider_strategy::tests::openrouter_detection_accepts_subdomains`, with 502 unit tests filtered out.

## Decision Log

- Decision: Keep the refactor scoped to `src/main.rs` and avoid moving code into new modules.
  Rationale: Task `054.md` names `src/main.rs` as the file involved and asks to extract typed steps from `CodingAssistant::run`, not to reorganize module ownership.
  Date/Author: 2026-05-09 / Codex.
- Decision: Preserve existing callback and output ordering while extracting helpers.
  Rationale: The CLI's behavior depends on persistence callbacks being installed before hooks, stream JSON, progress callbacks, and agent execution. The refactor should not change session files, hook records, or printed output.
  Date/Author: 2026-05-09 / Codex.

## Outcomes & Retrospective

Completed. `CodingAssistant::run` no longer carries `#[allow(clippy::too_many_lines)]` and now reads as a short orchestration method: prepare the run, load resources, build the session, attach persistence and hooks, attach output callbacks, execute the turn, render output, and clean up any worktree. The refactor added private state structs for the prepared run, loaded resources, and turn result so helper signatures stayed readable. Full validation passed with `just ci`.

## Context and Orientation

The binary entry point lives in `src/main.rs`. `CodingAssistant` is the clap-parsed CLI struct. The `CmdRunner` trait is implemented for `CodingAssistant`, and `CodingAssistant::run` is the async method invoked by `main()` after logger and `DataDir` setup.

`DataDir` owns cache/session paths and session file creation. `SettingsLoader` reads global and project `.cake/settings.toml` files. `SkillCatalog` records discovered skills and diagnostics. `ToolContext` carries directories that tools may access. `Agent` owns the model conversation loop and supports callbacks for persistence, hooks, progress, retry status, and stream JSON output. `HookRunner` executes configured hooks around a session and prompt lifecycle.

The current `CodingAssistant::run` performs these steps inline: save startup directory, resolve extra sandbox directories, optionally create a git worktree, read prompt/stdin content, load settings, read AGENTS.md files, resolve and discover skills, build a `ToolContext`, log skill diagnostics, choose a `RunMode`, build an `Agent` and `Session`, attach persistence, load hooks, attach stream/progress callbacks, emit start records, run hooks, send the user message, emit completion records, print text or JSON output, and clean up the worktree.

## Plan of Work

First, add small private structs in `src/main.rs` near `RunSession` to name the intermediate state passed between steps. A `PreparedRun` should hold the original directory, effective current directory after optional worktree setup, resolved prompt content, extra directories from `--add-dir`, and optional worktree handle. A skill/setup struct should hold loaded settings, AGENTS.md files, the filtered skill catalog, and the `Arc<ToolContext>`.

Next, add helper methods on `CodingAssistant` for the orchestration chunks currently embedded in `run`: prepare environment and input, load settings plus skills, attach persistence, attach hooks, attach output/progress callbacks, execute the agent turn with hooks and completion records, and render final output.

Then rewrite `CodingAssistant::run` as a short sequence that calls those helpers. The method should still clean up the worktree at the end and return the same errors as before. If a helper must mutate the `Agent`, pass and return `Agent` explicitly or take `&mut Agent` depending on which keeps ownership clear. Keep the async hook and send flow in a helper because it is the densest part of the method.

Finally, remove `#[allow(clippy::too_many_lines)]` from `CodingAssistant::run`. Keep the existing `#[allow(clippy::too_many_arguments)]` on `build_client_and_session` unless the refactor naturally removes the need; task `054` is about run length, not all existing clippy exceptions.

## Concrete Steps

Work from the repository root, `/Users/travisennis/Projects/cake`.

Run a focused test pass after the code edit:

    cargo test main

Run the required full project check before completion:

    just ci

If clippy reports the extracted helpers have new too-many-arguments or type-complexity warnings, introduce a small local state struct rather than adding new allows unless the codebase already uses the same exception for that exact shape.

## Validation and Acceptance

Acceptance is met when `CodingAssistant::run` no longer has `#[allow(clippy::too_many_lines)]`, the method reads as named steps instead of a long inline procedure, existing CLI behavior is preserved, and `just ci` exits successfully. For user-visible behavior, text mode should still print the model response after completion, JSON mode should still print a JSON object before propagating errors, stream JSON mode should still stream callback JSON without adding a final text response, and persistent sessions should still write records through `DataDir`.

## Idempotence and Recovery

The changes are source refactors and task metadata updates only. Re-running tests and `just ci` is safe. If the refactor creates a borrow checker issue, recover by moving more state into an explicit struct instead of weakening lifetimes or cloning large values unnecessarily. Do not delete or rewrite unrelated user changes in the working tree.

## Artifacts and Notes

The original long method starts at `src/main.rs:690` before this plan's implementation and carries `#[allow(clippy::too_many_lines)]`. The nearby helpers already cover model resolution, session creation, progress formatting, and worktree setup, so the implementation should prefer reusing those helpers.

## Interfaces and Dependencies

No external dependencies are required. Use existing types from `crate::config`, `crate::clients`, and `crate::hooks`. New helper structs should stay private to `src/main.rs`.

Revision note: Created this plan because task `054` is `Effort: L` and repository policy requires an ExecPlan before implementation.

Revision note: Completed the implementation, recorded validation evidence, and prepared this plan to move from active to completed.
