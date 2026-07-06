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

See the [Guardrails index](guardrails/index.md) for the full list of guardrails and their descriptions.

## Design docs

See the [Design docs index](design-docs/index.md) for the full list of design documents and their descriptions.
