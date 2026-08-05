# Task Workflow (GitHub Issues)

Use this reference to choose, prepare, work, and close tasks. Tasks live in GitHub Issues on this repository; `gh` is the primary interface. Issue identity, labels, Projects v2 fields (Priority, Effort, Status), and close state are owned by GitHub. This reference focuses on the decisions and order of work that GitHub cannot determine.

For the first task in a session, run the session briefing (`just brief`), then inspect the specific issue with `gh issue view <number>`. Reread this document when you need to refresh the workflow.

## Choose And Inspect Work

If the user names an issue number or title, use that issue even if another issue is higher in the queue:

```bash
gh issue view <number>
```

If the user asks for the next task, list the ready queue. Use the Projects v2 Status field (Backlog, Ready, In Progress, Blocked) to segment the board, and the `type:`, `area:`, and `risk:` labels to filter by category:

```bash
# Ready queue (triaged, workable) — Cake Backlog project (project 1)
gh project item-list 1 --owner travisennis --query 'status:Ready' --limit 200

# Blocked issues
gh project item-list 1 --owner travisennis --query 'status:Blocked' --limit 200

# Issues in a particular area
gh issue list --state open --label 'area:clients'
```

`just brief` prints the same status summary plus active ExecPlans and recent research. GitHub issue search does not index Projects v2 Status fields, so query the project with `gh project item-list` rather than `gh issue list --search 'status:...'`.

The committed vocabulary in `.github/labels.yml` is the single source of truth for labels; verify it with `just labels-check-file`.

When choosing from the queue:

1. Work lower priority numbers first: `P0`, then `P1` through `P4`.
2. Start only `Ready` issues. `Backlog` issues need triage, and `Blocked` issues are not directly workable. Resume an `In Progress` issue only when the user asks.
3. Check dependencies before starting. Work an incomplete dependency first or explain why the requested issue is blocked. Dependencies are recorded in a `## Depends on` body section as `#<number>` links.
4. Treat parent issues as planning records and work their sub-issues in the stated order. Parents use GitHub sub-issues; do not invent a separate tracker status.
5. Use label filters when the user asks for work in a particular area or risk category.

Before editing, read the full issue and inspect the relevant repository state. If the issue is vague, stale, or conflicts with the current implementation, record the discovery or ask for the missing product decision before proceeding.

## Create And Triage Issues

Create issues through the CLI:

```bash
gh issue create --title "Short imperative title" --label "type:...,area:..." --body-file <path>
```

The body should give a future worker enough context to understand:

- the problem and why it matters;
- the relevant files, commands, modules, or observed behavior;
- useful implementation direction without unnecessarily prescribing the fix;
- concrete acceptance criteria and expected verification.

Use the `## Depends on` body section to record issue dependencies as `#<number>` links. Keep the Projects v2 fields current: Priority runs from `P0` for urgent blockers to `P4` for deferred work; Effort `XS` and `S` are localized, `M` is moderate, and `L` or `XL` requires an ExecPlan before implementation. Status starts at Backlog (untriaged) or Ready when the issue is fully scoped.

Every issue should have stable `type:*` and `area:*` labels. Add `risk:*` labels only when they affect routing or verification.

## Work An Issue

Follow this procedure for every implementation issue, whether or not it has an ExecPlan:

1. Run `gh issue view <number>`. Confirm that the issue still matches the repository, is ready to work, and has no incomplete dependencies. During this inspection, consult `docs/workflow/research.md` if resolving material uncertainty requires evidence that should survive the session or inform multiple artifacts. Keep brief issue-local code reading in the issue rather than creating research note churn. Leave the issue `Backlog` while material research questions prevent clear scope or acceptance.

2. Set the Status field to `In Progress`. If the user explicitly asks to resume an existing `In Progress` issue, continue it without restarting the lifecycle.

3. Before implementation, route any required decision or planning work:
   - Consult `docs/workflow/research.md` when factual or technical uncertainty requires durable evidence that may feed an ADR, issue, or ExecPlan. Research is evidence, not a decision or implementation contract.
   - Consult `docs/adr/README.md` when the issue introduces or changes a durable architectural decision, including persisted state, configuration, security boundaries, migrations, breaking behavior, or major dependencies.
   - Consult `docs/workflow/exec-plans.md` for `L` and `XL` issues, and for smaller work that is cross-cutting or substantially uncertain. Create or update the ExecPlan and link it from the issue before changing code.

   When all three apply, work in conceptual order: research evidence, architectural decision, then execution planning. Do not require research records or documentation changes for every issue.

4. Implement only the issue's problem and acceptance scope. Preserve unrelated worktree changes, and do not commit unless the user explicitly asks.

5. Run the repository's routed verification commands. Record material results in an issue comment and update the acceptance notes so the record explains how the outcome was verified.

6. Before pushing the change, assess documentation impact. Documentation is an ordinary project deliverable, so follow the project's own documentation guidance when durable user or contributor knowledge may have changed. Record the documents checked and updated, or the reason no update was needed, in the issue. Do not require documentation changes for every issue.

7. If the issue has an ExecPlan, complete its records when the change reaches `master` (step 8): update the ExecPlan Outcomes & Retrospective, move it to the completed plan bucket with `git mv`, update the issue's link to the completed path, and record the outcome.

8. Close the issue when the change reaches `master`. Pushing the branch or opening the pull request completes the acceptance record but leaves the issue open; see the lifecycle below.

For an issue without an ExecPlan, skip step 7. The inspection, start, implementation, verification, documentation assessment, and handoff steps remain the same.

### Issue lifecycle: definition of done

This section is the single source of truth for the issue lifecycle. [AGENTS.md](../../AGENTS.md), [CONTRIBUTING.md](../../CONTRIBUTING.md), and [docs/workflow/exec-plans.md](exec-plans.md) reference it instead of restating the rule. The lifecycle has two checkpoints:

- **Change pushed (branch push / pull request opened)** --- the work record completes and the issue stays open. At this checkpoint the verification results are recorded in an issue comment and the acceptance notes are marked complete (steps 5-6). This is the definition of done for a change entering a PR: implementation complete, routed checks run and recorded, acceptance notes complete, documentation impact assessed, PR open for review.
- **Change merged to `master`** --- the issue closes. Close with a summary of what was delivered and how it was verified; for ExecPlan-driven issues, complete the ExecPlan records first (step 7). This is the definition of done for the issue: the PR is merged and the issue record says delivered and verified.

A change can be done in a PR while its issue remains open; the issue closes only when the change is in `master`.

## Change Or Close An Issue

Use `gh` commands for lifecycle and queue metadata:

```bash
gh issue edit <number> --add-label <label>       # retriage / accept
gh issue edit <number> --body-file <path>        # rewrite body (deps, scope)
gh issue comment <number> --body <text>          # progress / acceptance notes
gh issue close <number>                          # complete -- only once the change is in master
gh issue close <number> --reason "not planned"   # cancel (with a comment explaining why)
gh issue reopen <number>                         # reopen
```

Set the Projects v2 Status field to reflect queue state: Backlog (untriaged), Ready (accepted, workable), In Progress (claimed), Blocked (waiting on a dependency or decision).

Cancellation requires a reason: close with `--reason "not planned"` and add a comment recording why.

## Parents And Sub-Issues

Parent issues (trackers) use GitHub sub-issues. A parent whose sub-issues are all closed is complete; close the parent itself instead of starting implementation work on it. Do not invent a separate "tracking" status.
