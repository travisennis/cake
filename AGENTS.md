# AGENTS.md

## Project Overview

cake is an AI coding assistant CLI that:

- Is written with Rust 2024 edition with the Tokio async runtime
- Binary-only CLI. Not a library.
- Uses clap for CLI parsing, anyhow/thiserror for error, tracing for logging
- Uses reqwest + serde/serde_json for HTTP/JSON
- Integrates with LLMs through OpenAI-compatible Chat Completions and Responses API backends.
- Executes tools (Bash, Read, Edit, Write) in a sandboxed environment
- Manages conversation sessions with continue/resume/fork capabilities
- Uses OS-level sandboxing (macOS Seatbelt, Linux Landlock)

**Core mechanism**: The agent loop lets the model execute tools, receive results, and continue until it returns a final response.

--------------------------------------------------------------------------------

## Start Here

1. Read this file fully before making changes.
2. For task work, read `.agents/TASKS.md`, then `.agents/.tasks/index.md`, then the specific task file.
3. Prefer narrow checks first, then `cargo fmt`, then `just ci` before final handoff.
4. Do not commit or push unless explicitly asked.
5. Never edit generated task indexes by hand.

--------------------------------------------------------------------------------

## Required Workflow

- Before final handoff for any code, test, config, fixture, or dependency change, run `just ci`.
- If `just ci` cannot be run, state the exact reason and list the narrower checks that were run instead.
- For Rust code changes, use this verification sequence:
  1. Run the narrowest useful check first, such as `cargo check --tests`, `cargo test <module_or_test_name>`, or `cargo test`.
  2. Run `cargo fmt` after code edits.
  3. Run `just ci` before final handoff or commit.
- Run `just coverage-check` when a change adds or removes meaningful Rust code, changes tests, or touches coverage-sensitive areas. It mirrors the CI coverage gate and catches drops below the 90% project threshold before pushing.
- When changing test fixtures, test-only code, struct literals used in tests, or `#[cfg(test)]` modules, run `cargo check --tests` before relying on `cargo build` or `cargo check`. Plain `cargo build` and `cargo check` do not validate this project's test code.
- This is a binary-only crate. Do not run `cargo test --lib`; there is no library target. Use `cargo test <module_or_test_name>` for targeted tests, or `cargo test` for the full test suite.
- Do not commit or push code unless explicitly asked to.

--------------------------------------------------------------------------------

## Build/Test/Run

```bash
# Build release binary
cargo build --release

# Build and install to ~/bin
just install

# Run tests
cargo test

# Run tests for a specific module
cargo test <module_name>

# Check test code without running tests
cargo check --tests

# Run tests with coverage
just coverage

# Print coverage summary
just coverage-summary

# Check coverage against the 90% CI threshold
just coverage-check

# Run coverage and open HTML report
just coverage-open

# Formatting
cargo fmt

# Linting
just clippy-strict

# Update dependencies
just update-dependencies

# Full CI check
just ci

# Verify Rust toolchain pins are synchronized
just rust-version-check
```

--------------------------------------------------------------------------------

## Targeted Test Examples

```bash
# Good targeted test commands
cargo test responses
cargo test session::tests
cargo test test_name

# Do not use; this crate has no library target
cargo test --lib
```

--------------------------------------------------------------------------------

## Task Queue Rules

- When a task includes committing and task-status updates, commit the intended code/task changes together unless the user asks for separate commits. After committing and moving or regenerating task files, run `git status --short` before the final response and report any remaining uncommitted or untracked files.
- When asked to create, choose, update, or work on a task, first read `.agents/TASKS.md`, then use `.agents/.tasks/index.md` as the task queue and open the specific task file before acting.
- Use task labels to filter work by type, area, and risk when the user asks for focused work.
- Do not edit generated task indexes by hand; update task files and run `just task-index` (never invoke the underlying Python script directly).
- When marking a task as Completed, use `just task-complete <id>`. It updates the task front matter, moves the file from `.agents/.tasks/active/` to `.agents/.tasks/completed/`, and regenerates the indexes in one step. Do not leave Completed tasks in `active/`.
- When marking a task as Cancelled, use `just task-cancel <id>`. It updates the task front matter, moves the file from `.agents/.tasks/active/` to `.agents/.tasks/cancelled/`, and regenerates the indexes in one step. Do not leave Cancelled tasks in `active/`.

--------------------------------------------------------------------------------

## Research Rules

