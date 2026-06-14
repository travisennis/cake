# Agent Instructions

## Project

cake is a Rust 2024 binary-only AI coding assistant CLI. It uses Tokio, clap,
anyhow/thiserror, tracing, reqwest, serde/serde_json, OpenAI-compatible Chat
Completions and Responses API backends, sandboxed tool execution, persisted
sessions, and macOS Seatbelt / Linux Landlock sandboxing.

The core mechanism is an agent loop: the model can request tools, receive tool
results, and continue until it returns a final response.

Treat these as compatibility surfaces unless the task explicitly asks for a
breaking change: CLI behavior, tool execution semantics, sandbox boundaries,
session file formats, configuration shape, provider/backend behavior,
streaming/output formats, and task workflow metadata.

## Operating Loop

1. Classify the change before editing.
2. Read only the docs needed for that change class.
3. State assumptions and ask before making risky guesses. Stop when confused.
4. Keep the change surgical; do not mix unrelated behavior, formatting,
   dependency, task, or documentation cleanup.
5. Run the narrowest useful check first, then the required final checks.
6. Handoff with what changed, what was verified, and any remaining risk.

When this conflicts with a specialized workflow doc, the specialized doc wins.

## Workflow Routing

### Code, Behavior, Config, Sessions, Providers, Tools, Sandboxing

Read [ARCHITECTURE.md](ARCHITECTURE.md) for the codemap and invariants.
Check `docs/design-docs/` before changing behavior or security boundaries.
Consider whether the change needs an [ADR](docs/adr/README.md) or ExecPlan.
Preserve documented contracts (CLI, output, settings, sessions, file-format,
workflow) unless the task explicitly changes them.

### Tasks

When asked to create, choose, update, or work on a task, read `.agents/TASKS.md`, then use `ahm task next`, `ahm task ready`, `ahm task list`, `ahm task blocked`, or `ahm task show <id>` to inspect task state before acting. Do not edit generated task indexes by hand; use `ahm` commands or regenerate with `ahm index` when source metadata changes.

### Research

When asked to create, update, organize, or use research, read
`.agents/RESEARCH.md`, then use `.agents/.research/index.md` as the map.

### ExecPlans

Use `.agents/PLANS.md` for L/XL work, multi-module refactors, major behavior
changes, and changes to the agent loop, tool execution, sandboxing, sessions,
or API backends.

### ADRs

Use `docs/adr/README.md` before implementation when a task introduces or
changes a durable architectural decision.

### Documentation

Read `.agents/DOCS.md` before auditing or updating docs. Follow existing
conventions; update docs when user-facing behavior, config, architecture,
workflow, or compatibility changes.

### Dependencies, Build, CI, Release

Do not update dependencies unless asked. Keep `Cargo.toml` and `Cargo.lock`
consistent. Use the smallest feature set.

Use [CONTRIBUTING.md](CONTRIBUTING.md) for setup, commands, PR workflow,
and commit conventions.

## Verification

For Rust changes:

1. Run the narrowest useful check first, such as `cargo check --tests`,
   `cargo test <module_or_test_name>`, or `cargo test`.
2. Run `cargo fmt` after code edits.
3. Run `just check-coverage` when adding or removing meaningful Rust code,
   changing tests or fixtures, changing coverage configuration or baselines
   under `ci/`, or changing dependency features in a way that affects compiled
   code.
4. Run `just ci` before final handoff for code, test, config, fixture, or
   dependency changes.

If `just ci` cannot be run, state the exact reason and list the narrower checks
that were run instead.

For cfg-sensitive or platform-specific Rust changes, run the narrowest feasible
target check for installed non-host targets affected by the change. On macOS,
prefer `just clippy-linux` for Linux-sensitive changes when the target and cross
compiler are available; otherwise use the closest feasible
`cargo check --target ...` command. State any platform verification gap in the
handoff.

For dependency changes, also run `just check-deps`. It is not part of
`just ci`.

For documentation-only changes, run the narrowest useful Markdown or link
checks instead of `just ci` when no code, tests, config, fixtures, generated
task indexes, dependency files, or build metadata changed. Explain the skip in
the handoff.

This crate has no library target. Do not run `cargo test --lib`; use
`cargo test <module_or_test_name>` for targeted tests or `cargo test` for the
full test suite.

## Repository Rules

- Do not commit or push unless explicitly asked.
- Assume uncommitted changes may belong to the user. Do not revert, overwrite,
  or clean files you did not intentionally change.
- Before broad edits, inspect `git status --short`.
- Before final handoff, report remaining uncommitted or untracked files when
  relevant.
- Do not edit generated task, research, or ExecPlan indexes by hand. Update
  the source records and run the appropriate `ahm` command (`ahm index` for
  indexes, `ahm adr` commands for ADRs).
- Treat `.agents/*` workflow guides and `docs/adr/README.md` as ahm-managed
  templates. Change canonical guidance in the AHM repository, not through local
  consumer edits.
- When moving implementation between files or modules, update repository code
  maps and implementation-location references even if user-facing behavior is
  unchanged.

## Code Style

- Use `thiserror` for custom errors and `anyhow` for application errors.
- Prefer Tokio `async fn` and `?` for error propagation.
- Default to deleting dead code. Use `#[cfg(test)]` only for test-only items.
- Use `#[expect(dead_code, reason = "...")]` only for serde fields that must
  exist for deserialization but are not read by application logic. The reason
  must say: `field required for serde deserialization; not read by application code`.
- Never use `#[allow(dead_code)]`.

## Commit Handoff

After any requested commit, run `git status --short`, include the commit hash,
state whether the worktree is clean, and list remaining modified, deleted, or
untracked files. Use Conventional Commits; the `commit-msg` hook validates the
format.
