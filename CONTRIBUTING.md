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
2. If the task will change repository files, create the branch before editing: `just branch <type>/<slug>`, or `just worktree <type>/<slug>` to work in a linked worktree beside other in-flight work. Read-only analysis, triage, or research that produces no repository changes can stay on the current branch.
3. Read the implementation and its focused tests before editing.
4. Make the smallest coherent change.
5. Run a focused test or check while iterating.
6. Format changed code and run the applicable final gate.
7. Review the diff for compatibility, security, and unnecessary complexity.
8. Push the branch and open a pull request with `just pr`. Pass `labels="type:...,area:..."`, `body=<file>`, `title=<text>` (default: HEAD commit subject), and `issue=<number>` to apply labels, use a body file, set the title, and comment the PR URL back on the managed issue; without a body the recipe fills from commits. Opening the pull request is the default handoff; wait for explicit user approval before merging, enabling auto-merge, closing the PR or issue, or deleting the remote branch.

The crate has no library target. Do not use `cargo test --lib`.

## Command-safety corpus cases

Append cases to `src/clients/tools/corpus/commands.jsonl` as documented in its README. Run `just judge-corpus-check` locally; the live `just judge-corpus` requires provider credentials and authorized external spend.

## Verification

**Definition of done:** run the gate your change class routes to and report, in the handoff or pull request, which gates ran and which did not. The matrix below names the command for each class and what it actually covers, so a passing fast gate is not mistaken for a complete one.

### Change-class matrix

  | Change class                               | Gate                                                                                                                         | What it covers                                                                                                                                                                                                                                                                                                                                                        |
  | ------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
  | Rust source, control flow, tests           | `just check`                                                                                                                 | Toolchain pin, `cargo fmt --check`, strict Clippy, all-feature tests, import and dependency-direction lints, module/instruction/glossary lints, the per-function complexity ratchet (`just cc-check`), and the Python fixture suites (`just check-scripts`)                                                                                                           |
  | Per-function complexity or CRAP risk       | `just cc-check`, `just change-risk-report` for the per-function detail                                                       | Cyclomatic complexity against `ci/cargo-crap-baseline.json`; new functions use the target ceiling and existing ones use `max(target, baseline CC)`. Also inside `just check`                                                                                                                                                                                          |
  | Coverage or change-risk sensitive          | `just check-coverage`                                                                                                        | Coverage threshold, CRAP regression, and the same CC gate, all from one instrumented run. Cleans the coverage artifacts that may affect results through `scripts/coverage-clean.sh`, then stops before evaluating the threshold if profile data survived the clean or the report spells one file under two source roots, rather than print a verdict nobody can trust |
  | Regenerating `ci/cargo-crap-baseline.json` | `just change-risk-baseline`                                                                                                  | Cleans and guards the same artifacts through `scripts/coverage-clean.sh`, then rewrites the baseline from a fresh run                                                                                                                                                                                                                                                 |
  | `scripts/**` Python tooling                | `just check-scripts`                                                                                                         | The same suites CI's `changes` job runs: dependency sweep, profiling helper, binary-size baseline, change classifier, `just pr`, eval harness, session metrics, the coverage artifact guard, and the documentation corpus. Stdlib only; no credentials, network, or provider calls. Also inside `just check`                                                          |
  | Markdown                                   | `just docs-check` for the tracked corpus; `panache format --check <files>` and `panache lint <files>` for an untracked draft | Formatting and lint over the Markdown git tracks, so an untracked scratch note is neither checked nor rewritten. The pre-push route checks only the changed living documents                                                                                                                                                                                          |
  | Snapshots                                  | `just snapshots`, then `cargo insta review`                                                                                  | insta snapshot acceptance                                                                                                                                                                                                                                                                                                                                             |
  | Dependencies                               | `just check-deps`                                                                                                            | `cargo deny` advisories                                                                                                                                                                                                                                                                                                                                               |
  | Linux-only `cfg` paths                     | `just clippy-linux`                                                                                                          | Clippy against `x86_64-unknown-linux-gnu`, when the target and cross compiler are installed                                                                                                                                                                                                                                                                           |
  | Judge or evaluation fixtures               | `just judge-corpus-check`, `just judge-bench-check`, `just eval-check`                                                       | Deterministic and offline. The live variants (`just judge-corpus`, `just judge-bench`, `just eval`) call model providers and need credentials and authorized spend                                                                                                                                                                                                    |
  | Rust-version pin                           | `just rust-version-check`                                                                                                    | The toolchain pin stays synchronized across its files. Also inside `just check`                                                                                                                                                                                                                                                                                       |

