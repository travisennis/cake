# Contributing to Cake

Agent operating rules are in [AGENTS.md](AGENTS.md). This document is the shared human and agent development workflow.

## Setup

Prerequisites are Git and either [mise](https://mise.jdx.dev/) or a manually installed Rust toolchain and `just`.

```bash
mise trust
mise install
just setup
```

`mise trust` marks the repository's `.mise.toml` as trusted; mise refuses to read an untrusted config, so it must run before `mise install`. It is a one-time step per clone location.

`just setup` installs the Cargo utilities used by repository recipes and the git hooks declared in `prek.toml` (`pre-commit`, `pre-push`, and `commit-msg`). Re-running it is safe: the hook install is idempotent. Run `just --list` for the authoritative command catalog.

`mise install` also provides `sccache` and points `RUSTC_WRAPPER` at it, so a newly created worktree reuses already-compiled dependencies instead of rebuilding the graph from cold. Without mise, install `sccache` and export `RUSTC_WRAPPER=sccache` yourself.

Binary-size audits additionally require `cargo-bloat`:

```bash
cargo install cargo-bloat
```

Follow the [Auditing Binary Size runbook](docs/runbooks/auditing-binary-size.md).

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

## Command-safety corpus cases

Append cases to `src/clients/tools/corpus/commands.jsonl` as documented in its README. Run `just judge-corpus-check` locally; the live `just judge-corpus` requires provider credentials and authorized external spend.

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
- Documentation-only changes: targeted `panache format --check` and `panache lint` for changed living documents, link validation, and `git diff --check`. Use `just docs-check` when intentionally validating the complete Markdown corpus; it also runs `just lint-instruction-size`.
- Instruction changes (AGENTS.md, `.agents/skills/`, guardrails, runbooks): `just lint-instruction-size` caps AGENTS.md, the one document loaded every session, reports the corpus, and also runs in `just ci`. [Agent-facing instructions](docs/guardrails/agent-instructions.md) is the authority for what an added instruction must justify.

### Pre-push routing

The pre-push hook routes by changed path class instead of running the full gate unconditionally.

- Markdown-only pushes run `just pre-push-docs`: targeted `panache format --check` and `panache lint` on the changed living documents, plus `git diff --check`.
- Pushes touching `src/`, `tests/`, `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `.cargo/`, `.github/workflows/`, `justfile`, or `ci/` run the full `just ci` gate.
- Mixed pushes run both.
- Anything unclassified or unresolvable fails closed to the full gate.
- `just pre-push-force` always runs the full gate; `just pre-push-classify` prints the classification for the current branch.

[Working on branches and worktrees](docs/runbooks/parallel-worktrees.md) covers how the base is resolved and why the gate follows the checkout rather than the pushed ref.

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

Issues, research notes, ExecPlans, and ADRs are managed records; follow the issue lifecycle in [docs/workflow/tasks.md](docs/workflow/tasks.md).

## Git and commits

Commit and push freely on a feature branch, and commit often --- uncommitted work is the fragile state. Stage the paths you changed rather than `git add -A`, so unrelated in-flight edits stay out of your pull request. Ask first before force-pushing (it discards history) or opening a pull request.

All work happens on a branch, which is what makes that safe: `master` is protected by a GitHub ruleset rejecting direct pushes, and by the `branch-guard` hook in `prek.toml`, which rejects commits and pushes on `master` at both `pre-commit` and `pre-push`. Branch names use the commit type as a prefix, such as `feat/turn-limits` or `fix/sandbox-read-only`. [Working on branches and worktrees](docs/runbooks/parallel-worktrees.md) covers the mechanics, including running several branches at once in linked worktrees.

`ci/cargo-crap-baseline.json` is generated and committed, so parallel branches conflict on it. A three-way merge of that file is meaningless. Take `master`'s copy and regenerate with `just change-risk-baseline`.

Commits use [Conventional Commits](https://www.conventionalcommits.org/):

```text
feat(cli): add a flag
fix(sandbox): preserve read-only paths
docs: simplify contributor guidance
```

Common types are `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, and `revert`. Keep a commit focused and ensure required hooks and checks pass before pushing.

Scopes are optional and at most one, from the `scopes` allowlist in `cog.toml`, enforced by the commit-msg hook:

```text
agent  cli  config  extensions  prompts  providers  sandbox  session  tools
```

The vocabulary names the architecture domain that owns the change; the file or tool belongs in the subject. It is coarser than `.ahm` `area:*` labels, and cross-cutting changes stay unscoped. Adding a scope is a vocabulary change proposed in the PR that updates the allowlist; history predates the allowlist, so `cog check` is not a gate, and amended messages must comply.

## Pull requests

Explain the user-visible or maintainer-visible outcome, notable design choices, compatibility or security impact, and exact verification. Link the managed task or ADR when one exists. Open the pull request only when the change is ready to merge: acceptance notes, verification, documentation assessment, and any ExecPlan archival must already be complete. Use `Closes #<number>` for the managed issue; GitHub closes it when the pull request reaches `master`, after which the issue may receive its final delivered/verified summary. Update documentation only when its authority is affected.
