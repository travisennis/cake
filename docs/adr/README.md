# Architecture decision records

ADRs preserve the context, alternatives, and tradeoffs behind consequential decisions. They are historical records, not a second living specification of the implementation. They are not implementation plans; use ExecPlans for step-by-step delivery planning.

Browse decisions by reading the numbered files in this directory (`NNN-short-decision-title.md`). An `accepted` status means the decision was adopted at the recorded time; later ADRs, amendments, or the current implementation may refine it. A superseded decision remains in the archive and points to its replacement.

## When To Write An ADR

Write or update an ADR before implementation when an issue introduces or changes an architectural decision.

ADR-required triggers:

- `type:feature` issues that introduce or change user-visible behavior, persisted state, tool behavior, model-provider behavior, sandbox behavior, configuration shape, or another durable architectural contract.
- Security-sensitive changes, including command execution, filesystem access, network access, secrets, auth headers, logging redaction, sandbox boundaries, or permission escalation.
- Breaking changes, deprecations, migrations, compatibility changes, or changed default behavior.
- New major runtime dependencies that affect behavior, security posture, binary size, licensing, or platform support.
- Cross-platform behavior changes, especially macOS/Linux divergence.
- Substantial changes in an architecturally significant area of this repository (for example its core domain logic, public interfaces, persistence, or configuration shape).

ADRs are usually optional for localized bug fixes, tests, docs, small refactors, and implementation-only follow-through that does not create a new durable decision. When in doubt, prefer a short ADR over leaving an important decision implicit.

## Relationship To Issues And ExecPlans

- Create or update the ADR before code changes begin.
- Reference the ADR from the issue body or implementation notes.
- If the same issue requires an ExecPlan, the ExecPlan should cite the ADR and describe how it will implement the accepted decision.
- If implementation discovers that the decision needs to change, update the ADR before continuing.

## Numbering And Naming

Use the next available three-digit number and a short kebab-case title:

```text
docs/adr/NNN-short-decision-title.md
```

Allocate the next number from the highest existing ADR number. Keep existing numbers stable. Do not renumber ADRs after they are created or referenced.

## Status

Use one of these statuses, set in front matter:

  | Status                  | Meaning                                                                   |
  | ----------------------- | ------------------------------------------------------------------------- |
  | `proposed`              | The decision is being drafted or reviewed.                                |
  | `accepted`              | The decision is approved and should guide implementation.                 |
  | `rejected`              | The decision was considered and declined.                                 |
  | `deprecated`            | The decision is retained for history but should no longer guide new work. |
  | `superseded by ADR-NNN` | A later ADR replaces this decision entirely.                              |

Change an ADR's status by editing its front matter directly. When superseding an ADR, keep the old file on disk; the replacement ADR lists the superseded ADR in its `## More Information` section.

## Changing Existing Decisions

Treat ADRs as decision history, not living specifications. Do not delete or rewrite an old ADR just because a later decision changes direction.

When new evidence, requirements, or implementation experience changes an accepted decision, create a new ADR instead of editing the old decision in place. The old ADR should continue to describe the decision that was accepted at the time.

Create a new ADR when:

- A later decision reverses, replaces, or materially changes an accepted architectural boundary.
- The old decision was correct when made, but new requirements, constraints, or implementation evidence changed the tradeoff.
- Multiple issues or future contributors need a durable explanation of why the decision changed.

Update an existing ADR when:

- The decision itself is unchanged and the edit only clarifies wording, fixes stale references, or adds missing links.
- The ADR already anticipated the extension and the edit records detail without changing the accepted contract.
- A later ADR supersedes it; in that case, add a short supersession note with a link to the replacement ADR.

Full supersession is expressed by setting the old record's `status` to `superseded by ADR-NNN` and writing reciprocal body references. Use full supersession only when the new ADR fully replaces the old decision. Partial supersession (when only part of an older decision is replaced) is represented by keeping the old ADR's status as `accepted` and recording the partial replacement in the body, usually under `## More Information`. The new ADR should state which part of the older decision it supersedes and list the older ADR in its References.

## Template

Use a constrained MADR-profile ADR with scalar front matter and standard sections. The profile uses a subset of MADR 4.x:

- Front matter is `key: value` only (no YAML block lists, block scalars, or multi-line values).
- List-like fields (`decision-makers`, `consulted`, `informed`) use comma-separated scalar values.

Example:

```markdown
---
status: proposed
date: YYYY-MM-DD
decision-makers: Name, Name
consulted: Name
informed: issue NNN
---
# Short Decision Title

## Context and Problem Statement

Describe the problem, constraints, prior behavior, and forces that make a
decision necessary.

## Decision Drivers

- TODO

## Considered Options

- TODO

## Decision Outcome

Chosen option: TODO, because TODO.

### Consequences

- Good, because TODO.
- Bad, because TODO.

## More Information

- TODO
```

Create a new ADR by copying the template above into `docs/adr/NNN-short-decision-title.md`, filling the front matter and body, and adding the `status: proposed` record. ADR body prose is author-owned and is not rewritten by lifecycle tooling.

Current user behavior and compatibility semantics belong in [Configuration](../configuration.md), [Integrations](../integrations.md), [Security](../security.md), and [Architecture](../../ARCHITECTURE.md).
