# Agent Instructions

## Project

cake is a Rust 2024 binary-only AI coding assistant CLI with sandboxed tool execution, persisted sessions, and OpenAI-compatible Chat Completions and Responses API backends.

Compatibility surfaces: CLI behavior, tool execution semantics, sandbox boundaries, session file formats, configuration shape, provider/backend behavior, streaming/output formats, and task workflow metadata. Preserve them unless the task explicitly changes them.

## Operating Loop

1. Classify the request before editing.
2. Load only the routed docs needed for that request.
3. Preserve compatibility surfaces unless explicitly changed.
4. Keep edits surgical and verify according to risk.
5. Handoff with changes, checks, and remaining risk.

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

For doc work, read `.agents/DOCS.md` and `docs/guardrails/documentation.md` first. Also use them when behavior, config, architecture, workflow, or compatibility changes require doc updates.

### Workflow Overlays

These overlays do not replace the specific workflow routes above. Use them first
to identify or manage the work item, then re-classify the concrete task and load
the relevant routed workflow docs before editing.

When asked to create, choose, update, or work on a task, read `.agents/TASKS.md`,
inspect the task with `ahm task ...`, open the task file, then return to
Workflow Routing and choose the specific route or routes required by the task
content. When a task, workflow doc, or user request calls for an ExecPlan, read
`.agents/PLANS.md`. When one calls for an ADR, read [docs/adr/README.md](docs/adr/README.md).
When asked to create, update, organize, or use research, read `.agents/RESEARCH.md`,
then use `.agents/.research/index.md` as the map.

## Repository Rules

- Do not commit or push unless explicitly asked.
- Assume uncommitted changes may belong to the user. Do not revert, overwrite, or clean files you did not intentionally change.
- Before broad edits, inspect `git status --short`.
- Before final handoff, report remaining uncommitted or untracked files when relevant.
- When moving implementation between files or modules, update repository code maps and implementation-location references even if user-facing behavior is unchanged.

## Handoff

End with what changed, exact checks run, remaining risks or skipped checks, and actionable next steps. For commits, include hash, worktree cleanliness, and leftover changes.
