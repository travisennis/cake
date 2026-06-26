# Agent Instructions

## Project

cake is a Rust 2024 binary-only AI coding assistant CLI with sandboxed tool execution, persisted sessions, and OpenAI-compatible Chat Completions and Responses API backends.

Compatibility surfaces: CLI behavior, tool execution semantics, sandbox boundaries, session file formats, configuration shape, provider/backend behavior, streaming/output formats, and task workflow metadata. Preserve them unless the task explicitly changes them.

## Operating Loop

1. Do managed-work intake first:
   - If the request is about a task, ExecPlan, ADR, or research note, use `ahm` to understand that managed work item before choosing implementation docs.
   - If the request is directly about code, CLI behavior, tests, docs, build, release, or repo mechanics, skip `ahm` intake and classify the request directly.
2. Classify the concrete work by Workflow Routing.
3. Load only the routed docs needed for that concrete work.
4. State the selected route and loaded docs before editing or in handoff.
5. Preserve compatibility surfaces unless explicitly changed.
6. Keep edits surgical and verify according to risk.
7. Handoff with changes, checks, and remaining risk.

When this file conflicts with a specialized workflow doc for that workflow, the specialized doc wins.

## Workflow Routing

### Agent Loop, Tools, And Tool Execution

Use this workflow for agent loop control flow, tool schemas, tool dispatch, tool results, tool-call concurrency, and transcript behavior. Consult `docs/guardrails/agent-loop-tools-and-tool-execution.md`, [ARCHITECTURE.md](ARCHITECTURE.md), `docs/design-docs/tools.md`, and `docs/design-docs/conversation-types.md`. Preserve tool execution semantics unless explicitly changed.

### Sandboxing And Filesystem Boundaries

Use this workflow for Seatbelt, Landlock, path validation, allowed directories, command safety checks, and sandbox/network policy. Consult `docs/guardrails/sandboxing-and-filesystem-boundaries.md`, `docs/design-docs/sandbox.md`, `docs/design-docs/tools.md`, and [ARCHITECTURE.md](ARCHITECTURE.md). Call out security impact in the handoff.

### Sessions, Resume, And Machine-Readable Output

Use this workflow for persisted JSONL sessions, continue/resume/fork, telemetry, transcript records, and `stream-json` output. Consult `docs/guardrails/sessions-resume-and-machine-readable-output.md`, `docs/design-docs/session-management.md`, `docs/design-docs/streaming-json-output.md`, and `docs/design-docs/conversation-types.md`. Preserve session and machine-readable output compatibility.

### Providers, Models, And Settings

Use this workflow for Responses API, Chat Completions, OpenRouter/provider strategy, retry behavior, model config, settings TOML, and API request shaping. Consult `docs/guardrails/providers-models-and-settings.md`, `docs/design-docs/settings.md`, `docs/design-docs/api-retry-strategy.md`, and relevant backend code. Preserve provider/backend behavior unless explicitly changed.

### CLI And User Output

Use this workflow for command-line behavior, flags, exit codes, human-readable output, progress display, and help text. Consult `docs/guardrails/cli-and-user-output.md`, `docs/design-docs/cli.md`, `docs/design-docs/logging.md` when observability changes, and [CONTRIBUTING.md](CONTRIBUTING.md) for verification.

### Prompts, Skills, And Hooks

Use this workflow for system prompt construction, AGENTS.md loading, skill discovery/activation, and command hooks. Consult `docs/guardrails/prompts-skills-and-hooks.md`, `docs/design-docs/prompts.md`, `docs/design-docs/skills.md`, and `docs/design-docs/hooks.md`.

### Implementation Quality

Use this workflow for refactors, lint posture, error handling, maintainability, and behavior-preserving cleanup. Consult `docs/guardrails/implementation-quality.md`, [ARCHITECTURE.md](ARCHITECTURE.md) when module boundaries or invariants are unclear, and [CONTRIBUTING.md](CONTRIBUTING.md) for code style, verification, and local commands.

### Dependencies, Build, CI, Release

Do not update dependencies unless asked. Keep `Cargo.toml` and `Cargo.lock` consistent. Use the smallest feature set. Consult `docs/guardrails/dependencies-build-ci-release.md` and [CONTRIBUTING.md](CONTRIBUTING.md) for setup, verification, commands, PR workflow, and commit conventions.

### Documentation

For doc work, read `docs/guardrails/documentation.md` first. Also use it when behavior, config, architecture, workflow, or compatibility changes require doc updates.

### Managed Work Intake With `ahm`

`ahm` is for understanding and managing higher-order workflow records. It is not the implementation route. Use it first when the user asks about a managed work item, then return to Workflow Routing and choose the route for the actual change.

Use these entry points:

- Tasks: run `ahm context task`, inspect the relevant task with `ahm task ...`, and open the task file before editing.
- ExecPlans: run `ahm context plan` when the request or task calls for an ExecPlan.
- ADRs: run `ahm context adr` when the request or task calls for an ADR, and use `ahm adr` commands for lifecycle changes.
- Research: run `ahm context research` and use `.agents/.research/index.md` as the map when asked to create, update, organize, or use research.
- General session briefing: run `ahm context` only when asked for broad project context or when no narrower managed-work context applies.

After `ahm` intake, re-classify the discovered work under Workflow Routing. For example, a task about CLI flags still uses the CLI route; a task about sandbox policy still uses the sandboxing route; a task about prompt, skill, or hook behavior still uses the prompts, skills, and hooks route.

Never hand-edit generated task, research, ExecPlan, or ADR indexes. Update the source records and run the appropriate `ahm` command. Use `ahm task` commands for task state moves and `ahm adr` commands for ADR lifecycle changes.

Treat `ahm context` output as the canonical workflow guidance.

## Repository Rules

- Do not commit or push unless explicitly asked.
- Assume uncommitted changes may belong to the user. Do not revert, overwrite, or clean files you did not intentionally change.
- Before broad edits, inspect `git status --short`.
- Before final handoff, report remaining uncommitted or untracked files when relevant.
- When moving implementation between files or modules, update repository code maps and implementation-location references even if user-facing behavior is unchanged.

## Handoff

End with what changed, exact checks run, remaining risks or skipped checks, and actionable next steps. For commits, include hash, worktree cleanliness, and leftover changes.
