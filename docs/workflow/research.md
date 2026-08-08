# Research Notes Workflow

This document explains how research artifacts are handled in this repository. When you are asked to create, update, organize, or use research, read this document first, then use the Notion Research database as the map and open the relevant notes directly.

Research is evidence, not an authoritative decision or implementation contract. Architectural decisions belong in ADRs, actionable scope belongs in GitHub issues, and broad implementation guidance belongs in ExecPlans.

## Research Storage

Research lives in the **Research** database in Notion (under the Cake page), not in this repository:

- Cake page (hub): https://app.notion.com/p/Cake-3b630bc66cc781809af4d98454822ce6
- Research database: https://app.notion.com/p/e4b33f5fa8cb438e93dd85727b26e928
- Research Home (workflow summary): https://app.notion.com/p/Research-Home-3b630bc66cc7812d9298c5172428a7c7

Each note is a database page. The **Type** property mirrors the former `docs/research/` subdirectories and is the primary map:

- `Inbox` for raw ideas, pasted notes, and thin captures that have not been triaged.
- `Investigations` for project-specific findings from debugging, profiling, code reading, session analysis, or behavior checks.
- `Sources` for notes from external articles, papers, documentation, tools, or open source repositories.
- `Topics` for synthesized, durable notes about an area of this project or an idea that may feed several tasks or plans.
- `Archived` for stale or superseded notes kept for historical reference.

The database views are the research map: *By type* groups the former subdirectories, *Inbox review* filters stale inbox notes for disposition, and *Active* / *Archived* filter by status. Always open the note itself before relying on it.

Notes moved from the repository carry a provenance footer (`Source: docs/research/... · moved from the cake repository to Notion on 2026-08-08`).

## Creating Research

Put rough, untriaged material in `Inbox` unless the user or context clearly identifies a better location. Prefer a short, descriptive title.

Create durable research (investigation, source, or topic notes) when factual or technical investigation should survive the current session, needs to inform more than one task, plan, or ADR, or supplies evidence for an architectural decision or cross-cutting implementation guidance. Keep brief, task-specific code reading and implementation observations in the issue or ExecPlan rather than creating research note churn; not every question during implementation needs a durable record.

Set these database properties on durable research notes. Raw inbox notes may leave them unset.

  | Property                      | Values                                           |
  | ----------------------------- | ------------------------------------------------ |
  | Type                          | Inbox, Investigations, Sources, Topics, Archived |
  | Status                        | inbox, active, synthesized, superseded, archived |
  | Confidence                    | low, medium, high                                |
  | Created / Updated             | ISO dates                                        |
  | Related tasks / Related plans | Free text linking issues or ExecPlans            |

## Using Research

Research is not automatically authoritative. Before using a research note to justify implementation work, check its status, date, confidence, evidence, and whether a newer task, ADR, ExecPlan, or source file supersedes it.

Research evidence feeds architectural decisions (ADRs), actionable work (issues), or broad implementation guidance (ExecPlans). Route research findings according to their nature:

- **ADRs** are authoritative for architectural decisions. Feed evidence to an ADR when a finding shapes a durable design choice, security boundary, configuration contract, or other architecturally significant decision.
- **Issues** are authoritative for actionable scope. Create or link an issue when a finding implies concrete, scoped work.
- **ExecPlans** are authoritative for implementation plans. Promote broad or cross-cutting findings to an ExecPlan.

Research itself is evidence, not a decision or contract. Preserve uncertainty and open questions in research notes rather than presenting guesses as settled facts.

Research should usually flow from rough capture to durable project work:

```text
inbox note -> investigation/source/topic synthesis -> ADR, issue, or ExecPlan -> completed artifact
```

Inbox notes must eventually receive a disposition. When reviewing notes with Type = Inbox, choose one of these outcomes for each stale note: promote useful synthesis to `Topics`, create an issue for actionable work, or delete material that has no continuing value. Reviews report age and staleness but never choose or apply the disposition automatically.

## Updating Research

When a note becomes stale, do not silently delete useful context. Set its Status to `superseded` or move it to `Archived`, and add a short note explaining what replaced it.
