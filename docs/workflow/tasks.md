# Task Workflow (GitHub Issues)

Use this reference to choose, prepare, work, and close tasks. Tasks live in GitHub Issues on this repository; `gh` is the primary interface. Issue identity, labels, Projects v2 fields (Priority, Effort, Status), and close state are owned by GitHub. This reference focuses on the decisions and order of work that GitHub cannot determine.

For the first task in a session, inspect the specific issue with `gh issue view <number>`. Reread this document when you need to refresh the workflow.

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

GitHub issue search does not index Projects v2 Status fields, so query the project with `gh project item-list` rather than `gh issue list --search 'status:...'`.

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

5. Before opening the pull request, run the repository's routed verification commands. Record material results in an issue comment and update the acceptance notes so the record explains how the outcome was verified. Opening the pull request is the contributor's assertion that the implementation is ready to merge; review may still require changes.

6. Before opening the pull request, assess documentation impact. Documentation is an ordinary project deliverable, so follow the project's own documentation guidance when durable user or contributor knowledge may have changed. Record the documents checked and updated, or the reason no update was needed, in the issue. Do not require documentation changes for every issue.

7. If the issue has an ExecPlan, complete its repository records before opening the pull request: update the ExecPlan Outcomes & Retrospective and move it to the completed plan bucket with `git mv`. If the issue body links to the plan, update that link after the pull request merges so the completed path is present on `master`.

8. Open the pull request only after steps 5-7 are complete. Reference the issue with `Closes #<number>` in the pull request body. The issue stays open during review and closes automatically when the pull request reaches `master`; after merge, add the delivered/verified summary and reconcile the issue's completed-plan link if needed.

For an issue without an ExecPlan, skip step 7. The inspection, start, implementation, verification, documentation assessment, and handoff steps remain the same.

### Issue lifecycle: definition of done

This section is the single source of truth for the issue lifecycle. [AGENTS.md](../../AGENTS.md), [CONTRIBUTING.md](../../CONTRIBUTING.md), and [docs/workflow/exec-plans.md](exec-plans.md) reference it instead of restating the rule. The lifecycle has two checkpoints and one post-merge record update:

- **Pull request opened (ready-to-merge checkpoint)** --- all implementation work, routed verification, acceptance notes, documentation assessment, and ExecPlan repository records are complete. The issue stays open during review. Include `Closes #<number>` in the pull request body so the issue is linked for automatic closure.
- **Pull request merged to the default branch (`master`)** --- GitHub closes the linked issue automatically. This is the definition of done for the issue because the implementation is now in `master`.
- **After merge** --- add the delivered/verified summary to the closed issue, update its link to the completed ExecPlan path if that link was not updated earlier, and set the issue's Projects v2 Status field to `Done`. A normal merged pull request does not need a separate `gh issue close` command. The `.github/workflows/move-closed-issues-to-done.yml` workflow sets Status to `Done` automatically for issues closed by a merged pull request; the agent-side step is the backstop for merges the workflow cannot see.

A pushed branch without an open pull request is not a lifecycle checkpoint. If a pull request is closed without merging, leave the issue open and either continue the work in a new pull request or close it separately as not planned.

## Change Or Close An Issue

Use `gh` commands for lifecycle and queue metadata:

```bash
gh issue edit <number> --add-label <label>       # retriage / accept
gh issue edit <number> --body-file <path>        # rewrite body (deps, scope)
gh issue comment <number> --body <text>          # progress, acceptance notes, or post-merge summary
gh issue close <number>                          # fallback only after a merged PR lacked a closing keyword
gh issue close <number> --reason "not planned"   # cancel (with a comment explaining why)
gh issue reopen <number>                         # reopen
```

Set the Projects v2 Status field to reflect queue state: Backlog (untriaged), Ready (accepted, workable), In Progress (claimed), Blocked (waiting on a dependency or decision), Done (merged to the default branch and closed).

Cancellation requires a reason: close with `--reason "not planned"` and add a comment recording why.

## Parents And Sub-Issues

Parent issues (trackers) use GitHub sub-issues. A parent whose sub-issues are all closed is complete; close the parent itself instead of starting implementation work on it. Do not invent a separate "tracking" status.
