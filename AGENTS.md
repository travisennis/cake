# Agent Instructions

## Project

Cake is a Rust 2024 binary-only AI coding assistant CLI with sandboxed tool execution, persisted sessions, and OpenAI-compatible Chat Completions and Responses API backends.

Users depend on CLI shape and exit behavior, machine-readable output formats, tool execution and sandbox semantics, persisted session records, hook and toolbox protocols, settings precedence, and prompt construction. Treat those as compatibility surfaces and preserve them unless the task explicitly changes them.

## Operating loop

1. Run `just brief` (or list open issues with `gh issue list --state open`) before any work.
2. If the request names an issue, ExecPlan, ADR, or research record, inspect it (`gh issue view <number>` or the file itself) before choosing implementation work.
3. Create the branch before the first edit: `just branch <type>/<slug>`, or `just worktree <type>/<slug>` when another agent is working in parallel. `master` is protected and rejects commits and pushes.
4. Classify the change, select the route below, and load only that route's documents.
5. Read the smallest relevant code and tests. [ARCHITECTURE.md](ARCHITECTURE.md) names the code authority for each surface.
6. Preserve compatibility unless the task explicitly changes it.
7. If work is managed, track it in a GitHub issue: set Status to In Progress when you start, and close the issue with acceptance notes before the commit that contains its implementation.
8. Make surgical edits and run risk-proportionate checks.
9. After implementation edits, run reviews in a subagent and address findings until the reviewer gives an all clear. If a third round reports findings of the same class, stop patching: report the finding class and the suspected design flaw, and escalate to a design decision.
10. Perform preflight.
11. Hand off the branch and its pull request, exact checks, skipped checks, and remaining risk.

Large or cross-cutting work requires an ExecPlan per [docs/workflow/exec-plans.md](docs/workflow/exec-plans.md).

## Workflow routing

### Managed Work: Issues, ExecPlans, ADRs, And Research

Use for choosing, preparing, and closing work; authoring execution plans; recording research evidence; and writing architecture decision records.

Consult:

- [Task workflow](docs/workflow/tasks.md), for the GitHub Issues lifecycle: queue selection, triage, the work procedure, and closing.
- [ExecPlan workflow](docs/workflow/exec-plans.md), for authoring execution plans.
- [ADR README](docs/adr/README.md), for when and how to write architecture decision records.
- [Research workflow](docs/workflow/research.md), for research note conventions.
- `docs/exec-plans/`, `docs/research/`, and `docs/adr/`, which are the authority for records.

### CLI, Output Formats, And Exit Behavior

Use for CLI flags, defaults, `--help`, exit codes, stdout and stderr, completion JSON, and stream JSON.

Consult:

- [Integration contracts](docs/integrations.md), for exit codes, completion JSON, and stream-json shape.
- [ARCHITECTURE.md](ARCHITECTURE.md), for the CLI and agent boundary.
- `src/main.rs`, `src/cli/`, and `cake --help`, which are the authority for CLI shape.

Machine-readable stdout carries only its declared JSON format.

### Tools, Scheduling, And Model-Visible Errors

Use for tool schemas, tool execution semantics, per-path scheduling, and the errors a model sees.

Consult:

- [ARCHITECTURE.md](ARCHITECTURE.md), for the agent and tool boundary and the tool invariants.
- [ADR 013](docs/adr/013-per-path-serialization-of-mutating-tool-calls.md), for serialization of mutating tool calls.
- [ADR 012](docs/adr/012-schema-constrained-final-output.md), for schema-constrained final output.
- `src/clients/tools/`, its `*-description.txt` files, and its snapshots, which are the authority for schemas and model-visible text.

No prose document owns tool schemas end to end. Treat the code, descriptions, and snapshots as the contract, and add focused tests for changes.

### Sandbox, Filesystem Access, And Trusted Extensions

Use for sandbox policies, allowed paths, command policy, and trusted hook or toolbox executables.

Consult:

- [Security and trust boundaries](docs/security.md), for the threat model, policies, enforcement layers, and what the sandbox does not restrict.
- [Debugging Sandbox Denials runbook](docs/runbooks/debugging-sandbox.md), for platform-specific operational diagnosis and recovery.
- [ADR 014](docs/adr/014-sandbox-policy-cli-flag.md), for the sandbox policy flag.
- [ADR 016](docs/adr/016-nested-seatbelt-sandbox-fallback.md), for the recognized nested-Seatbelt fallback.
- [ADR 015](docs/adr/015-declarative-command-policy.md), for declarative command policy.
- [ADR 017](docs/adr/017-trusted-executable-toolbox-tools.md), for trusted toolbox executables.

Sandboxing is default-on and availability failures fail closed. Security-boundary changes require explicit impact analysis and platform-specific verification.

Before editing a security boundary, enumerate the bypass classes you intend to defend against. A review-reported bypass class you did not enumerate is a signal to revisit the design, not to add another check.

### Providers, Agent Loop, And Request Shaping

Use for backends, wire formats, retries, headers, interrupts, and agent-loop control flow.

Consult:

- [ARCHITECTURE.md](ARCHITECTURE.md), for the conversation and backend boundary.
- [Integration contracts](docs/integrations.md), for provider retry behavior.
- [Debugging Failed Cake Runs runbook](docs/runbooks/debugging-cake.md), for reactive agent-loop failure triage.
- [ADR 001](docs/adr/001-agent-loop-architecture.md), for the agent loop.
- [ADR 008](docs/adr/008-structured-provider-headers.md), for structured provider headers.
- [ADR 011](docs/adr/011-interrupt-handling.md), for interrupt handling and graceful shutdown.
- `src/clients/` and its snapshots, which are the authority for wire examples.

