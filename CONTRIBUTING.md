# Contributing to Cake

Agent operating rules are in [AGENTS.md](AGENTS.md). This document is the shared human and agent development workflow.

## Setup

Prerequisites are Git, Python 3, and either [mise](https://mise.jdx.dev/) or a manually installed Rust toolchain and `just`. The repository scripts and `just doctor` use only the Python standard library.

```bash
mise trust
mise install
just setup
```

`mise trust` marks the repository's `.mise.toml` as trusted; mise refuses to read an untrusted config, so it must run before `mise install`. It is a one-time step per clone location.

`just setup` installs the pinned Cargo utilities used by repository recipes and the git hooks declared in `prek.toml` (`pre-commit`, `pre-push`, and `commit-msg`). It verifies every pinned version after installing and exits nonzero with the tool name and install command when one is missing or broken. Re-running it is safe. Run `just --list` for the authoritative command catalog.

`just doctor` is the read-only readiness report for the commands above. It checks the Rust toolchain and `just` versions against `rust-toolchain.toml` and `.mise.toml`, the pinned Cargo utilities through `scripts/dev-tools.sh`, and the three installed hook wrappers. It never installs, runs a hook, mutates the checkout or configuration, or calls a provider. Exit 0 means the gates and hooks can run; exit 1 names each required failure with a recovery; exit 2 is a usage or configuration error.

`mise install` also provides `sccache` and points `RUSTC_WRAPPER` at it, so a newly created worktree reuses already-compiled dependencies instead of rebuilding the graph from cold. Without mise, install `sccache` and export `RUSTC_WRAPPER=sccache` yourself.

The user-facing cake skills --- `analyzing-cake-sessions`, `debugging-cake`, `debugging-cake-sandbox`, and `configuring-cake` --- ship in the separate [cake-skills](https://github.com/travisennis/cake-skills) repository, not here. [AGENTS.md](AGENTS.md) routes to them by name, so install them into your user-level skills directory once per machine:

```bash
git clone https://github.com/travisennis/cake-skills.git ~/Projects/cake-skills
mkdir -p ~/.agents/skills
cp -R ~/Projects/cake-skills/skills/. ~/.agents/skills/
```

Symlink them instead of copying (`cd ~/Projects/cake-skills && just link`) to pick up edits on the next run. Project skills in a repository's `.agents/skills/` still take precedence. Verify with `cake debug skills`.

Binary-size audits additionally require `cargo-bloat`:

```bash
cargo install cargo-bloat
```

Follow the [Auditing Binary Size runbook](docs/runbooks/auditing-binary-size.md).

## Development loop

1. Inspect `git status --short` and preserve unrelated work.
2. If the task will change repository files, work on `master` for a small or single-task change, or create a branch when the change needs isolation: `just branch <type>/<slug>`, or `just worktree <type>/<slug>` to work in a linked worktree beside other in-flight work. Read-only analysis, triage, or research that produces no repository changes can stay on the current branch.
3. Read the implementation before editing.
4. Make the smallest coherent change.
5. Run `cargo check --all-targets` while iterating.
6. Format changed code and run `just check`.
7. Review the diff for compatibility, security, and unnecessary complexity.
8. Integrate the change. Maintainers push `master` when it is ready: fast-forward or squash-merge a local branch into `master`, push, and delete the branch. Do not open a GitHub pull request unless the user explicitly asks for one. External contributors push a branch and open a pull request with `just pr` when it is ready for review.

The crate has no library target.

## No tests

This repository has no test suite and does not accept new tests. Do not write tests, test fixtures, snapshot tests, or test-only helpers, and do not run `cargo test` or any test harness. If a change seems to need a test, verify the behavior with a manual `cake` invocation or `cake bash check` instead, and describe that verification in the handoff.

### Change size

Keep a change within 800 changed lines unless it is mechanical, and within 500 when it changes complex logic. Past either limit, stop extending the diff and report a staging plan instead: the smallest coherent stage to land first, based on the actual diff, its dependencies, and the affected call sites. A change that cannot be staged at all is a scope problem to raise, not a budget to raise.

## Verification

**Definition of done:** run the gate your change class routes to and report, in the handoff (or the pull request for an external contribution), which checks ran and which did not.

  | Change class                 | Command                   | What it covers                                                                                                                                                                                  |
  | ---------------------------- | ------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
  | Rust source or control flow  | `just check`              | Toolchain pin, `cargo fmt --check`, strict Clippy over all targets and features, the per-function complexity ratchet, and the import, dependency-direction, module, glossary, and comment lints |
  | Per-function complexity risk | `just cc-check`           | Cyclomatic complexity against `ci/complexity-baseline.json`; unlisted functions use the CC 10 target, listed grandfathers use `max(target, baseline CC)`. Inside `just check`                   |
  | Markdown                     | `just docs-check`         | Formatting and lint over the Markdown git tracks                                                                                                                                                |
  | Dependencies                 | `just check-deps`         | `cargo deny` advisories, plus the license allowlist in `deny.toml`                                                                                                                              |
  | Linux-only `cfg` paths       | `just clippy-linux`       | Clippy against `x86_64-unknown-linux-gnu`, when the target and cross compiler are installed                                                                                                     |
  | Rust-version pin             | `just rust-version-check` | The toolchain pin stays synchronized across its files. Also inside `just check`                                                                                                                 |

Additional checks:

- Dependency changes: `just check-deps`; [Dependency and supply chain posture](docs/dependencies.md) is the authority for pin ownership and update review.
- Documentation-only changes: `just docs-check`, plus `git diff --check`.
- Label changes: `just labels-check-file` (file validation), `just labels-check` (repo drift vs `.github/labels.yml`), `just labels` (apply), `just labels-prune` (delete unlisted labels).
- Instruction changes (AGENTS.md, `.agents/skills/`, guardrails, runbooks): no size cap applies; [Agent-facing instructions](docs/guardrails/agent-instructions.md) is the authority for what an added instruction must justify.

If an applicable check cannot run, report the exact reason and the narrower checks that did run.

## Code conventions

- Use `thiserror` for typed domain errors and `anyhow` for application context.
- Prefer `?` over manual propagation.
- Delete dead code. Do not hide it behind `#[allow]`.
- Use `#[expect(..., reason = "...")]` only for intentional, explained lint exceptions.
- Keep imports at module scope unless conditional compilation makes that impossible.
- Use absolute `crate::` imports in production code; verified by `just lint-imports`.
- Do not add ordinary comments to Rust production sources. Rationale belongs in [docs/adr/](docs/adr/), the only sanctioned home for a decision; `just lint-comments` enforces the ban against `ci/comment-baseline.json`. Doc comments and `// SAFETY:` blocks are exempt because clap and clippy consume them. Extend an existing record when the decision belongs to the same subsystem, and lower the ratchet after a cleanup rather than raising it.
- Preserve public behavior during refactors unless the task explicitly changes it.
- Spawn `git` through `config::git::command`. Git exports `GIT_DIR` and its siblings into hooks and everything they spawn, so a command that inherits them operates on the exporting repository rather than the directory it was given.

Add documentation only when the change affects a user workflow, external contract, security boundary, durable architectural invariant, or contributor workflow.

## Module size

`just lint-module-size` reports every `src/` file over 800 production lines. It is a report, not a gate: the brace-counting heuristic in `scripts/lint-module-size.py` documents the cases it misclassifies. Treat the report as a direction rather than a threshold to cross --- new functionality goes in a new module, and a change to a file already over the line should not make it longer.

These files are already over the production threshold, which is what makes them the ones that attract unrelated changes. Add to one only when the change belongs to what the file already does:

`src/cli/bash.rs`, `src/clients/agent/agent_loop.rs`, `src/clients/agent_runner.rs`, `src/clients/responses.rs`, `src/clients/tools/bash.rs`, `src/clients/tools/edit.rs`, `src/clients/tools/mod.rs`, `src/clients/tools/sandbox/mod.rs`, `src/config/settings.rs`, `src/config/toolbox.rs`, `src/hooks.rs`, `src/main.rs`, `src/types/session.rs`.

`just lint-module-size` prints the current counts; do not copy them here, where they would drift.

## Git and commits

Commit and push freely, and commit often --- uncommitted work is the fragile state. Stage the paths you changed rather than `git add -A`, so unrelated in-flight edits stay out of your change. Ask first before force-pushing (it discards history). Push `master` when the work is ready; external contributors push a branch and open a pull request.

Repository changes may land directly on `master` or on a branch. A branch is useful for isolation or worktrees: names use the commit type as a prefix, such as `feat/turn-limits` or `fix/sandbox-read-only`. Maintainers integrate locally and push `master`; a branch is not a required review step. There is no remote ruleset requiring pull requests and no local branch guard. [Working on branches and worktrees](docs/runbooks/parallel-worktrees.md) covers the mechanics, including running several branches at once in linked worktrees.

Commits use [Conventional Commits](https://www.conventionalcommits.org/):

```text
feat(cli): add a flag
fix(sandbox): preserve read-only paths
docs: simplify contributor guidance
```

Common types are `feat`, `fix`, `docs`, `refactor`, `perf`, `build`, `ci`, `chore`, and `revert`. Keep a commit focused and ensure required hooks and checks pass before pushing.

Scopes are optional and at most one, from the `scopes` allowlist in `cog.toml`, enforced by the commit-msg hook:

```text
agent  cli  config  extensions  prompts  providers  sandbox  session  tools
```

The vocabulary names the architecture domain that owns the change; the file or tool belongs in the subject. It is coarser than the PR `area:*` labels, and cross-cutting changes stay unscoped. Adding a scope is a vocabulary change proposed in the change that updates the allowlist; history predates the allowlist, so `cog check` is not a gate, and amended messages must comply.

## Integration and pull requests

Maintainers integrate locally. Commit on `master` for a small change, or use a local branch for isolation, then fast-forward or squash-merge it into `master`, push, and delete the branch. Do not open a GitHub pull request unless the user explicitly asks for one.

External contributors push a branch and open a pull request when the change is ready for review: acceptance notes, verification, and documentation assessment must already be complete. Explain the user-visible or maintainer-visible outcome, notable design choices, compatibility or security impact, and exact verification. Use `just pr` with `labels=`, `body=`, and `title=`; the recipe applies labels from `.github/labels.yml` and fills the body from commits when none is given. Do not merge, enable auto-merge, close the PR, or delete the remote branch without explicit user approval.

Use a Conventional Commit subject for the pull request title too, for example `feat(session): show timing breakdown`. Cake configures GitHub to use the PR title as the default squash-commit subject, including for one-commit pull requests. The `Conventional PR Title` check reruns when a title changes; review any manually edited squash message in the merge dialog as well.
