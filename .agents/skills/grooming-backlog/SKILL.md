---
name: grooming-backlog
description: Groom Open, Pending, and Blocked ahm tasks so decisions, dependencies, and acceptance criteria are clear enough for work.
---

# Grooming Backlog

Use this skill for a grooming pass over Cake's local ahm backlog. [Task workflow](../../../docs/workflow/tasks.md) owns task lifecycle and storage; GitHub Issues remains intake.

## Goals

- Every Open task has a clear next action or is triaged into Pending or Blocked.
- Every Pending task has the decisions and acceptance scope needed for implementation.
- Every Blocked task records its reason and what would unblock it.
- Front-matter dependencies and ExecPlan references are accurate.
- Task bodies are self-contained; resolved questions are recorded in the body.
- Generated indexes remain CLI-owned views, not files to edit.

## When To Groom

Groom before a work cycle, after implementation changes the queue, when Open or Blocked tasks have gone stale, or when an agent reports insufficient scope.

## Workflow

### 1. Inspect The Queue

```bash
ahm prime
ahm task list --status Open,Pending,Blocked,Tracking
ahm task ready
ahm task blocked
ahm task labels
```

Use `.github/labels.yml` for Cake's allowed labels; `task labels` lists observed labels. Read each task with `ahm task show <id>`. Include Pending tasks with unmet dependencies: they appear in the blocked view even though their recorded status is Pending.

### 2. Audit Each Task

Check priority P0--P4, effort XS--XL, one type label and at least one area label from Cake's vocabulary. Add risk labels only where they affect routing. Inspect dependency ids with `ahm task dep tree <id>`; dependencies represent blockers, not merely related work. Confirm that L/XL tasks have a current ExecPlan before implementation.

Resolve stale references to files, commands and settled decisions. Record established choices and their evidence, and flag alternatives that still need a product or architecture decision. Check that acceptance criteria are concrete and useful, without TODO questions; actual completion evidence belongs in Acceptance Notes when work is done. Respect the distinction between Open scope needing triage, an explicit Blocked decision, and Pending work waiting on an incomplete dependency.

Check trackers and their children together. A Tracking parent is a planning record; work its children, and report a ready tracker for completion when all children are completed or cancelled. Inspect dependent tasks when a prerequisite was cancelled: cancellation alone does not mean its required work was delivered.

### 3. Apply Authorized Corrections

An explicit grooming request authorizes mechanical corrections to documented invariants, including stale metadata, invalid paths, and dependency references. It does not authorize inventing unresolved product or architecture decisions, creating additional tasks, or cancelling tasks without explicit approval. For an audit/report request, do not mutate records.

Use the CLI:

```bash
ahm task edit <id> --priority P2 --effort S
ahm task edit <id> --add-label area:tools --remove-label area:cli
ahm task edit <id> --section "Decision" --body-file <path>
ahm task dep add <id> <dependency-id>
ahm task dep remove <id> <dependency-id>
ahm task accept <id>
ahm task block <id> --reason <text>
ahm task unblock <id>
```

Use `task edit` for body sections and supported metadata, preserving CLI-owned Comments and Cancellation Reason. Do not replace the record's status by editing front matter. For an already Blocked task whose reason needs correction, `task block` is a no-op; unblock and block it again with the established reason, preserving its dependencies. Propose cancellation of obsolete work and wait for explicit approval before `ahm task cancel <id> --reason <text>`.

### 4. Record Unresolved Decisions

For a decision that prevents implementation, record the question in the task and use `ahm task block <id> --reason <text>`, with `--ref <url>` for an external reference when applicable. Keep unfinished scoping work Open with a next action. Do not leave an unresolved implementation decision in the ready queue.

### 5. Verify

For every task in the requested queues, report its outcome: Pending, Blocked, Open with a next action, or Tracking with its child state. List anything skipped, inaccessible or unresolved and why. Continue until the requested queues are exhausted or the remaining items are explicitly listed.

```bash
ahm task ready
ahm task blocked
ahm status
```

Normal CLI mutations regenerate indexes. Body-only edits need no regeneration; run `ahm index` after unavoidable manual metadata/linkage changes. Distinguish the migration's accepted missing-blocked-reason warnings from new errors or unexplained warnings.