`just check` is the fast gate and is what the pre-push route runs for code and mixed pushes. GitHub CI remains the source of truth for the platform sandbox tests and the Coverage job. `just check-full` is the complete local suite: `just check`, the Linux compatibility check, coverage/change risk, dependency advisories, Rust and Markdown documentation, and a release build. Run focused commands such as `cargo test <name>` before the gate.

Additional checks:

- Dependency changes: `just check-deps`; [Dependency and supply chain posture](docs/dependencies.md) is the authority for pin ownership and update review.
- Documentation-only changes: `just pre-push-docs` covers committed Markdown, plus `git diff --check`. No automated link checker exists; relative links were audited clean on 2026-09-02, see #222.
- Label changes: `just labels-check-file` (file validation, also CI), `just labels-check` (repo drift vs `.github/labels.yml`), `just labels` (apply), `just labels-prune` (delete unlisted labels).
- Instruction changes (AGENTS.md, `.agents/skills/`, guardrails, runbooks): `just lint-instruction-size` caps AGENTS.md, the one document loaded every session, reports the corpus, and also runs in `just check`. [Agent-facing instructions](docs/guardrails/agent-instructions.md) is the authority for what an added instruction must justify.
- Markdown gate scopes are intentional: the pre-commit hook checks formatting for changed Markdown and lint over the tracked corpus. The pre-push route checks both for changed living documents. `just docs-check` and CI check the tracked corpus, which is the whole corpus on a clean CI checkout. This gives formatting feedback before commit without adding a full-corpus formatting pass to every commit; the full-corpus gates remain the final check.
- `just pre-push-docs` measures the committed changes between the base and `HEAD` and prints that range, plus a note when worktree Markdown sits outside it, so an empty file list cannot read as a pass. Untracked Markdown becomes checkable the moment it is staged; `just docs-check` covers the tracked corpus, which is what the gates own. Neither gate looks at untracked files, and neither rewrites them.

### Pre-push routing

The pre-push hook routes by changed path class instead of running the full local validation suite unconditionally.

- Markdown-only pushes run `just pre-push-docs`: targeted `panache format --check` and `panache lint` on the changed living documents, plus `git diff --check`.
- Pushes touching `src/`, `tests/`, `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `.cargo/`, `.github/workflows/`, `justfile`, `scripts/`, or `ci/` run the fast `just check` gate, which includes the complexity ratchet and the script fixture suites.
- Mixed pushes run both.
- Anything unclassified or unresolvable fails closed to `just check-full`.
- `just pre-push-force` always runs `just check-full`; `just pre-push-classify` prints the classification for the current branch.

[Working on branches and worktrees](docs/runbooks/parallel-worktrees.md) covers how the base is resolved and why the gate follows the checkout rather than the pushed ref.

If an applicable check cannot run, report the exact reason and the narrower checks that did run. Several checks need no build, coverage pass, or network, so a blocked prerequisite rarely means no evidence at all: `just cc-check`, `just check-scripts`, `just docs-check`, and `just pre-push-classify` cover complexity, the Python tooling, Markdown, and the change classification. [Local gate prerequisites](docs/runbooks/local-gate-prerequisites.md) carries the recovery for each prerequisite (an unaccepted toolchain license, a missing tool, a missing target, a missing credential). Do not describe a failing primary branch as unrelated without investigating it.

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

Commit and push freely on a feature branch, and commit often --- uncommitted work is the fragile state. Stage the paths you changed rather than `git add -A`, so unrelated in-flight edits stay out of your pull request. Ask first before force-pushing (it discards history); open the pull request when the work is ready for review.

Repository changes happen on a branch, which is what makes that safe: `master` is protected by a GitHub ruleset rejecting direct pushes, and by the `branch-guard` hook in `prek.toml`, which rejects commits and pushes on `master` at both `pre-commit` and `pre-push`. Branch names use the commit type as a prefix, such as `feat/turn-limits` or `fix/sandbox-read-only`. [Working on branches and worktrees](docs/runbooks/parallel-worktrees.md) covers the mechanics, including running several branches at once in linked worktrees.

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

Explain the user-visible or maintainer-visible outcome, notable design choices, compatibility or security impact, and exact verification. Link the managed task or ADR when one exists. Open the pull request only when the change is ready for review: acceptance notes, verification, documentation assessment, and any ExecPlan archival must already be complete. The pull request is then handed off for review; do not merge or enable auto-merge without explicit user approval. Use `Closes #<number>` for the managed issue; GitHub closes it when the pull request reaches `master`, after which the issue may receive its final delivered/verified summary. Update documentation only when its authority is affected.
