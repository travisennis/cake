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

Treat CLI behavior, tool execution semantics, sandbox boundaries, session file formats, configuration shape, provider/backend behavior, streaming/output formats, and task workflow metadata as compatibility surfaces. Preserve them unless the task explicitly asks for a breaking change.

--------------------------------------------------------------------------------

## Documentation Map

- Use this file for agent operating rules, golden path workflows, and safety constraints.
- Use [CONTRIBUTING.md](CONTRIBUTING.md) for contributor onboarding, development setup, common command invocations, and PR workflow guidance for humans and agents.
- Use `docs/design-docs/` for durable implementation and architecture details.
- Use `.agents/` for task, research, ExecPlan, and documentation workflow records.

--------------------------------------------------------------------------------

## Operating Loop

1. Classify the change before editing.
2. Gather the minimum context required for that change class.
3. State assumptions and ask before making risky guesses.
4. Make the smallest change that satisfies the request.
5. Run the narrowest useful verification first.
6. Finish with the required project checks and a concise handoff.

--------------------------------------------------------------------------------

## Workflow Routing

- Agent loop, tool execution, hooks, sandboxing, sessions, API backends, model/provider behavior, prompts, or configuration changes may affect durable behavior or security boundaries. Check the relevant `docs/design-docs/` file and consider whether an ADR or ExecPlan is required.
- CLI, output, streaming JSON, settings, or task workflow changes should preserve documented user-facing contracts. Update matching docs when behavior, flags, file formats, or workflow metadata change.
- Dependency, feature, build, CI, or release changes must keep `Cargo.toml` and `Cargo.lock` consistent, use the smallest feature set that solves the problem, and follow the dependency checks below.
- Documentation-only changes should verify Markdown and links. Rust tests and `just ci` may be skipped when no code, tests, config, fixtures, generated task indexes, dependency files, or build metadata changed; explain the skip in the handoff.

--------------------------------------------------------------------------------

## Start Here

1. Read this file fully before making changes.
2. Read [CONTRIBUTING.md](CONTRIBUTING.md) when you need setup, command, or PR workflow details.
3. For the first task in a session, read `.agents/TASKS.md`, then `.agents/.tasks/index.md`, then the specific task file. For later tasks in the same session, reread only the task index and specific task file unless `.agents/TASKS.md` changed or the task changes task workflow semantics.
4. Prefer narrow checks first, then `cargo fmt`, then `just ci` before final handoff.
5. Do not commit or push unless explicitly asked.
6. Never edit generated task indexes by hand.

--------------------------------------------------------------------------------

## Required Workflow

- Before final handoff for any code, test, config, fixture, or dependency change, run `just ci`.
- If `just ci` cannot be run, state the exact reason and list the narrower checks that were run instead.
- For Rust code changes, use this verification sequence:
  1. Run the narrowest useful check first, such as `cargo check --tests`, `cargo test <module_or_test_name>`, or `cargo test`.
  2. Run `cargo fmt` after code edits.
  3. Run `just ci` before final handoff or commit.
- For cfg-sensitive or platform-specific Rust changes, run the narrowest feasible target check for any installed non-host target affected by the change. This includes changes touching `#[cfg]`, platform-specific modules, sandbox backends, or target-specific dependency features. On macOS, for Linux-sensitive changes, prefer `just clippy-linux` when the Linux target and cross compiler are available; otherwise use the closest feasible `cargo check --target ...` command. If a platform path cannot be verified locally, state the exact gap in task notes and the final handoff.
- Run `just check-coverage` before final handoff when a change adds or removes meaningful Rust code, changes tests or fixtures, changes coverage-related configuration or baselines under `ci/`, or changes dependency/feature selection in a way that can affect compiled code. It mirrors the CI coverage threshold and cargo-crap change-risk gates. It is not required for docs-only changes or task metadata changes.
- When changing test fixtures, test-only code, struct literals used in tests, or `#[cfg(test)]` modules, run `cargo check --tests` before relying on `cargo build` or `cargo check`. Plain `cargo build` and `cargo check` do not validate this project's test code.
- For dependency work, do not update dependencies unless explicitly asked or required by the task. Use `just update-dependencies` for broad dependency refreshes. When adding, removing, updating, or changing Cargo features for dependencies, keep `Cargo.toml` and `Cargo.lock` consistent, prefer the smallest feature set that solves the problem, run `just check-deps`, run the Rust verification sequence above, and run `just check-coverage` if compiled code or feature selection changes. New major runtime dependencies that affect behavior, security posture, binary size, licensing, or platform support may require an ADR; check `docs/adr/README.md`.
- Run `just check-deps` before final handoff for dependency updates, dependency additions/removals, Cargo feature changes, or edits to dependency audit configuration. It is not part of `just ci`; GitHub runs dependency audit separately on the scheduled workflow.
- For documentation-only changes, run the narrowest useful Markdown or link checks instead of `just ci` when the Workflow Routing docs-only conditions apply.
- This is a binary-only crate. Do not run `cargo test --lib`; there is no library target. Use `cargo test <module_or_test_name>` for targeted tests, or `cargo test` for the full test suite.
- Do not commit or push code unless explicitly asked to.

--------------------------------------------------------------------------------

## Build/Test/Run

```bash
# Install required cargo tools for development
just setup

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

# Check coverage threshold and untested-complexity regression
just check-coverage

# Check dependency advisories
just check-deps

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

# Broad local validation suite
just check-full

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
- Do not edit generated indexes by hand. Update source task, research, or ExecPlan files and run `ahm index`. Do not run `ahm index` after `ahm task start`, `ahm task complete`, or `ahm task cancel` unless you edit metadata by hand afterward; those commands already regenerate indexes.
- When marking a task as Completed, use `ahm task complete <id>`. It updates the task front matter, moves the file from `.agents/.tasks/active/` to `.agents/.tasks/completed/`, and regenerates the indexes in one step. Do not leave Completed tasks in `active/`.
- When marking a task as Cancelled, use `ahm task cancel <id>`. It updates the task front matter, moves the file from `.agents/.tasks/active/` to `.agents/.tasks/cancelled/`, and regenerates the indexes in one step. Do not leave Cancelled tasks in `active/`.

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

## Documentation Workflow

Before auditing or updating documentation, read `.agents/DOCS.md`.
Prefer the repository's existing documentation conventions over adding
new structures.

--------------------------------------------------------------------------------

## Architecture Decision Records

When a task introduces or changes a durable architectural decision, write or update an ADR under `docs/adr/` before implementation. Follow `docs/adr/README.md` for ADR triggers, numbering, naming, and template rules.

--------------------------------------------------------------------------------

## Implementation Documentation

When moving implementation between files or packages, update repository code maps and implementation-location references even if user-facing behavior is unchanged.

--------------------------------------------------------------------------------

## Common Pitfalls

- `cargo build` does not validate test-only code.
- Do not edit `.agents/.tasks/index.md` directly.
- Completed and Cancelled tasks must be moved with `ahm task complete` or `ahm task cancel`.
- This crate has no library target.
- Do not introduce `#[allow(dead_code)]`.

--------------------------------------------------------------------------------

## Additional Notes

- Cache directory: `~/.cache/cake/` (logs, ephemeral data)
- Session directory: `~/.local/share/cake/sessions/` (conversation history)
- Both can be overridden via `CAKE_DATA_DIR` environment variable
- Project-level settings in `.cake/settings.toml`
- Logs at `~/.cache/cake/cake.YYYY-MM-DD.log` (daily rotation, 7-day retention)
