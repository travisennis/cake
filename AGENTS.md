# Agent Instructions

## Project

Cake is a Rust 2024 binary-only AI coding assistant CLI with sandboxed tool execution, persisted sessions, and OpenAI-compatible Chat Completions and Responses backends.

Users depend on CLI shape and exit behavior, machine-readable output, tool and sandbox semantics, session records, hook and toolbox protocols, settings precedence, and prompt construction. Treat those as compatibility surfaces; preserve them unless the task explicitly changes them.

## Operating loop

1. Classify the change, pick the route below, and load only that route's documents.
2. Decide whether the task will edit repository files. For read-only work such as backlog grooming, research, audits, or recommendations, stay on the current branch; do not create a branch or worktree. If edits become necessary, create the branch immediately before the first edit: `just branch <type>/<slug>`, or `just worktree <type>/<slug>` beside another agent.
3. Read the smallest relevant code and tests. [ARCHITECTURE.md](ARCHITECTURE.md) names the code authority for each surface.
4. Keep the diff narrow: no mixing behavior changes, dependency updates, formatting, snapshot regeneration, or unrelated cleanup. Churn hides the change a reviewer needs to see.
5. Run checks proportionate to the risk, per [CONTRIBUTING.md](CONTRIBUTING.md), and preflight before handoff.
6. Track managed work in a GitHub issue and follow the lifecycle in [docs/workflow/tasks.md](docs/workflow/tasks.md).
7. Open the pull request when the work is ready for review, then stop and hand off. Do not merge, auto-merge, close the PR or issue, or delete the remote branch without explicit user approval.

Large or cross-cutting work requires an ExecPlan per [docs/workflow/exec-plans.md](docs/workflow/exec-plans.md).

## Workflow routing

Load the matching route. Each document named is its surface's authority and links the decisions behind it.

### Managed work: issues, ExecPlans, ADRs, research

Choosing, preparing, and closing work; execution plans; research evidence; architecture decisions.

- [Task workflow](docs/workflow/tasks.md), for the issue lifecycle.
- [ExecPlan workflow](docs/workflow/exec-plans.md), [ADR README](docs/adr/README.md), and [Research workflow](docs/workflow/research.md), for authoring each record.

### CLI, output formats, and exit behavior

Flags, defaults, `--help`, exit codes, stdout and stderr, completion JSON, stream JSON.

- [Integration contracts](docs/integrations.md), for exit codes and JSON shapes.
- `src/main.rs`, `src/cli/`, and `cake --help`, which are the authority for CLI shape.

### Tools, scheduling, and model-visible errors

Tool schemas, execution semantics, per-path scheduling, and the errors a model sees.

- [ARCHITECTURE.md](ARCHITECTURE.md), for the agent and tool boundary.
- `src/clients/tools/`, its `*-description.txt` files, and its snapshots, which are the authority for schemas and model-visible text.

No prose document owns tool schemas. The code, descriptions, and snapshots are the contract; add focused tests.

### Sandbox, filesystem access, and trusted extensions

Sandbox policies, allowed paths, command policy, trusted hook and toolbox executables, and the hook and toolbox wire protocols.

- [Security and trust boundaries](docs/security.md), the authority for the trust boundary.
- [Integration contracts](docs/integrations.md), for the hook and toolbox wire protocols.
- [Debugging sandbox denials](docs/runbooks/debugging-sandbox.md), for denials in practice.

Sandboxing is default-on and fails closed. Security-boundary work requires that document's impact analysis and platform verification.

### Providers, agent loop, and request shaping

Backends, wire formats, retries, headers, interrupts, and agent-loop control flow.

- [ARCHITECTURE.md](ARCHITECTURE.md), for the conversation and backend boundary.
- [Debugging failed runs](docs/runbooks/debugging-cake.md), for agent-loop failure triage.
- `src/clients/` and its snapshots, which are the authority for wire examples.

### Settings, profiles, skills, and prompt construction

Settings keys and precedence, profiles, model selection, skills, AGENTS.md discovery, system prompts.

- [Configuration](docs/configuration.md).
- `src/config/` and `src/prompts/`, which are the authority for resolution order and prompt assembly.

