---
name: grooming-backlog
description: Go through Backlog, Ready, and Blocked issues in the GitHub backlog and make sure all decisions are made, dependencies are correct, and no open questions remain. The goal is to get issues into a ready state for work.
---

# Grooming Backlog

Use this skill when the backlog needs a grooming pass. Grooming moves issues from ambiguous, underspecified, or stale states into a ready state where an agent can pick them up and work them without needing additional decisions or clarification.

## Goals

- Every untriaged (Backlog) issue is either Ready (ready to work), Blocked (blocker documented), or has a clear next action.
- Every Ready issue has all product and design decisions recorded.
- Every Blocked issue documents what it is blocked on and what would unblock it.
- Dependencies (the `## Depends on` body section) and ExecPlan links are accurate and complete.
- Issue bodies are self-contained --- no open questions in the body or comments.
- No generated indexes to maintain; the board and the issue list are the index.

## When to groom

- Before a sprint or work cycle planning.
- When the backlog has accumulated stale Backlog or Blocked issues.
- After a block of implementation work (to rebalance the queue).
- Any time an agent reports an issue is too vague to work.

## Workflow

### 1. Inspect the queue

```bash
# Quick ready queue — Cake Backlog project (project 1)
gh project item-list 1 --owner travisennis --query 'status:Ready' --limit 200

# Active issues not ready
gh project item-list 1 --owner travisennis --query 'status:Blocked' --limit 200
gh project item-list 1 --owner travisennis --query 'status:Backlog' --limit 200

# Full open list with labels
gh issue list --state open

# Label vocabulary
cat .github/labels.yml
```

GitHub issue search does not index Projects v2 Status fields, so query the project with `gh project item-list` rather than `gh issue list --search 'status:...'`.

### 2. For each open issue, audit

**Field invariants (Projects v2 on the Cake Backlog project):**

- `Status` is one of: `Backlog`, `Ready`, `In Progress`, `Blocked`.
- `Priority` is set and uses the project's priority scale (`P0`-`P4`).
- `Effort` is set and uses the project's effort scale (`XS`-`XL`).
- Labels include at least one `type:*` and one `area:*` label, from the vocabulary in `.github/labels.yml`.
- `## Depends on` references real issue numbers. An issue only depends on another if the dependency is genuinely blocking --- not just "related to."
- Issues with `Effort` `L` or `XL` link an ExecPlan (in `docs/exec-plans/active/`) or document that no plan is needed.

**Decision completeness:**

- If the issue presents alternatives (e.g., "use X or Y"), record which alternative was chosen and why. If none is chosen yet, set Status to `Blocked` and document what decision is needed.
- If the issue references external inputs (issues, design docs, conversations) that have since been resolved, capture the resolution in the issue body.
- If the issue is `L` or `XL` without an ExecPlan, flag it.

**Body quality:**

- Acceptance criteria should not contain `TODO` placeholders or unchecked items that should have been decided before work begins.
- Relevant files, modules, and commands listed in the issue are still valid (paths exist, modules still in use).
- The issue body does not contain "ask the user" or "decide later" phrasing without a corresponding `Blocked` status and blocker note.

**Dependency graph:**

- For each `## Depends on` entry, check that the dependency exists and is not itself Blocked or Backlog. If a dependency is blocked, the depending issue should also be `Blocked` with a note referencing the dependency.
- For each issue that other issues depend on, check that its status reflects that it is a dependency (e.g., if #102 and #103 depend on #101, #101 should not be closed without unblocking or updating #102 and #103).

### 3. Fix what you can directly

- Set the Projects v2 `Status` field to move issues between `Backlog`, `Ready`, and `Blocked` (`gh project item-edit` or the web UI).
- Update priority, effort, and labels with `gh issue edit`.
- Edit the `## Depends on` section in the issue body with `gh issue edit`.
- Record decisions in the issue body (add a `## Decision` section when recording a resolved choice).
- Remove stale `TODO` placeholders from acceptance criteria when the question has been answered.
- If an issue is superseded, obsolete, or no longer relevant, close it with `gh issue close <n> --reason "not planned"` plus a comment explaining why.

### 4. Flag what needs human input

When an issue needs a product, design, or architecture decision that an agent cannot make alone:

1. Set the Projects v2 `Status` field to `Blocked`.

2. Add or update the blocker note at the top of the issue body:
   ```
   ## Blocker
   Awaiting decision on [describe what]. See [reference].
   ```

3. Do not leave the issue `Ready` with undocumented open questions.

### 5. Verify

No index regeneration is needed. Confirm the board reflects the changes:

```bash
gh project item-list 1 --owner travisennis --query 'status:Blocked' --limit 200
gh project item-list 1 --owner travisennis --query 'status:Ready' --limit 200
```
