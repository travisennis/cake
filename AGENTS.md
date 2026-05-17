# AGENTS.md

## Project Overview

cake is an AI coding assistant CLI that:

- Written with Rust 2024 edition with Tokio async runtime
- Binary-only CLI. Not a library.
- Uses clap for CLI parsing, anyhow/thiserror for error, tracing for logging
- Use reqwest + serde/serde_json for HTTP/JSON
- Integrates with LLMs via an API compatible for with OpenAI Chat Completions or the Responses API.
- Executes tools (Bash, Read, Edit, Write) in a sandboxed environment
- Manages conversation sessions with continue/resume/fork capabilities
- Uses OS-level sandboxing (macOS Seatbelt, Linux Landlock)

**Core mechanism**: The agent loop lets the model execute tools, receive results, and continue until it returns a final response.

---

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

---

## Agent Instructions

- Run the `Full CI check` command when you complete a task that involves code, config, or dependency changes to make sure the code is correct.
- This is a binary-only crate. Do not run `cargo test --lib`; there is no library target. Use `cargo test <module_or_test_name>` for targeted tests, or `cargo test` for the full test suite.
- When changing test fixtures, test-only code, struct literals used in tests, or `#[cfg(test)]` modules, run `cargo check --tests` before relying on `cargo build` or `cargo check`. Plain `cargo build` and `cargo check` do not validate this project's test code.
- For Rust code changes, use this verification sequence:
  1. Run the narrowest useful check first, such as `cargo check --tests`, `cargo test <module_or_test_name>`, or `cargo test`.
  2. Run `cargo fmt` after code edits.
  3. Run `just ci` before final handoff or commit.
- When a task includes committing and task-status updates, commit the intended code/task changes together unless the user asks for separate commits. After committing and moving or regenerating task files, run `git status --short` before the final response and report any remaining uncommitted or untracked files.
- When asked to create, choose, update, or work on a task, first read `.agents/TASKS.md`, then use `.agents/.tasks/index.md` as the task queue and open the specific task file before acting.
- Use task labels to filter work by type, area, and risk when the user asks for focused work.
- Do not edit generated task indexes by hand; update task files and run `just task-index` (never invoke the underlying Python script directly).
- When marking a task as Completed, use `just task-complete <id>`. It updates the task front matter, moves the file from `.agents/.tasks/active/` to `.agents/.tasks/completed/`, and regenerates the indexes in one step. Do not leave Completed tasks in `active/`.
- When asked to create, update, organize, or use research, first read `.agents/RESEARCH.md`, then use `.agents/.research/index.md` as the research map and open the relevant research file before acting.
- Do not commit or push code unless explicitly asked to.

---

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

---

## Code Style Guidelines

- **Error Handling**: Use `thiserror` for custom errors, `anyhow` for application errors
- **Async**: Prefer `async fn` with Tokio; use `?` for error propagation
- **Dead Code Suppression**: Follow this ordered policy for suppressing dead-code warnings:
  1. **Default**: Delete dead code. If a function, method, field, or type is unused, remove it.
  2. **Acceptable**: `#[cfg(test)]` for items that are only useful from `#[cfg(test)]` callers (test fixtures, test-only constructors). Do not use `#[cfg(test)]` on `pub fn` items on public types; use `pub(crate)` for test-only accessors instead.
  3. **Last resort**: `#[expect(dead_code, reason = "...")]` only for serde struct fields that must exist for deserialization to succeed but are not read by application logic. The reason must state this explicitly: `reason = "field required for serde deserialization; not read by application code"`. Do not use `#[expect(dead_code, ...)]` for any other purpose.
  - `#[allow(dead_code)]` is forbidden project-wide. The `allow_attributes` and `allow_attributes_without_reason` lints in `Cargo.toml` enforce this at compile time.

---

## ExecPlans

When writing complex features or significant refactors (e.g. L or XL tasks), use an ExecPlan (as described in .agents/PLANS.md) from design to implementation.

Keep `.agents/exec-plans/active/index.md` current when creating, completing, or moving plans.

---

## Additional Notes

- Cache directory: `~/.cache/cake/` (logs, ephemeral data)
- Session directory: `~/.local/share/cake/sessions/` (conversation history)
- Both can be overridden via `CAKE_DATA_DIR` environment variable
- Project-level settings in `.cake/settings.toml`
- Logs at `~/.cache/cake/cake.YYYY-MM-DD.log` (daily rotation, 7-day retention)