### Sessions, persistence, and telemetry

Session JSONL, record semantics, resume, and telemetry sidecars.

- [Integration contracts](docs/integrations.md), for layout and record semantics.
- [Analyzing sessions](docs/runbooks/analyzing-cake-sessions/index.md), for reviewing a session.
- `src/types/session.rs` and its snapshots, which are the authority for serialized records.

### Documentation and agent instructions

Prose changes, whether they document a surface or change how an agent behaves.

- [Agent-facing instructions](docs/guardrails/agent-instructions.md), when the prose exists to change how an agent behaves.
- [CONTRIBUTING.md](CONTRIBUTING.md), for the documentation checks.
- The route for the surface, when the document states a contract or invariant.

Documentation-only changes may skip the Rust gate and must say so in the pull request. A change touching any code, configuration, fixture, or snapshot is not documentation-only, whatever its prose ratio.

### Dependencies, build, verification, and release

`Cargo.toml`, `Cargo.lock`, the `justfile`, CI, toolchain, and release work.

- [CONTRIBUTING.md](CONTRIBUTING.md), the command catalog and verification policy.
- [Dependency posture](docs/dependencies.md), [Automation conventions](docs/automations/README.md), [Branches and worktrees](docs/runbooks/parallel-worktrees.md), and [CI runner images](docs/runbooks/ci-runner-images-and-required-checks.md).

A workflow job name is a branch-protection identifier; renaming one without updating the ruleset blocks every pull request.

### Code quality, complexity, and coverage

Complexity targets, CRAP scores, coverage, and the coverage-first refactoring workflow. Relevant when writing or modifying functions.

- [Code complexity targets](docs/guardrails/complexity-targets.md), the authority; `just cc-check` is the gate.

## Repository rules

- Commit and push freely on a feature branch; commit often. Stage specific paths, never `git add -A`. Open a pull request when the task is complete and the work is ready for review. Ask before force-pushing.
- One branch holds one task, cut from an up-to-date `master`.
- Preserve unrelated user changes; never clean or revert them.
- Never resolve a merge conflict in `ci/cargo-crap-baseline.json` by hand. Take `master`'s copy, then regenerate it with `just change-risk-baseline`.
- Use Conventional Commits, scoped only from the `cog.toml` allowlist; verified by the commit-msg hook. See [CONTRIBUTING.md](CONTRIBUTING.md) for scope selection.
- Future work and unresolved questions belong in GitHub issues, not durable docs.

## Pull requests

State the change class and its compatibility impact. If it touches a compatibility surface named under [Project](#project), say which one and what preserves it.

Label with exactly one `type:*` and at least one `area:*` from `.github/labels.yml`; never invent or rename labels. Add a `risk:*` label when the change breaks a compatibility surface, depends on external service behavior, or touches a security boundary; those labels route review, so omitting one silently skips it.

Record which checks ran and which did not; a skipped check belongs in the pull request with its reason. Opening a PR is the default handoff; merge only with explicit user approval.

## Final Handoff Instructions

When you finish a user request, give a concise handoff that helps the user decide what to do next.

Include:

1. **What changed**
   - State the concrete files, behavior, commands, docs, or tests changed.
   - Don't narrate every implementation detail unless it affects future work.

2. **What was verified**
   - List the exact checks run, such as `cargo test foo`, `cargo fmt`, `just check`, browser verification, etc.
   - If a relevant check was skipped or failed, say exactly why.

3. **What remains**
   - Name any known risks, incomplete work, skipped cleanup, failing tests, TODOs, or assumptions.
   - If nothing remains, say that plainly.

4. **Next actions**
   - Give only actionable next steps.
   - Separate required next steps from optional follow-ups.
   - Don't invent extra work just to sound thorough.

5. **Worktree state, when relevant**
   - If files were edited, mention remaining uncommitted or untracked files when useful.
   - If a commit was requested, include the commit hash and whether the worktree is clean.

Style rules:

- Be brief unless the change was complex.
- Lead with outcomes, not effort.
- Use file references when they help.
- Don't include generic praise, filler, or "let me know if..." endings.
- Don't hide failures or skipped verification.