Provider-specific behavior stays at provider and backend boundaries.

### Settings, Profiles, Skills, And Prompt Construction

Use for settings keys and precedence, profiles, model selection, skills, AGENTS.md discovery, and system prompts.

Consult:

- [Configuration](docs/configuration.md), for locations and precedence, models, filesystem access, skills, instructions, and hooks.
- [ADR 003](docs/adr/003-settings-profiles.md), for settings profiles.
- [ADR 002](docs/adr/002-agent-skills.md), for the skills system.
- `src/config/` and `src/prompts/`, which are the authority for resolution order and prompt assembly.

### Sessions, Persistence, And Telemetry

Use for session JSONL, record semantics, resume, and telemetry sidecars.

Consult:

- [Integration contracts](docs/integrations.md), for persisted-session layout and record semantics.
- [Analyzing Cake Sessions runbook](docs/runbooks/analyzing-cake-sessions/index.md), for evidence-backed review of a persisted session.
- [Debugging Failed Cake Runs runbook](docs/runbooks/debugging-cake.md), for reactive triage before deeper session analysis.
- [ADR 004](docs/adr/004-append-only-session-task-events.md), for append-only task events.
- [ADR 007](docs/adr/007-per-session-telemetry-sidecar.md), for the telemetry sidecar.
- `src/types/session.rs` and its snapshots, which are the authority for serialized records.

Session files are append-only and versioned. Serialized-format changes require compatibility analysis.

### Hook And Toolbox Protocols

Use for the hook protocol, hook effects, and the toolbox describe and execute contracts.

Consult:

- [Integration contracts](docs/integrations.md), for the hook and toolbox wire protocols.
- [Security and trust boundaries](docs/security.md), for the trusted-extension boundary.
- [ADR 005](docs/adr/005-command-hooks.md), for command hooks.
- [ADR 017](docs/adr/017-trusted-executable-toolbox-tools.md), for trusted toolbox executables.

Untrusted model actions never implicitly acquire the authority of trusted hooks or toolbox executables.

### Agent Instructions And Skills

Use for changes to this file, `.agents/skills/`, or any other prose whose purpose is to change how an agent behaves.

Consult:

- [Agent-facing instructions](docs/guardrails/agent-instructions.md), for the evidence a behavior-shaping edit requires.

### Dependencies, Build, Verification, And Release

Use for `Cargo.toml`, `Cargo.lock`, the `justfile`, CI, toolchain, and release work.

Consult:

- [CONTRIBUTING.md](CONTRIBUTING.md), the canonical command catalog and verification policy.
- [Dependency and supply chain posture](docs/dependencies.md), for pin ownership, tooling pins, and the review a dependency update requires.
- [Automation conventions](docs/automations/README.md), for scheduled maintenance, its reporting rules, and which surfaces each automation owns.
- [Working on branches and worktrees runbook](docs/runbooks/parallel-worktrees.md), for branch, worktree, and pull-request mechanics.
- [Auditing Binary Size runbook](docs/runbooks/auditing-binary-size.md), for investigating release binary bloat.

Dependency changes require explicit scope and `Cargo.toml`/`Cargo.lock` consistency. Classify a dependency update from its upstream diff, not from the repository-side diff or a green CI run.

### Code Quality, Complexity, And Coverage

Use for cyclomatic complexity targets, CRAP scores, coverage requirements, and the coverage-first refactoring workflow. Relevant when writing or modifying functions, or when reviewing code quality.

Consult:

- [Code complexity targets](docs/guardrails/complexity-targets.md), for CC and CRAP targets and the refactoring workflow.
- `cargo-crap` and `just cargo-crap-report`, for the CI CRAP gate.
- Issue #335, for enforcement mechanisms.

## Repository rules

- Do not commit or push unless explicitly asked.
- Work on a branch cut from an up-to-date `master`, never on `master` itself. Integration happens through a pull request. [Working on branches and worktrees](docs/runbooks/parallel-worktrees.md) has the mechanics.
- One branch holds one task. Do not carry unrelated work across on the same branch.
- Preserve unrelated user changes; never clean or revert them.
- Never resolve a merge conflict in `ci/cargo-crap-baseline.json` by hand or by three-way merge. Take `master`'s copy, then regenerate it with `just change-risk-baseline`.
- Close the GitHub issue tracking a change before the commit that contains its implementation; issue state, links, and sub-issues are the managed record.
- Use Conventional Commits when writing commit messages; verified by the commit-msg hook.
- Labels on GitHub issues and pull requests: use only the vocabulary in `.github/labels.yml`; never invent or rename labels. `just labels-check` verifies the repo matches it, and the label-governance workflow removes out-of-vocabulary labels from issues.
- Future work and unresolved questions belong in GitHub issues, not durable docs.
- Update architecture documentation only when a durable boundary or invariant changes, not when symbols or files move.
- Before broad edits and before handoff, inspect `git status --short`.

## Verification

Use focused checks first. `just ci` is the normal code-change gate; documentation-only work uses targeted Panache format/lint checks for changed living documents and link validation. Follow [CONTRIBUTING.md](CONTRIBUTING.md) for exceptions and specialized checks.
