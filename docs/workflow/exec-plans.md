# Execution Plans (ExecPlans)

An ExecPlan is a design document a coding agent can follow to deliver a working feature. Write for a reader who has only the current working tree and this one file: no memory of prior plans, no external context.

Large or cross-cutting work requires one. So does smaller work that is substantially uncertain.

## In this repository

In-progress plans live in `docs/exec-plans/active/`. When a plan is complete and its `Outcomes & Retrospective` is filled in, move it to `docs/exec-plans/completed/` with `git mv`. The directories are the index; no separate listing is maintained.

When a plan completes an issue, follow the lifecycle in [tasks.md](tasks.md). Before opening the pull request, fill the issue's acceptance notes, update the plan's `Outcomes & Retrospective`, and move the plan to the completed bucket. If the issue body links to the plan, update that link after the pull request merges.

## Requirements

- **Self-contained.** In its current form the plan contains everything a novice needs to succeed. Do not write "as defined previously" or "see the architecture doc" --- include the explanation here, even at the cost of repeating yourself. Do not link external blogs or docs; if knowledge is required, put it in the plan in your own words. If a plan builds on a prior plan that is checked in, incorporate it by reference; if it is not checked in, include the relevant context.
- **Living.** Revise it as work proceeds, as discoveries occur, and as decisions are made. Each revision must remain self-contained. It must always be possible to restart from only the plan.
- **Outcome-focused.** Produce demonstrably working behavior, not code that satisfies a definition. State what someone can do afterward that they could not do before, and how to see it working.
- **Plain.** Define every term of art in plain language, or do not use it.

While implementing, resolve ambiguities autonomously and record the choice. Do not stop to ask for next steps; proceed to the next milestone.

## Writing the plan

Purpose comes first: explain in a few sentences why the work matters from a user's perspective, then give the exact steps --- what to edit, what to run, what to observe.

Name files by full repository-relative path, and name functions and modules precisely. When you give a command, give the working directory and the exact command line.

Phrase acceptance as behavior a human can verify ("navigating to `http://localhost:8080/health` returns HTTP 200 with body OK"), not as internal attributes ("added a `HealthCheck` struct"). For an internal change, show tests that fail before and pass after. Include expected output so a novice can tell success from failure.

Write steps that can be run twice without damage. If a step can fail halfway, say how to retry. If it is destructive, spell out the backup or fallback.

Resolve ambiguity in the plan itself and explain the choice. Over-explain user-visible effects; under-specify incidental implementation details.

## Formatting

Write in prose. Prefer sentences over lists. Checklists are permitted only in `Progress`, where they are required.

An ExecPlan is a single fenced block labeled `md`. Do not nest triple-backtick fences inside it --- present commands, transcripts, and diffs as indented blocks instead, so the outer fence does not close early. When the file's entire content is the plan, omit the outer backticks.

## Milestones

Milestones are narrative. Introduce each with a paragraph giving its scope, what will exist at the end that did not exist before, the commands to run, and the acceptance you expect. Goal, work, result, proof.

Each milestone must be independently verifiable and must advance the overall goal. Milestones tell the story; `Progress` tracks granular work. Both are required.

Explicit prototyping milestones are encouraged when they de-risk a larger change --- a toy implementation that proves a library behaves as needed, for example. Label the scope as prototyping, say how to run it, and state the criteria for promoting or discarding it. Parallel implementations are fine during a migration when they keep tests passing; describe how to validate both paths and retire one safely.

## Skeleton

```
## <Short, action-oriented description>

This ExecPlan is a living document, maintained per docs/workflow/exec-plans.md.
The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes &
Retrospective must be kept current as work proceeds.

## Purpose / Big Picture

What someone gains after this change and how they can see it working.

## Progress

Checkboxes for granular steps, reflecting actual current state. Every stopping
point appears here, splitting a partial task into done and remaining if needed.
Timestamps show the rate of progress.

- [x] (2025-10-01 13:00Z) Example completed step.
- [ ] Example partially completed step (completed: X; remaining: Y).

## Surprises & Discoveries

Unexpected behavior, bugs, or insights found while implementing, with concise
evidence. Test output is ideal.

- Observation: ... Evidence: ...

## Decision Log

- Decision: ... Rationale: ... Date/Author: ...

## Outcomes & Retrospective

What was achieved, what remains, lessons learned, measured against the purpose.

## Context and Orientation

The current state, as if the reader knows nothing. Key files by full path.
Every non-obvious term defined. No reference to prior plans.

## Plan of Work

In prose, the sequence of edits: for each, the file, the location, and what
changes.

## Concrete Steps

Exact commands and working directories, with short expected transcripts.

## Validation and Acceptance

How to exercise the system and what to observe, phrased as behavior.

## Idempotence and Recovery

Which steps are safe to repeat, and the retry or rollback path for those
that are not.

## Artifacts and Notes

The transcripts, diffs, or snippets that prove success, as indented examples.

## Interfaces and Dependencies

The libraries, modules, types, and signatures that must exist by the end,
using stable paths such as `crate::module::function`, and why each was chosen.
```

## Revising a plan

Reflect a change across every affected section, including the living sections, and add a note at the bottom recording what changed and why. An ExecPlan describes the why as well as the what.
