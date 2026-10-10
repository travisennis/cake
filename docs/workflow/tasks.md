# Task Workflow

Use this reference to choose, prepare, work, and close tasks. `ahm task ...` owns task identity, front matter, storage, lifecycle transitions, and index regeneration. This project-owned document describes the decisions and order of work that the CLI cannot determine. GitHub Issues is intake; the working backlog lives in ahm.

## Choose And Inspect Work

Start a managed-work session with `ahm prime`. If the user names a task id or title, inspect that task even if another task is higher in the queue:

```bash
ahm task show <id>
```

For the next task, run `ahm task next`. Inspect the broader queue with:

```bash
ahm task ready
ahm task blocked
ahm task list --status Open
ahm task ready --label area:tools
```

When choosing from the queue:

1. Work lower priority numbers first: `P0`, then `P1` through `P4`.
2. Start only `Pending` tasks whose dependencies are complete. `Open` tasks need triage; `Blocked` tasks need their blocker resolved. Resume an `In Progress` task only when the user asks.
3. Inspect dependencies before starting. A Pending task with incomplete dependencies appears in `ahm task blocked`, not `ahm task ready`.
4. Treat `Tracking` parents as planning records and work their children in the stated order. A tracker enters the ready queue when all its children are completed or cancelled and its own dependencies are complete; complete the tracker rather than starting implementation work on it.
5. Use `--label` filters for an area or risk category. `.github/labels.yml` is Cake's label vocabulary; `ahm task labels` lists labels currently in use.

Before editing, read the full task and inspect the relevant repository state. If the task is vague, stale, or conflicts with the implementation, record the discovery or obtain the missing product decision before proceeding.

## Create And Triage Tasks

Create tasks through the CLI with explicit labels from `.github/labels.yml`; ahm's default labels are not Cake's vocabulary:

```bash
ahm task create "Short imperative title" --labels "type:task,area:tools" --body-file <path>
```

The command allocates the id, writes front matter, places the task in the active bucket, and regenerates indexes. Use `--description` for a concise summary or `--body-file` for a detailed record. Include the problem, why it matters, relevant files and behavior, implementation direction, and concrete acceptance criteria with expected verification.

New tasks default to `Open`. Accept fully scoped work with `ahm task accept <id>` to move it to `Pending`, or create it with `--status Pending`. Leave unresolved scope, product choices, evidence, or planning in Open; record a known blocker with `ahm task block <id> --reason <text>`.

Priority runs from `P0` to `P4`; effort runs from `XS` to `XL`. Every task has one `type:*` and at least one `area:*` label; add `risk:*` labels when they affect routing or verification. `L` and `XL` work requires an ExecPlan before implementation. Record real blocking dependencies with `--depends-on <ids>` at creation or `ahm task dep add <id> <dependency-id>`, not a prose dependency list. Create children with `--parent <id>`; they receive lettered ids. A parent planning record uses `Tracking` status.

## Work A Task

Follow this procedure for each implementation task:

1. Run `ahm task show <id>`. Confirm that the task matches the repository, has clear scope and acceptance, and has no incomplete dependencies.
2. Run `ahm task start <id>`. If the user explicitly resumes an In Progress task, continue it without restarting the lifecycle.
3. Settle required decision and planning work before implementation. Use [Research workflow](research.md) for durable evidence, [ADR guidance](../adr/README.md) for architectural decisions, and [ExecPlan workflow](exec-plans.md) for L/XL, cross-cutting, or substantially uncertain work. Keep brief task-local observations in the task; leave it Open while material uncertainty prevents clear scope.
4. Create the branch before the first repository edit with `just branch <type>/<slug>`, or `just worktree <type>/<slug>` beside another agent. Implement only the task's scope, preserve unrelated work, and commit freely on the feature branch.
5. Run the routed checks in [CONTRIBUTING.md](../../CONTRIBUTING.md) and preflight the change. Record material results and replace placeholder or unchecked Acceptance Notes with the actual outcome and verification.
6. Assess documentation impact. Record the documents checked and updated, or explain why no update is needed. Do not require documentation changes for every task.
7. If an ExecPlan applies, complete its Outcomes & Retrospective and move it to `docs/exec-plans/completed/` when the whole plan is complete. Update the task's plan reference for the completed path.
8. Run `ahm task complete <id>` before the handoff commit that finalizes the task. Work-in-progress commits are expected; completion is required before the final implementing commit, not every intermediate commit. Follow the lifecycle below for integration and handoff.

