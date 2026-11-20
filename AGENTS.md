# Agent Instructions

## Project

Cake is a Rust 2024 binary-only AI coding assistant CLI with sandboxed tool execution, persisted sessions, and OpenAI-compatible Chat Completions and Responses backends.

Users depend on CLI shape and exit behavior, machine-readable output, tool and sandbox semantics, session records, hook and toolbox protocols, settings precedence, and prompt construction. Treat those as compatibility surfaces; preserve them unless the task explicitly changes them.

## Operating loop

1. Classify the change, pick the route below, and load only that route's documents.
2. Decide whether the task edits repository files. Read-only audits, research, recommendations, and backlog inspection stay on the current branch with no branch, commit, GitHub, or pull-request side effects unless requested. If edits are needed, work on `master` for a small or single-task change; use `just branch <type>/<slug>`, or `just worktree <type>/<slug>` beside another agent, when the change needs isolation.
3. Read the smallest relevant code. [ARCHITECTURE.md](ARCHITECTURE.md) names the code authority for each surface.
4. Keep the diff narrow: no mixing behavior changes, dependency updates, formatting, or unrelated cleanup.
5. Build, format, and lint what you changed. There is no test suite; do not reintroduce one.
6. Integrate locally and push `master` when edits are complete; then hand off. Do not open a GitHub pull request unless the user explicitly asks for one; external contributors use a branch and pull request instead. For a pull request, do not merge, auto-merge, close the PR, or delete the remote branch without explicit user approval.

## Verification: no tests

This repository does not write, keep, or run tests. Do not create test files, test modules, test fixtures, snapshot tests, or test-only helpers. Do not run `cargo test`, `cargo nextest`, coverage, or any test harness. If you encounter a test while working in a file, delete it rather than updating it.

Verify a change by building and linting:

- `cargo check --all-targets`
- `just check` (format check, complexity gate, strict Clippy, and the import, dependency-direction, module-size, and glossary lints)
- `just build` for a release build when behavior changed
- `just docs-check` for Markdown changes

Report the exact commands you ran and anything you could not run, and why.

## Workflow routing

Load the matching route. Each document named is its surface's authority.

### CLI, output formats, and exit behavior

Flags, defaults, `--help`, exit codes, stdout and stderr, completion JSON, stream JSON.

- [Integration contracts](docs/integrations.md), for exit codes and JSON shapes.
- `src/main.rs`, `src/cli/`, and `cake --help`, which are the authority for CLI shape.

### Tools, scheduling, and model-visible errors

Tool schemas, execution semantics, per-path scheduling, and the errors a model sees.

- [ARCHITECTURE.md](ARCHITECTURE.md), for the agent and tool boundary.
- `src/clients/tools/` and its `*-description.txt` files, which are the authority for schemas and model-visible text.

### Sandbox, filesystem access, and trusted extensions

Sandbox policies, allowed paths, command policy, trusted hook and toolbox executables, and the hook and toolbox wire protocols.

- [Security and trust boundaries](docs/security.md), the authority for the trust boundary.
- [Integration contracts](docs/integrations.md), for the hook and toolbox wire protocols.
- Use the `debugging-cake-sandbox` skill for denials in practice.

Sandboxing is default-on and fails closed. Security-boundary work requires that document's impact analysis and platform verification.

### Providers, agent loop, and request shaping

Backends, wire formats, retries, headers, interrupts, and agent-loop control flow.

- [ARCHITECTURE.md](ARCHITECTURE.md), for the conversation and backend boundary.
- Use the `debugging-cake` skill for agent-loop failure triage.
- `src/clients/`, which is the authority for wire examples.

### Settings, profiles, skills, and prompt construction

Settings keys and precedence, profiles, model selection, skills, AGENTS.md discovery, system prompts.

- [Configuration](docs/configuration.md).
- `src/config/` and `src/prompts/`, which are the authority for resolution order and prompt assembly.

### Sessions, persistence, and telemetry

