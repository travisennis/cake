# Contributing to Cake

Agent operating rules are in [AGENTS.md](AGENTS.md). This document is the shared human and agent development workflow.

## Setup

Prerequisites are Git and either [mise](https://mise.jdx.dev/) or a manually installed Rust toolchain and `just`.

```bash
mise trust
mise install
just setup
```

`mise trust` marks the repository's `.mise.toml` as trusted; mise refuses to read an untrusted config, so it must run before `mise install`. It is a one-time step per clone location --- mise keys trust to the config file's path, so a new worktree path asks again.

`just setup` installs the Cargo utilities used by repository recipes and the git hooks declared in `prek.toml` (`pre-commit`, `pre-push`, and `commit-msg`). Re-running it is safe: the hook install is idempotent. To refresh hooks without the full setup, run `prek install --hook-type pre-commit --hook-type pre-push --hook-type commit-msg` yourself. Run `just --list` for the authoritative command catalog.

`mise install` also provides `sccache` and points `RUSTC_WRAPPER` at it, so a newly created worktree reuses already-compiled dependencies instead of rebuilding the graph from cold. The setting is scoped to `.mise.toml`, so it applies to local development only. Without mise, install `sccache` and export `RUSTC_WRAPPER=sccache` yourself, or accept a cold build per worktree.

Binary-size audits additionally require `cargo-bloat`:

```bash
cargo install cargo-bloat
```

Follow the [Auditing Binary Size runbook](docs/runbooks/auditing-binary-size.md) for the release build and analysis commands.

## Development loop

1. Inspect `git status --short` and preserve unrelated work.
2. Create the branch before editing: `just branch <type>/<slug>`, or `just worktree <type>/<slug>` to work in a linked worktree beside other in-flight work.
3. Read the implementation and its focused tests before editing.
4. Make the smallest coherent change.
5. Run a focused test or check while iterating.
6. Format changed code and run the applicable final gate.
7. Review the diff for compatibility, security, and unnecessary complexity.
8. Push the branch and open a pull request with `just pr`.

The crate has no library target. Do not use `cargo test --lib`.

## Verification

**Definition of done:** run `just ci`.

For Rust, configuration, CI, fixture, or dependency changes, run:

```bash
just ci
```

This checks toolchain synchronization, Linux compilation, formatting, strict Clippy in both feature modes, tests, coverage/change risk, imports, and module size. Run focused commands such as `cargo test <name>` before the full gate.

Additional checks:

- Dependency changes: `just check-deps`. [Dependency and supply chain posture](docs/dependencies.md) is the authority for pin ownership and update review.
- Rust-version changes: `just rust-version-check`.
- Linux-sensitive changes on macOS: `just clippy-linux` when the target and cross-compiler are installed.
- Snapshot changes: `just snapshots`, then `cargo insta review`.
- Label changes: `just labels-check-file` (file validation, also CI), `just labels-check` (repo drift vs `.github/labels.yml`), `just labels` (apply), `just labels-prune` (delete unlisted labels).
- Full release-oriented validation: `just check-full`.
- Documentation-only changes: targeted `panache format --check` and `panache lint` for changed living documents, link validation, and `git diff --check`. Use `just docs-check` when intentionally validating the complete Markdown corpus.

### Pre-push routing

The pre-push hook routes by changed path class instead of running the full gate unconditionally. The classifier is `scripts/classify-changes.sh`, shared with the `changes` job in `.github/workflows/ci.yml`, and it measures the changed set against the upstream branch when one is set, otherwise against `origin/master` (branches from `just branch` and `just worktree` have no upstream):

- Markdown-only pushes run `just pre-push-docs`: targeted `panache format --check` and `panache lint` on the changed living documents plus `git diff --check`.
- Pushes touching `src/`, `tests/`, `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `.cargo/`, `.github/workflows/`, `justfile`, or `ci/` run the full `just ci` gate.
- Mixed pushes run both.
- A changed file in no class (for example `prek.toml` or `.mise.toml`) fails closed to the full gate, as does an unresolvable base.
- `just pre-push-force` always runs the full gate; `just pre-push-classify` prints the classification for the current branch.

If an applicable check cannot run, report the exact reason and the narrower checks that did run. Do not describe a failing primary branch as unrelated without investigating it.

## Code conventions

- Use `thiserror` for typed domain errors and `anyhow` for application context.
- Prefer `?` over manual propagation.
- Delete dead code. Do not hide it behind `#[allow]`.
- Use `#[expect(..., reason = "...")]` only for intentional, explained lint exceptions.
- Keep imports at module scope unless conditional compilation makes that impossible.
- Use absolute `crate::` imports in production code; verified by `just lint-imports`.
- Preserve public behavior during refactors unless the task explicitly changes it.
- Spawn `git` through `config::git::command`, and in tests through `config::git::test_support` or the `git` helper in `tests/`. Git exports `GIT_DIR` and its siblings into hooks and everything they spawn, so a command that inherits them operates on the exporting repository rather than the directory it was given.

Tests and snapshots should encode behavior close to its implementation. Add documentation only when the change affects a user workflow, external contract, security boundary, durable architectural invariant, or contributor workflow.

## Managed work

Issues, research notes, ExecPlans, and ADRs are managed records. Work is tracked in GitHub Issues. Use the workflow references in AGENTS.md (task, ExecPlan, ADR, and research workflows), never edit generated indexes (there are none), and follow the issue lifecycle in [docs/workflow/tasks.md](docs/workflow/tasks.md).

## Git and commits

Do not overwrite unrelated changes. Commit only when requested.

All work happens on a branch. `master` is protected: a GitHub ruleset rejects direct pushes and requires a pull request with passing checks, and the `branch-guard` hook in `prek.toml` rejects a commit made on `master` before you spend a verification gate on it. Branch names use the commit type as a prefix, such as `feat/turn-limits` or `fix/sandbox-read-only`. [Working on branches and worktrees](docs/runbooks/parallel-worktrees.md) covers the mechanics, including running several branches at once in linked worktrees.

`ci/cargo-crap-baseline.json` is generated and committed, so parallel branches conflict on it. A three-way merge of that file is meaningless. Take `master`'s copy and regenerate with `just change-risk-baseline`.

Commits use [Conventional Commits](https://www.conventionalcommits.org/):

```text
feat(cli): add a flag
fix(sandbox): preserve read-only paths
docs: simplify contributor guidance
```

Common types are `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, and `revert`. Keep a commit focused and ensure required hooks and checks pass before pushing.

## Pull requests

Explain the user-visible or maintainer-visible outcome, notable design choices, compatibility or security impact, and exact verification. Link the managed task or ADR when one exists. Open the pull request only when the change is ready to merge: acceptance notes, verification, documentation assessment, and any ExecPlan archival must already be complete. Use `Closes #<number>` for the managed issue; GitHub closes it when the pull request reaches `master`, after which the issue may receive its final delivered/verified summary. Update documentation only when its authority is affected.