## Task Lifecycle: Definition Of Done

Task completion and repository integration are separate facts. `ahm task complete <id>` records completed and verified work on the feature branch; integration into `master` makes delivery true. The acceptance notes, documentation assessment, and applicable ExecPlan archival are finished before that final implementing commit. Read-only reports need no task unless persistence was requested.

When a pull request is the handoff, open it after those records and routed checks are complete. Reference the ahm task id in its body and use `just pr task=<id>` to record the URL on the task. A task in the home store has no repository-relative link, and GitHub closing keywords do not complete it. Do not merge, enable auto-merge, close the PR, or delete the remote branch without explicit user approval. If the implementing branch or pull request is abandoned, use `ahm task reopen <id>` and return the task to Open for triage or accept it back into Pending as appropriate.

After authorized integration, record the delivered commit or PR and final verification with `ahm task comment <id> <text>`. Task records and their completion state remain local; they are not synchronized by GitHub or CI. The current branch protections still require pull requests for remote integration; this lifecycle does not change those protections.

## Change Or Close A Task

Use commands for metadata and lifecycle whenever one exists:

```bash
ahm task edit <id> --priority P1 --effort S
ahm task edit <id> --section "Acceptance Notes" --body-file <path>
ahm task dep add <id> <dependency-id>
ahm task dep remove <id> <dependency-id>
ahm task block <id> --reason <text>
ahm task unblock <id>
ahm task comment <id> <text>
ahm task complete <id>
ahm task cancel <id> --reason <text>
ahm task reopen <id>
```

`task edit` refuses status and dependency changes; use the lifecycle and dependency commands. Comments and Cancellation Reason are owned by their commands, so body edits must preserve them. Direct body edits are a fallback for working context; avoid manual front-matter or bucket changes.

Cake sets `strict_acceptance: true` in `.ahm/config.json`. Completion refuses missing Acceptance Notes, TODO placeholders, or unchecked acceptance items unless explicitly overridden with `--force`. Replace them with the actual outcome and checks rather than bypassing the gate. Some migrated tasks have no acceptance section and need one added before completion. Task completion moves the record, updates eligible dependents, and regenerates indexes. Cancellation requires a reason; it records that reason and moves the record to the cancelled bucket.

## Storage And Manual Fallback

Task source records live outside git in `~/.ahm/projects/cake-d8f8727b/tasks/`, with `active/`, `completed/`, and `cancelled/` buckets. Run `ahm store path` for the resolved paths. The key derives from the origin remote, so local clones and worktrees share one backlog. Another machine needs its own initialized store and records; a changed remote can resolve to another store. See [ADR 041](../adr/041-ahm-task-records-in-the-user-level-home-store.md) for the decision, approved sandbox grant, source pin, and backup/restore procedure.

The repository commits `.ahm/config.json` and `.ahm/.gitignore`, not task records. A fresh checkout without its home store exits 1 with `store_dir_unreadable` until `ahm init` creates the directories and indexes. Initialization creates an empty local backlog; it does not download tasks. Back up `~/.ahm` and preserve the migration import JSON and issue-number list outside git.

`tasks/index.md` and the bucket indexes are generated views; never edit them directly. Normal task mutations regenerate them. Run `ahm index` after unavoidable manual metadata, location, or linkage changes; body-only edits do not need it. `docs/adr/index.md` is also generated and gitignored. Reference a task by id and `ahm task show <id>`, not a relative link to its home-store file.

If ahm is unavailable, inspect the source records and generated indexes as a fallback. Preserve ids, filenames and matching buckets, and regenerate indexes once the CLI is available. Imported Blocked tasks may produce the accepted `task_blocked_missing_reason` warnings; status and doctor must still exit 0 without errors, and those warnings are recorded against the migration baseline.

## GitHub Intake

GitHub Issues remains available for new reports and scheduled CI failures. It is not the working queue. Converting an accepted report is an explicit operator-owned action:

```bash
gh issue view <number>
ahm task create "Accepted report title" --external-ref <issue-url> --labels "type:bug,area:tools" --body-file <path>
```

Preserve the report's context and acceptance scope in the task. The operator comments the new task id and `ahm task show <id>` command on the issue, then closes it with an explanation that work continues in ahm. Do not automatically synchronize GitHub state with the local store. Scheduled checks continue creating their rolling intake issue because CI cannot mutate this machine's backlog; an accepted fix becomes an ahm task through the same triage step.