Session JSONL, record semantics, resume, and telemetry sidecars.

- [Integration contracts](docs/integrations.md), for layout and record semantics.
- Use the `analyzing-cake-sessions` skill for reviewing a session.
- `src/types/session.rs`, which is the authority for serialized records.

### Documentation and agent instructions

Prose changes, whether they document a surface or change how an agent behaves.

- [Agent-facing instructions](docs/guardrails/agent-instructions.md), when the prose exists to change how an agent behaves.
- [CONTRIBUTING.md](CONTRIBUTING.md), for the documentation checks.
- The route for the surface, when the document states a contract or invariant.

A change touching code, configuration, or a fixture is not documentation-only, whatever its prose ratio.

### Dependencies, build, verification, and release

`Cargo.toml`, `Cargo.lock`, the `justfile`, CI, toolchain, and release work.

- [CONTRIBUTING.md](CONTRIBUTING.md), the command catalog.
- [Dependency posture](docs/dependencies.md), [Automation conventions](docs/automations/README.md), [Branches and worktrees](docs/runbooks/parallel-worktrees.md), and [CI runner images](docs/runbooks/ci-runner-images.md).

A workflow job name identifies the check in the GitHub UI and in `gh` output; keep it stable unless the change intends to rename the check.

### Code quality and complexity

Complexity targets, the cyclomatic-complexity ratchet, and the module-size report. Relevant when writing or modifying functions.

- [Code complexity targets](docs/guardrails/complexity-targets.md), the authority; `just cc-check` is the gate.

## Repository rules

- Commit and push freely on `master` or a feature branch for authorized implementation work; commit often. Stage specific paths, never `git add -A`. Ask before force-pushing.
- A task that uses a branch gets one branch, cut from an up-to-date `master`.
- Preserve unrelated user changes; never clean or revert them.
- Use Conventional Commits, scoped only from the `cog.toml` allowlist; verified by the commit-msg hook. See [CONTRIBUTING.md](CONTRIBUTING.md) for scope selection.
- Future work and unresolved questions belong in the response or the issue tracker, not in new durable records.

## Integration and pull requests

Integrate maintainer work locally: use `master` directly for a small change, or a local branch for isolation, then fast-forward or squash-merge it into `master`, push, and delete the branch. Do not open a GitHub pull request unless the user explicitly asks for one. External contributors push a branch and open a pull request per [CONTRIBUTING.md](CONTRIBUTING.md#integration-and-pull-requests).

When a pull request exists, state the change class and its compatibility impact. If it touches a compatibility surface named under [Project](#project), say which one and what preserves it. Label it with exactly one `type:*` and at least one `area:*` from `.github/labels.yml`; never invent or rename labels. Add a `risk:*` label when the change breaks a compatibility surface, depends on external service behavior, or touches a security boundary; those labels route review, so omitting one silently skips it. Do not merge, auto-merge, close the PR, or delete the remote branch without explicit user approval.

Record which checks ran and which did not; a skipped check belongs in the handoff with its reason.

## Final Handoff Instructions

When you finish a user request, give a concise handoff that helps the user decide what to do next.

Include:

1. **What changed** --- the concrete files, behavior, commands, or docs changed. Don't narrate implementation detail unless it affects future work.
2. **What was verified** --- the exact build, lint, and docs commands run. If a relevant check was skipped or failed, say exactly why.
3. **What remains** --- known risks, incomplete work, skipped cleanup, TODOs, or assumptions. If nothing remains, say so plainly.
4. **Next actions** --- only actionable next steps, separated into required and optional. Don't invent work to sound thorough.
5. **Worktree state** --- remaining uncommitted or untracked files when useful; a requested commit's hash and whether the worktree is clean.

Style rules:

- Respond in 2 to 3 sentences, unless the what you need to communicate requires more exposition to be clearly communicated.
- Lead with outcomes, not effort.
- Use file references when they help.
- Don't include generic praise, filler, or "let me know if..." endings.
- Don't hide failures or skipped verification.
