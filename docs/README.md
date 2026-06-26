# Cake Documentation

This directory holds durable project documentation. For agent-facing routing, start with [AGENTS.md](../AGENTS.md). For architecture and codemap, see [ARCHITECTURE.md](../ARCHITECTURE.md). For contributor setup and workflow, see [CONTRIBUTING.md](../CONTRIBUTING.md).

## Where to look

  | You want to…                                | Read                                                               |
  | ------------------------------------------- | ------------------------------------------------------------------ |
  | Understand the domain model and terminology | [DOMAIN.md](DOMAIN.md)                                             |
  | Understand the architecture and find code   | [ARCHITECTURE.md](../ARCHITECTURE.md)                              |
  | Change code safely (agent-facing rules)     | [guardrails/](guardrails/) — pick the one that matches your change |
  | Deep-dive on a subsystem                    | [design-docs/](design-docs/)                                       |
  | Understand a past architectural decision    | [adr/](adr/)                                                       |
  | Reference an API integration detail         | [references/](references/)                                         |
  | Set up a dev environment or run tests       | [CONTRIBUTING.md](../CONTRIBUTING.md)                              |
  | Audit or update documentation itself        | [guardrails/documentation.md](guardrails/documentation.md)         |

## Directory layout

```
docs/
├── README.md              # This file — documentation index
├── DOMAIN.md              # Core concepts and glossary
├── guardrails/            # Agent-facing risk rules, one per change surface
├── design-docs/           # Subsystem design documents
├── adr/                   # Architecture Decision Records
└── references/            # Stable API reference material
```

## Guardrails

Guardrails are short, agent-facing checklists for risky change surfaces. Each one covers scope, compatibility surfaces, required checks, and common failure modes. Read the one routed by [AGENTS.md](../AGENTS.md) before making changes.

  | Guardrail                                                                                                  | Use for                                              |
  | ---------------------------------------------------------------------------------------------------------- | ---------------------------------------------------- |
  | [Agent Loop, Tools, And Tool Execution](guardrails/agent-loop-tools-and-tool-execution.md)                 | Agent loop, tool schemas, tool dispatch, transcripts |
  | [CLI And User Output](guardrails/cli-and-user-output.md)                                                   | Flags, exit codes, help text, progress display       |
  | [Dependencies, Build, CI, And Release](guardrails/dependencies-build-ci-release.md)                        | Deps, toolchain pins, CI, release artifacts          |
  | [Documentation](guardrails/documentation.md)                                                               | Doc updates, generated indexes, ADRs                 |
  | [Implementation Quality](guardrails/implementation-quality.md)                                             | Refactors, error handling, lint posture              |
  | [Prompts, Skills, And Hooks](guardrails/prompts-skills-and-hooks.md)                                       | System prompts, AGENTS.md loading, skills, hooks     |
  | [Providers, Models, And Settings](guardrails/providers-models-and-settings.md)                             | API backends, retry, model config, settings TOML     |
  | [Sandboxing And Filesystem Boundaries](guardrails/sandboxing-and-filesystem-boundaries.md)                 | Seatbelt, Landlock, path validation, command safety  |
  | [Sessions, Resume, And Machine-Readable Output](guardrails/sessions-resume-and-machine-readable-output.md) | JSONL sessions, resume/fork, telemetry, stream-json  |

## Design docs

  | Doc                                                                        | Topic                                    |
  | -------------------------------------------------------------------------- | ---------------------------------------- |
  | [cli.md](design-docs/cli.md)                                               | CLI design and command structure         |
  | [conversation-types.md](design-docs/conversation-types.md)                 | ConversationItem type system             |
  | [prompts.md](design-docs/prompts.md)                                       | System prompt construction               |
  | [session-management.md](design-docs/session-management.md)                 | Session lifecycle and JSONL format       |
  | [sandbox.md](design-docs/sandbox.md)                                       | OS-level sandbox implementation          |
  | [streaming-json-output.md](design-docs/streaming-json-output.md)           | Machine-readable output schema           |
  | [logging.md](design-docs/logging.md)                                       | Logging architecture                     |
  | [tools.md](design-docs/tools.md)                                           | Tool framework (Bash, Read, Edit, Write) |
  | [settings.md](design-docs/settings.md)                                     | settings.toml loading and precedence     |
  | [skills.md](design-docs/skills.md)                                         | Skill discovery and activation           |
  | [hooks.md](design-docs/hooks.md)                                           | Command hooks                            |
  | [api-retry-strategy.md](design-docs/api-retry-strategy.md)                 | Retry policy and backoff                 |
  | [edit-tool-session-analysis.md](design-docs/edit-tool-session-analysis.md) | Edit-tool behavior analysis              |
