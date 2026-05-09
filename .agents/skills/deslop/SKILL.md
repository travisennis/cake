---
name: deslop
description: Run a focused review-readiness pass on a nearly finished change before commit. Executes three sequential review passes (rules conformance, type safety, overengineering) to catch issues, then synthesize and apply the worthwhile fixes.
---

# Deslop

Use this skill after the change is functionally correct and before `commit`. The PR should be describing already-deslopped code, not code that still needs cleanup.

## Goals

Leave the smallest clear diff that still solves the issue.
Run multiple focused review passes instead of relying on one final subjective read.
Preserve behavior while improving readability, type safety, and alignment with repo rules.

## Required context

Before reviewing, read:

- repo root `AGENTS.md`
- nested `AGENTS.md` files for the changed areas
- `.agents/PLANS.md`
- `docs/index.md`
- `docs/design-docs/index.md`
- any design doc directly relevant to the changed area
- the relevant active exec plan when one exists for the current work
- the changed files and enough nearby context to review them properly

If you're working on an ExecPlan, also inspect `.agents/exec-plans/active/`. When a plan clearly matches the current task, study it before reviewing as it often contains relevant context, constraints, and acceptance criteria not captured in the ticket or design docs.

## Review protocol

Run these three reviews sequentially, treating each as a clean pass with its own focus. Do not blur findings between passes.

### Pass 1: Rules and documentation conformance

- Are we following `AGENTS.md`, nested `AGENTS.md`, design docs, and core beliefs?
- Did we drift from documented repo patterns or ownership boundaries?

### Pass 2: Type safety and source of truth

- Are we preserving canonical types?
- Did we cast, redefine existing types, widen things unnecessarily, or break inference flow?
- Could a mistake slip to deploy time instead of build time?
- Prefer compile-time guarantees over runtime defensive programming inside typed repo-owned code. Validate or parse only at untrusted boundaries. Once data has crossed a validator-owned boundary or is carried by an inferred repo-owned type, trust it downstream and do not re-parse it.
- Use boundary validation only. Do not add defense-in-depth validation inside internal TypeScript helpers unless the input is truly untrusted or the operation is irreversible.

### Pass 3: Overengineering and simplification

- Did we write more code than needed?
- Did we create helpers, abstractions, factories, wrappers, or indirection without enough payoff?
- Could the same result be expressed more directly?

After all three passes, synthesize findings into one balanced report with these headings:

- "How did we do?"
- "Feedback to keep"
- "Feedback to ignore"
- "Plan of attack"

## Between-pass hygiene

Between each review pass, use `pnpm -w lint:slop:delta` (or equivalent) to identify the biggest regressions and improvements. Run any other narrow local checks you need. This keeps each pass grounded in concrete data while you mentally reset for the next focus area.

## What to fix automatically

If you are in an unattended implementation flow, apply the worthwhile feedback immediately before commit. Prioritize:

- type drift, casting, or duplicated type definitions
- violations of documented repo boundaries or design documents
- dead helpers, dead code, debug leftovers, placeholder text
- unnecessary wrappers or indirection that can be removed locally without widening scope

If feedback is speculative, conflicts across passes, or would widen scope materially, leave it out and mention it briefly in the synthesis/workpad.

## Steps

1. Gather the context (read everything listed in Required Context).
2. Run Pass 1 (rules and docs conformance) and record findings.
3. Run `pnpm -w lint:slop:delta` and any other narrow checks.
4. Run Pass 2 (type safety) and record findings.
5. Run `pnpm -w lint:slop:delta` again to check for regressions.
6. Run Pass 3 (overengineering/simplification) and record findings.
7. Synthesize all findings into the balanced report.
8. Apply the worthwhile feedback that is clearly in scope.
9. Rerun the narrowest affected validation immediately.
10. Update workpad, commit text, and PR-facing text so they describe the final post-deslop state rather than the earlier draft state.

## Stop rules

- Do not turn this into a refactor unrelated to the ticket.
- Do not churn stable code outside the changed area just to make it prettier.
- If a cleanup is subjective and not clearly better, leave it alone.
- Do not blindly apply every finding from every pass.