- When asked to create, update, organize, or use research, first read `.agents/RESEARCH.md`, then use `.agents/.research/index.md` as the research map and open the relevant research file before acting.

--------------------------------------------------------------------------------

## Git Worktree Safety

- Assume uncommitted changes may belong to the user.
- Do not revert, overwrite, or clean files you did not intentionally change.
- Before broad edits, inspect `git status --short`.
- Before final handoff, report remaining uncommitted or untracked files when relevant.

--------------------------------------------------------------------------------

## Commit Handoff Requirements

After any commit:

- Run `git status --short` before the final response.
- Include the commit hash in the final response.
- State whether the worktree is clean.
- If the worktree is not clean, list the remaining modified, deleted, or untracked files.
- Distinguish files changed by the agent from unrelated or pre-existing worktree changes when that context is known.

--------------------------------------------------------------------------------

## Commit Conventions

This project uses [Conventional Commits](https://www.conventionalcommits.org/). Commit messages are validated by a `commit-msg` hook.

**Format:** `<type>[(scope)]: <description>`

**Types:** `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`

**Recommended Scopes** (aligned with architecture):

  | Scope       | Description                                            |
  | ----------- | ------------------------------------------------------ |
  | `cli`       | Command-line interface and argument parsing            |
  | `agent`     | Agent orchestration, conversation loop, tool execution |
  | `responses` | Responses API backend                                  |
  | `chat`      | Chat Completions API backend                           |
  | `tools`     | Tool definitions (Bash, Read, Edit, Write, etc.)       |
  | `sandbox`   | Sandbox implementations (Seatbelt, Landlock)           |
  | `config`    | Configuration, sessions, data directory                |
  | `session`   | Session persistence and management                     |
  | `model`     | Model configuration and API types                      |
  | `prompts`   | System prompt construction, AGENTS.md integration      |
  | `logger`    | Logging configuration                                  |

--------------------------------------------------------------------------------

## Code Style Guidelines

- **Error Handling**: Use `thiserror` for custom errors, `anyhow` for application errors
- **Async**: Prefer `async fn` with Tokio; use `?` for error propagation
- **Dead Code Suppression**: Follow this ordered policy for suppressing dead-code warnings:
  1. **Default**: Delete dead code. If a function, method, field, or type is unused, remove it.
  2. **Acceptable**: `#[cfg(test)]` for items that are only useful from `#[cfg(test)]` callers (test fixtures, test-only constructors). Do not use `#[cfg(test)]` on `pub fn` items on public types; use `pub(crate)` for test-only accessors instead.
  3. **Last resort**: `#[expect(dead_code, reason = "...")]` only for serde struct fields that must exist for deserialization to succeed but are not read by application logic. The reason must state this explicitly: `reason = "field required for serde deserialization; not read by application code"`. Do not use `#[expect(dead_code, ...)]` for any other purpose.
- `#[allow(dead_code)]` is forbidden project-wide. The `allow_attributes` and `allow_attributes_without_reason` lints in `Cargo.toml` enforce this at compile time.

--------------------------------------------------------------------------------

## ExecPlans

When writing complex features or significant refactors (e.g. L or XL tasks), use an ExecPlan (as described in .agents/PLANS.md) from design to implementation.

Use an ExecPlan for:

- L or XL tasks
- Multi-module refactors
- Changes that alter agent loop behavior
- Changes to tool execution, sandboxing, sessions, or API backends

Keep `.agents/exec-plans/active/index.md` current when creating, completing, or moving plans.

--------------------------------------------------------------------------------

## Architecture Decision Records

When a task introduces or changes a durable architectural decision, write or update an ADR under `docs/adr/` before implementation. Follow `docs/adr/README.md` for ADR triggers, numbering, naming, and template rules.

--------------------------------------------------------------------------------

## Common Pitfalls

- `cargo build` does not validate test-only code.
- Do not edit `.agents/.tasks/index.md` directly.
- Completed and Cancelled tasks must be moved with `just task-complete` or `just task-cancel`.
- This crate has no library target.
- Do not introduce `#[allow(dead_code)]`.

--------------------------------------------------------------------------------

## Additional Notes

- Cache directory: `~/.cache/cake/` (logs, ephemeral data)
- Session directory: `~/.local/share/cake/sessions/` (conversation history)
- Both can be overridden via `CAKE_DATA_DIR` environment variable
- Project-level settings in `.cake/settings.toml`
- Logs at `~/.cache/cake/cake.YYYY-MM-DD.log` (daily rotation, 7-day retention)
