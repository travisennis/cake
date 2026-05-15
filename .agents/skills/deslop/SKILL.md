---
name: deslop
description: Run a focused review-readiness pass on a nearly finished change before commit. Executes three sequential review passes (rules conformance, type safety, overengineering) to catch issues, then synthesize and apply the worthwhile fixes.
---

# Deslop

Use this skill after a change is functionally correct and before a commit or handoff. The PR, commit text, task notes, or final response should describe already-deslopped code, not code that still needs cleanup.

## Goals

Leave the smallest clear diff that still solves the issue.
Run multiple focused review passes instead of relying on one final subjective read.
Preserve behavior while improving readability, type safety, and alignment with cake's repo rules.

## Required context

Before reviewing, gather the context below. Read each item when it is relevant
to the changed surface. If an item is not relevant, explicitly mark it not
applicable in the compliance notes with a short reason.

- repo root `AGENTS.md`
- nested `AGENTS.md` files for the changed areas
- `.agents/TASKS.md`, `.agents/.tasks/index.md`, and the active task file when the work came from a task
- `.agents/PLANS.md`
- `docs/design-docs/index.md`
- any design doc directly relevant to the changed area
- any ADR directly relevant to the changed area, especially under `docs/adr/`
- the relevant active exec plan when one exists for the current work
- the changed files and enough nearby context to review them properly

If you're working on an ExecPlan, also inspect `.agents/exec-plans/active/`. When a plan clearly matches the current task, study it before reviewing as it often contains relevant context, constraints, and acceptance criteria not captured in the ticket or design docs.

## Compliance notes

Before claiming the deslop pass is complete, write a brief compliance note in
the working response, task notes, or review synthesis. Keep it concise, but make
each required context item auditable.

Use this shape:

```markdown
### Deslop compliance

- Root AGENTS.md: read
- Nested AGENTS.md: none found under changed paths
- Task context: read `.agents/TASKS.md`, `.agents/.tasks/index.md`, and task `NNN`
- Plans: not applicable because this was an S/M task with no ExecPlan
- Design docs: not applicable because this was a docs-only policy change with no matching design-doc area
- ADRs: not applicable because no ADR covers the changed area
- ExecPlan: not applicable because no active plan exists for this task
- Changed files and diff: reviewed `git diff --stat` and targeted changed-file diffs
- Validation: ran `just task-index-check`; full CI not required because only skill/docs files changed
```

Do not write blanket statements such as "no design docs to check" unless you
looked for a relevant design doc or can explain why the changed area has no
design-doc surface. Prefer targeted discovery such as `rg --files -g AGENTS.md`,
`rg --files docs/design-docs docs/adr`, and targeted `git diff -- <paths>` over
large file dumps.

## Review protocol

Run these three reviews sequentially, treating each as a clean pass with its own focus. Do not blur findings between passes.

### Pass 1: Rules and documentation conformance

- Are we following `AGENTS.md`, nested `AGENTS.md`, design docs, and core beliefs?
- Did we drift from documented repo patterns or ownership boundaries?
- If the work came from a task or ExecPlan, does the implementation match its acceptance notes and recorded decisions?
- Did we update task, ExecPlan, design doc, or ADR notes when the change discovered something durable?

### Pass 2: Type safety and source of truth

- Are we preserving canonical types?
- Did we clone, stringify, parse, or convert instead of carrying the existing typed value?
- Did we introduce `unwrap`, `expect`, broad `allow` attributes, stringly typed sentinels, or lossy `serde_json::Value` plumbing where a typed struct or enum should be used?
- Are `anyhow` and `thiserror` used in the same style as nearby code: `thiserror` for domain errors and `anyhow` for application-level context?
- Are fallible APIs explicit about failure, with useful context and without swallowing serialization, session, sandbox, or tool execution errors?
- Are async boundaries clear, with no blocking filesystem or process work added inside latency-sensitive Tokio paths unless nearby code already accepts it?
- Are OpenAI-compatible API boundaries, tool argument parsing, session records, and config files validated at the boundary and then represented with repo-owned types downstream?
- Could a mistake slip to runtime that Rust, serde, or a narrower enum/struct could catch at compile time?

### Pass 3: Overengineering and simplification

- Did we write more code than needed?
- Did we create helpers, abstractions, factories, wrappers, or indirection without enough payoff?
- Could the same result be expressed more directly?
- Are new modules, traits, builders, or generic helpers justified by real reuse or by an existing design boundary?
- Did we preserve the binary-only CLI shape instead of introducing library-style APIs or public surface area without a project reason?

After all three passes, synthesize findings into one balanced report with these headings:

- "How did we do?"
- "Feedback to keep"
- "Feedback to ignore"
- "Plan of attack"
- "Deslop compliance"

## Between-pass hygiene

Between each review pass, ground the pass in concrete local evidence. Use the narrowest checks that fit the change:

- `git diff --stat` and `git diff -- <paths>` to keep the review anchored to the actual changed surface.
- `cargo fmt --check` or `cargo fmt` when formatting is affected.
- `cargo test <module_or_test_name>` for focused Rust tests in the changed area.
- `cargo clippy --all-targets --all-features -- -D warnings` or `just clippy-strict` when lint behavior, public types, or shared code changed.
- `just ci` after code, config, or dependency changes are complete, as required by repo instructions.

For docs-only or skill-only edits, read the rendered Markdown structure and verify links/paths by inspection or `rg --files`; full CI is not required unless code, config, or dependency files changed.

## What to fix automatically

If you are in an unattended implementation flow, apply the worthwhile feedback immediately before commit. Prioritize:

- type drift, unnecessary cloning/string conversion, or duplicated type definitions
- violations of documented repo boundaries or design documents
- dead helpers, dead code, debug leftovers, placeholder text
- new `unwrap`, `expect`, `todo!`, `dbg!`, or broad lint suppressions
- errors that lack actionable context at CLI, API, session, sandbox, or tool boundaries
- unnecessary wrappers or indirection that can be removed locally without widening scope

If feedback is speculative, conflicts across passes, or would widen scope materially, leave it out and mention it briefly in the synthesis/workpad.

## Steps

1. Gather the context from Required Context, reading relevant items and recording non-applicable items with reasons.
2. Run Pass 1 (rules and docs conformance) and record findings.
3. Run a narrow evidence check such as `git diff --stat`, a focused `cargo test`, or `cargo fmt --check`.
4. Run Pass 2 (type safety) and record findings.
5. Run the next narrow evidence check that fits the risks found so far.
6. Run Pass 3 (overengineering/simplification) and record findings.
7. Synthesize all findings into the balanced report, including Deslop compliance.
8. Apply the worthwhile feedback that is clearly in scope.
9. Rerun the narrowest affected validation immediately, then run `just ci` when the finished work changed code, config, or dependencies.
10. Update task notes, ExecPlan notes, commit text, PR-facing text, or final response so they describe the final post-deslop state rather than the earlier draft state.

## Stop rules

- Do not turn this into a refactor unrelated to the ticket.
- Do not churn stable code outside the changed area just to make it prettier.
- If a cleanup is subjective and not clearly better, leave it alone.
- Do not blindly apply every finding from every pass.
- Do not run broad or slow checks repeatedly when a focused test already covers the current pass; save `just ci` for final validation after code/config/dependency changes.
