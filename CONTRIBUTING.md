# Contributing to Cake

Agent operating rules are in [AGENTS.md](AGENTS.md). This document is the shared human and agent development workflow.

## Setup

Prerequisites are Git and either [mise](https://mise.jdx.dev/) or a manually installed Rust toolchain and `just`.

```bash
mise install
just setup
prek install --hook-type pre-commit --hook-type pre-push --hook-type commit-msg
```

`just setup` installs the Cargo utilities used by repository recipes. Run `just --list` for the authoritative command catalog.

Binary-size audits additionally require `cargo-bloat`:

```bash
cargo install cargo-bloat
```

Follow the [Auditing Binary Size runbook](docs/runbooks/auditing-binary-size.md) for the release build and analysis commands.

## Development loop

1. Inspect `git status --short` and preserve unrelated work.
2. Read the implementation and its focused tests before editing.
3. Make the smallest coherent change.
4. Run a focused test or check while iterating.
5. Format changed code and run the applicable final gate.
6. Review the diff for compatibility, security, and unnecessary complexity.

The crate has no library target. Do not use `cargo test --lib`.

## Verification

**Definition of done:** run `just ci`.

For Rust, configuration, CI, fixture, or dependency changes, run:

```bash
just ci
```

This checks toolchain synchronization, Linux compilation, formatting, strict Clippy in both feature modes, tests, coverage/change risk, imports, and module size. Run focused commands such as `cargo test <name>` before the full gate.

Additional checks:

- Dependency changes: `just check-deps`.
- Rust-version changes: `just rust-version-check`.
- Linux-sensitive changes on macOS: `just clippy-linux` when the target and cross-compiler are installed.
- Snapshot changes: `just snapshots`, then `cargo insta review`.
- Full release-oriented validation: `just check-full`.
- Documentation-only changes: targeted `panache format --check` and `panache lint` for changed living documents, link validation, `ahm doctor`, and `git diff --check`. Use `just docs-check` when intentionally validating the complete Markdown corpus.

If an applicable check cannot run, report the exact reason and the narrower checks that did run. Do not describe a failing primary branch as unrelated without investigating it.

## Code conventions

- Use `thiserror` for typed domain errors and `anyhow` for application context.
- Prefer `?` over manual propagation.
- Delete dead code. Do not hide it behind `#[allow]`.
- Use `#[expect(..., reason = "...")]` only for intentional, explained lint exceptions.
- Keep imports at module scope unless conditional compilation makes that impossible.
- Use absolute `crate::` imports in production code; verified by `just lint-imports`.
- Preserve public behavior during refactors unless the task explicitly changes it.

Tests and snapshots should encode behavior close to its implementation. Add documentation only when the change affects a user workflow, external contract, security boundary, durable architectural invariant, or contributor workflow.

## Managed work

`ahm` owns tasks, research, ExecPlans, ADR metadata, and generated indexes. Start with `ahm prime`. Use the scoped `ahm context ...` guidance for managed records, never edit generated indexes, and complete managed work before any commit containing its implementation.

## Git and commits

Do not overwrite unrelated changes. Branches are optional unless the work or maintainer requires one. Commit only when requested.

Commits use [Conventional Commits](https://www.conventionalcommits.org/):

```text
feat(cli): add a flag
fix(sandbox): preserve read-only paths
docs: simplify contributor guidance
```

Common types are `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, and `revert`. Keep a commit focused and ensure required hooks and checks pass before pushing.

## Pull requests

Explain the user-visible or maintainer-visible outcome, notable design choices, compatibility or security impact, and exact verification. Link the managed task or ADR when one exists. Update documentation only when its authority is affected.
