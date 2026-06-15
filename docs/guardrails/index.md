# Guardrails

These docs are short, agent-facing checklists for risky change surfaces. Read
the one routed by `AGENTS.md`, then use the linked design docs for deeper
context.

| Guardrail | Use For |
| --- | --- |
| [Agent Loop, Tools, And Tool Execution](agent-loop-tools-and-tool-execution.md) | Agent loop control flow, tool schemas, tool dispatch, tool results, and transcript behavior |
| [CLI And User Output](cli-and-user-output.md) | Flags, help text, exit codes, progress output, and human-readable formatting |
| [Dependencies, Build, CI, And Release](dependencies-build-ci-release.md) | Dependency changes, build metadata, toolchain pins, CI, release-sensitive edits |
| [Documentation](documentation.md) | Durable docs, generated indexes, design docs, ADRs, and user-facing doc updates |
| [Implementation Quality](implementation-quality.md) | Refactors, error handling, lint posture, module boundaries, and maintainability |
| [Prompts, Skills, And Hooks](prompts-skills-and-hooks.md) | System prompts, AGENTS.md loading, skill discovery/activation, and hooks |
| [Providers, Models, And Settings](providers-models-and-settings.md) | API backends, provider shaping, retry behavior, model config, and settings TOML |
| [Sandboxing And Filesystem Boundaries](sandboxing-and-filesystem-boundaries.md) | Seatbelt, Landlock, path validation, allowed directories, and command safety |
| [Sessions, Resume, And Machine-Readable Output](sessions-resume-and-machine-readable-output.md) | JSONL sessions, resume/continue/fork, telemetry, JSON output, and transcript records |
