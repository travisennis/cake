# Research Notes Workflow

This document explains how research artifacts are handled in this repository. When you are asked to create, update, organize, or use research, read this document first, then use the directory layout under `docs/research/` as the map and open the relevant source files directly.

Research is evidence, not an authoritative decision or implementation contract. Architectural decisions belong in ADRs, actionable scope belongs in GitHub issues, and broad implementation guidance belongs in ExecPlans.

## Research Storage

Research lives under `docs/research/` in this repository. The directory is intentionally lightweight: it should be easy to capture rough notes, but durable notes should make their status, source, and relationship to project work clear.

Use these subdirectories:

- `inbox/` for raw ideas, pasted notes, and thin captures that have not been triaged.
- `investigations/` for project-specific findings from debugging, profiling, code reading, session analysis, or behavior checks.
- `sources/` for notes from external articles, papers, documentation, tools, or open source repositories.
- `topics/` for synthesized, durable notes about an area of this project or an idea that may feed several tasks or plans.
- `archived/` for stale or superseded notes kept for historical reference.

The directory layout is the research map. Orient yourself with `ls`/glob, but always open the source research file before relying on a note.

## Creating Research

Put rough, untriaged material in `inbox/` unless the user or context clearly identifies a better location. Prefer a short, descriptive kebab-case filename.

Create durable research (investigation, source, or topic notes) when material factual or technical investigation should survive the current session, needs to inform more than one task, plan, or ADR, or supplies evidence for an architectural decision or cross-cutting implementation guidance. Keep brief, task-specific code reading and implementation observations in the issue or ExecPlan rather than creating research note churn; not every question during implementation needs a durable record.

Use this header for durable research documents when it is useful. Raw inbox notes may be shorter.

```md
# Title

Status: inbox | active | synthesized | superseded | archived
Created: YYYY-MM-DD
Updated: YYYY-MM-DD
Related tasks: -
Related plans: -
Confidence: low | medium | high

## Summary

## Notes / Evidence

## Implications for this project

## Follow-ups
```

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

Inbox notes must eventually receive a disposition. When reviewing `docs/research/inbox/`, choose one of these outcomes for each stale note: promote useful synthesis to `topics/`, create an issue for actionable work, or delete material that has no continuing value. Reviews report age and staleness but never choose or apply the disposition automatically.

## Updating Research

When a note becomes stale, do not silently delete useful context. Mark it `superseded` or move it to `archived/` with `git mv`, and add a short note explaining what replaced it.

When adding, moving, archiving, or renaming research files, use `git mv` so the move is recorded; no index regeneration is needed.
