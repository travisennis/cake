# Migrate task tracking from GitHub Issues to ahm

This ExecPlan is a living document, maintained per `docs/workflow/exec-plans.md`. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be kept current as work proceeds.

## Purpose / Big Picture

Cake's working backlog moves from GitHub Issues into `ahm` task records stored in the user-level store under `~/.ahm`, and every document, skill, and script that treats GitHub Issues as the task authority is rewritten. GitHub Issues stays open as an intake channel for third-party reports; `ahm` becomes the working queue. After this change a contributor or agent runs `ahm prime`, `ahm task ready`, and `ahm task show <id>` instead of `gh issue view` and `gh project item-list`, so a session can be briefed, pick up work, and complete it without touching GitHub.

How to see it working: from a checkout of `travisennis/cake` on a machine where `ahm init` has run, `ahm status` exits 0, `ahm task ready` lists the migrated backlog, and searching the workflow surface (`AGENTS.md`, `CONTRIBUTING.md`, `docs/workflow/`, `docs/runbooks/`, `docs/automations/`, `.agents/`, `scripts/`, `.github/pull_request_template.md`) for `gh issue` or `gh project` finds only intake instructions and history. The migrated GitHub issues are closed, each with a comment linking its ahm task.

Definitions used below. `ahm` is the local task and ADR record manager (a source build of `/Users/travisennis/Projects/ahm`, installed at `~/go/bin/ahm`). The *store* is the machine-local directory `~/.ahm/projects/cake-d8f8727b/tasks/` where ahm keeps this repository's records; the key `cake-d8f8727b` derives from the `origin` remote `github.com/travisennis/cake`, so every clone and every worktree of this repository on one machine shares one backlog. A *tracker* is a parent task whose children carry lettered ids (`137a`, `137b`, ...). *Panache* is the Markdown formatter and linter run by `just docs-check` and `just docs-fmt`. *ExecPlan* is this document.

The work is delivered as two pull requests: infrastructure (Milestone 1) and content (Milestones 2--4). The prerequisite landed 2026-10-08: issue #93 closed via PR #714 (`3b64459`, ADR 040), so an ordinary settings `[sandbox]` grant for `~/.ahm` survives startup and can bootstrap the store.

## Progress

- [x] (2026-10-09) Owner approved the plan review's recommendations: dangling dependencies, native blocked-by links, strict acceptance, blocked-reason warnings, ahm pin, `just pr task=`, snapshot handling, contributor sandbox grant.
- [x] (2026-10-09) Created `chore/ahm-task-records` from an up-to-date `master`.
- [x] (2026-10-09) Wrote this ExecPlan; formatted with panache; committed.
- [x] (2026-10-09) M1 infrastructure implemented: initialized the home store, committed configuration prepared with strict acceptance, generated index ignored, classifier fixtures added, ADR 041 written, rubric and sandbox grant added, task 001 accepted and started. `ahm status`, classifier fixtures, `just docs-check`, and `just check-scripts` pass.
- [x] (2026-10-09) M1 routed gate and three-pass preflight complete. `just check` passed outside the outer sandbox; the focused macOS writable-grant allow/deny test passed.
- [x] (2026-10-09) M1 handoff: PR #715 merged as `3d2deb4`; all CI checks passed. Continued on the owner-requested current branch `chore/ahm-task-records-content`.
- [x] (2026-10-09) M2: wrote `scripts/import-gh-issues-to-ahm.py`, offline fixtures and `just ahm-import-check`; live fetch and ahm dry run accepted all 141 records without refusals.
- [ ] M3: freeze the baseline, run the import, verify, close the migrated issues, preserve the snapshot.
- [ ] M4: rewrite the workflow surface; delete the GitHub-issue machinery; add `just pr task=`.
- [ ] M5: run the routed gates and preflight; open the content pull request; move this plan to `docs/exec-plans/completed/`.

## Surprises & Discoveries

- The installed ahm binary was built from `36050bfde66a053e99dbbe8dd672f80ced0ca0ca`, behind source HEAD `020071747be4cda34e1783a30e97b49b7d9079a1`; rebuilt and installed HEAD for M1. Evidence: `go version -m ~/go/bin/ahm` and `git rev-parse HEAD` in the ahm checkout.
- Task creation takes a positional title, not `--title`, and defaults to `Open`; `task start` only accepts `Pending`. M1 used `task accept 001` before `task start 001`; both transitions succeeded. The concrete steps now reflect the actual CLI.
- The backlog moves continuously. On 2026-10-05 the repository had 155 open issues with board counts 70 Ready / 53 Blocked / 32 Backlog; on 2026-10-09 it had 141 open issues with 64 / 45 / 32. All counts in this plan are illustrative; the move-time baseline is the verification target. Evidence: `gh issue list --state open` and `gh project item-list 1 --owner travisennis --format json`.
- `ahm task import` does not set a parent's status. Neither import nor `task create --parent` moves a parent to `Tracking`, so the import document must carry `status: "Tracking"` for the seven parent trackers. Evidence: `internal/ahm/task_import.go` and `internal/ahm/task_create.go` in the ahm checkout.
- Imported `Blocked` tasks without a `blocked_reason` produce warning-tier `task_blocked_missing_reason` findings from `ahm status` and `ahm doctor`; warnings never change exit codes, and the import format has no field for the reason. Evidence: `internal/ahm/validation_deps.go`.
- `strict_acceptance: true` refuses `ahm task complete` when acceptance notes are missing, contain a `TODO` placeholder, or contain unchecked `- [ ]` items, unless `--force` is passed. Of 141 open issues measured 2026-10-09: 62 have unchecked acceptance items (fixed by import-time normalization), 39 have no acceptance section at all (not fixed by normalization), and 40 are clean. Evidence: `internal/ahm/task_acceptance.go`, `internal/ahm/task_status.go`.
- Dependency data is messier than the issue sections suggest. Of 47 bullet-form `## Depends on` refs measured 2026-10-09, 29 point at open issues and 18 at closed ones (one target closed as not planned, `#46 → #60`); three issues also carry native GitHub blocked-by links, all to closed targets. Evidence: `gh api repos/travisennis/cake/issues/<n>/dependencies/blocked_by` over the open set.
- `ahm init` is a reconciling command. Run over the pre-existing partial store (`~/.ahm/projects/cake-d8f8727b/` holds only `project.json`), `ahm --dry-run init` reports it would create `.ahm/config.json` (with `strict_acceptance: false`), `.ahm/.gitignore`, the store bucket directories and indexes, and `docs/adr/index.md`, with no error. Evidence: `ahm --dry-run init` in `cake/`, 2026-10-09.
- `gh api repos/travisennis/cake/issues/<n>/parent` returns the sub-issue parent (verified against issue 634 → 322), so the import script can read parentage per issue rather than hard-coding the tracker list.

## Decision Log

- M2 exposes only fetch/replay and always invokes the importer with `--dry-run`; real import and issue closure are deferred to M3. `--save-snapshot` preserves source data for offline replay. Historical leading `../` link prefixes are resolved from the repository root, retaining the five known docs links; unresolved targets become plain labels with baseline findings. (2026-10-09)

These decisions were made by Travis Ennis across 2026-10-05 and 2026-10-09 and are not to be revisited without asking.

- Import open issues only; the already-closed issues stay on GitHub as history. Each migrated issue is closed after import with a comment linking its ahm task, so `ahm task ready` is the only live queue. (2026-10-05)
- Drop the old `## Migrated from ahm` sections during import; new records start fresh at `001`, the old id numbering is not preserved, and the collision with old issue numbers is accepted. (2026-10-05)
- Let `ahm` generate `docs/adr/index.md` and gitignore it; do not commit it. (2026-10-05)
- GitHub Issues remain open for new reports, including third-party ones; triage is an explicit operator-owned step that converts an accepted report into an ahm task with `external_ref` set to the issue URL, then closes the issue. (2026-10-05)
- Set `"strict_acceptance": true` in `.ahm/config.json`; normalize `- [ ]` / `- [x]` acceptance task-list boxes to plain `-` bullets during import, because a GitHub checkbox is not an ahm acceptance signal. The 39 issues without an acceptance section are completed later with notes added or `--force`; ADR 041 records this consequence. (2026-10-05, extended 2026-10-09)
- Split the work into two pull requests: infrastructure (`.ahm/`, classifier, ADR 041, judge rubric, repository sandbox grant), then content (import script, rewrites, deletions, the move). (2026-10-05)
- Use the `docs:` commit type across this work; no `cog.toml` change. (2026-10-05)
- Definition of done: run `ahm task complete <id>` on the feature branch before the implementing commit; merge to `master` is what makes it true. An abandoned pull request reopens the task or returns it to `Pending`. The pull request body references the task; nothing auto-closes. (2026-10-05)
- Scheduled-check failures (`report-failure` in `.github/workflows/scheduled.yml`) keep opening their rolling GitHub issue, because CI cannot write a local store; `.github/workflows/label-governance.yml` keeps its `issues: labeled` trigger for intake. (2026-10-05)
- Add a "Local task-store mutations" section to `.cake/judge-rubric.md` naming ahm mutations as routine and reversible; add no classifier corpus cases. (2026-10-05)
- Only bullet refs (`- #NNN`) inside `## Depends on` are dependencies; prose mentions such as "None --- #50 is closed" are not. Refs to imported records become `@ref` batch references; refs to closed targets are dropped and reported; the one not-planned target is flagged for manual review. Native blocked-by links are merged into `depends_on`. (2026-10-09)
- Accept the expected `task_blocked_missing_reason` warnings from imported `Blocked` tasks; the verification wording is "exit 0 with the expected blocked-reason warnings", not "doctor clean". (2026-10-09)
- Move the ahm pin from `4be3e44` to the ahm HEAD (or a fresh tag cut from it) at move time, and record the sha in ADR 041 and the contributor setup, because the docs adapt ahm's current workflow, which describes the ADR 030 transition contract and the Tracking lifecycle fix. (2026-10-09)
- Add `task=N` to `just pr`: it runs `ahm task comment N` with the pull request URL, replacing the deleted `issue=` comment-back. (2026-10-09)
- Preserve the import JSON and the issue-number list outside git, with the `~/.ahm` backup; ADR 041 records the restore path (re-import the JSON, or re-fetch the imported issues by number --- closed issues remain readable). (2026-10-09)
- Add `~/.ahm` to `cake/.cake/settings.toml` `[sandbox].writable` so clones and worktrees work with no per-machine setup, and name the grant in the contributor setup next to the ahm source-build note. (2026-10-09)
- Keep the lifecycle section of `docs/workflow/tasks.md` from over-fitting to GitHub pull requests: the owner is considering merging local branches straight to `master` for first-party work. Do not change the branch ruleset or the pull-request tooling in this task. Keep historical references (for example in `docs/exec-plans/completed/` and the `#254` provenance paragraph in `docs/guardrails/agent-instructions.md`) as history. (2026-10-05)

## Context and Orientation

The repository is `/Users/travisennis/Projects/cake/cake`. Everything in this plan happens inside it except store writes (`~/.ahm`), the GitHub reads and writes performed with `gh`, and the import artifacts kept outside git.

The current workflow. `docs/workflow/tasks.md` is the single source of truth for the issue lifecycle; `AGENTS.md`, `CONTRIBUTING.md`, `docs/workflow/exec-plans.md`, and `docs/workflow/research.md` reference it instead of restating it. The backlog lives in GitHub Issues with Projects v2 fields (Status, Priority, Effort). `just ready-queue` prints the Ready queue, `scripts/claim-issue.sh` moves a board item between Ready and In Progress, `scripts/list-ready-issues.sh` lists the Ready queue, and `just pr issue=N` comments the pull request URL back on an issue. Four agent-facing files drive the GitHub surface: `.agents/skills/grooming-backlog/SKILL.md`, `.agents/skills/finding-improvements/SKILL.md`, `.agents/skills/preflight/SKILL.md`, and `.agents/subagents/review.md`.

The target workflow. ahm owns the working backlog in the home store, and GitHub Issues is intake. The ahm command surface used by this plan: `ahm init` (create or reconcile workflow state), `ahm status` and `ahm doctor` (health), `ahm prime` (session briefing), `ahm task create/start/complete/comment/list/ready/blocked/next`, `ahm task import --from-file <json>` (bulk create), `ahm adr create` (allocate an ADR), `ahm index` (regenerate indexes), and `ahm store path` (show the store). ahm makes no network requests by design; the GitHub half of the migration stays in a cake-side script.

The import contract (ahm ADR 025). `ahm task import --from-file` takes a JSON array of records with fields `ref`, `title`, `body`, `status`, `priority`, `effort`, `labels`, `created`, `parent`, `depends_on`, and `external_ref`. Optional `ref` keys identify records inside the file; `@ref` values refer to batch records and numeric ids to existing records. The importer prevalidates the whole batch (exit 2 on malformed JSON or a bad document shape; exit 1 with all errors and nothing written on semantic refusal), allocates top-level ids in document order, then children under resolved parents, then resolves dependencies; a valid batch is written under one record lock with one index regeneration and rolls back on an ordinary write failure. Preview with `ahm --dry-run task import --from-file <path>`. Valid statuses are `Open`, `Pending`, `In Progress`, `Blocked`, `Tracking`, `Completed`, and `Cancelled`; priorities run `P0`--`P4`; efforts run `XS`--`XL`. Children get lettered ids under a
parent, at most 26 per parent; a child's `parent` must name a top-level record.

The measured backlog shape (2026-10-09, illustrative; re-measure at move time): 141 open issues; board 64 Ready / 32 Backlog / 45 Blocked; seven parent trackers (#322, #343, #438, #439, #440, #507, #653) with 30 parented open issues; 37 `## Depends on` sections with 47 bullet refs; 11 `## Migrated from ahm` sections; 5 relative Markdown links in 2 bodies (`#343`, `#128`, all pointing at repository docs); 50 issues carry 58 comments; acceptance coverage 39 missing / 62 unchecked / 40 clean; no open issue carries assignees or milestones.

Key files. `scripts/classify-changes.sh` classifies a diff as docs / code / mixed / unknown / none and routes the pre-push gate; `.github/workflows/ci.yml` runs it and `scripts/test-classify-changes.sh`. `.cake/settings.toml` holds the repository sandbox settings (currently `~/.cake`, `~/.cache/cake`, `~/.local/share/cake`, `~/.config/cake` writable). `.cake/judge-rubric.md` holds supplemental command-judge allowances. `docs/adr/README.md` is the ADR template authority; the next ADR number is 041 (039 and 040 are taken). `.github/labels.yml` is the label authority; its header already documents the vocabulary as "the former ahm task labels."

## Plan of Work

Five milestones. M1 ships as the infrastructure pull request; M2--M4 execute on a content branch cut from the merged infrastructure master; M5 hands off. This file is committed with M1 and stays in `docs/exec-plans/active/` until the whole migration completes.

**M1 --- Bootstrap the ahm project (infrastructure).** Goal: the repository holds a committed ahm configuration, the gates understand `.ahm/`, the decision is recorded, and the judge and sandbox know about ahm. Work: from the repository root run `ahm init` (it reconciles the pre-existing partial store); edit `.ahm/config.json` to add `"strict_acceptance": true`; add `docs/adr/index.md` to the repository `.gitignore`; add `.ahm/*` to the code arm of `scripts/classify-changes.sh` and a matching fixture case in `scripts/test-classify-changes.sh`; write `docs/adr/041-ahm-task-records-in-the-user-level-home-store.md`; add the "Local task-store mutations" section to `.cake/judge-rubric.md`; add `~/.ahm` to the `[sandbox].writable` list in `.cake/settings.toml`; create the migration's own task with `ahm task create` (explicit `--labels`, because the default `area:unknown` is not in the vocabulary) and start it. Result: `ahm status` exits 0 in the checkout, `just test-classify-changes` passes
with the new fixture, and the ADR records the decision. Proof: the commands under Concrete Steps and a `git status` showing exactly the infrastructure files.

The ADR must record: the home store and its key derivation; that ahm makes no network requests; that records are not in git; fresh-checkout behavior (`ahm` exits 1 with `store_dir_unreadable` until `ahm init` runs); the one-backlog-per-repository caveat (clones and worktrees share the store key derived from `origin`); that issues remain an intake channel while ahm is the working backlog; the definition of done; the accepted limitations (CI never exercises ahm; blocked-reason warnings for imported tasks; the strict-acceptance consequence for issues without acceptance sections; `docs/adr/index.md` gitignored, unlike cake-repl which commits it); and the pinned ahm source build. cake-repl's equivalent decision is `/Users/travisennis/Projects/cake/cake-repl/docs/adr/013-store-ahm-task-records-in-the-user-level-home-store.md`.

**M2 --- Write and validate the import script (prototyping milestone).** Goal: `scripts/import-gh-issues-to-ahm.py` produces exactly the JSON array the importer accepts, and a dry run proves it. Work: the script fetches the open issues with `gh`, the board fields with `gh project item-list 1 --owner travisennis --format json`, comments per issue, native blocked-by links, and sub-issue parents; maps the fields per the mapping below; emits the array in ascending issue-number order (this order fixes id allocation); writes the array and a baseline report to paths outside the repository; and invokes `ahm task import --from-file`. Field mapping: board Status `Backlog → Open`, `Ready → Pending`, `In Progress → In Progress`, `Blocked → Blocked`; the seven parent trackers import as `Tracking` regardless of board status; Projects v2 `Priority` → `priority` and `Effort` → `effort`; labels port 1:1; `external_ref` = the issue URL; `created` = the issue's RFC3339 timestamp; `depends_on` from bullet
refs to imported records as `@ref`, merged with native blocked-by links, with closed-target refs dropped and reported; comments written as `## Comments` entries in the form `**<RFC3339>** — _<login>_: <text>`; body edits: strip `## Migrated from ahm` sections, strip `## Depends on` sections (front matter is authoritative), normalize `- [ ]` / `- [x]` to `-` inside acceptance sections only, and absolutize relative Markdown links to `https://github.com/travisennis/cake/...` while stripping any target that does not resolve. Result: `ahm --dry-run task import --from-file <path>` reports the planned allocation with no refusals --- the migration task is 001, imported records take 002 upward, and trackers get lettered children. Proof: the dry-run transcript. Promote the prototype when the dry run is clean; there is no parallel implementation to retire.

**M3 --- Perform the move.** Goal: the store holds the migrated backlog and GitHub reflects the move. Work: freeze the baseline (the script's report: open and closed counts, board counts, tracker list and children, link and dependency findings); run the import for real; verify with the formula under Validation and Acceptance; then close each imported issue with a comment linking its ahm task, reading the ref-to-id mapping from the importer's report JSON (nothing closes if the import refuses); keep the import JSON and the issue-number list with the `~/.ahm` backup, not in git. Result: `ahm task ready` is the only live queue, and GitHub shows the closed issues with linking comments. Proof: the verification commands and a spot-check of two closed issues.

**M4 --- Rewrite the workflow surface and delete the GitHub machinery.** Goal: no workflow instruction treats GitHub Issues as the task authority. Core: rewrite `docs/workflow/tasks.md` by adapting cake-repl's copy of ahm's task workflow (`/Users/travisennis/Projects/cake/cake-repl/docs/workflow/tasks.md`, itself a near-verbatim adapt of ahm's `docs/workflow/tasks.md`), adjusting it to cake's conventions (camelCase headings, `just`-based command catalog) and this repository's storage section, and keeping the lifecycle section from over-fitting to pull requests. Then: `AGENTS.md` (operating loop gains `ahm prime`, `ahm task start`, `ahm task complete`; step 6's tracking requirement becomes the ahm task), `CONTRIBUTING.md` (`just pr task=N`; the managed-records sentence; the pull-request-body rule, since `Closes #` no longer auto-closes; the `.ahm area:*` aside), the four agent files listed under Context and Orientation, `.github/pull_request_template.md`,
`docs/runbooks/parallel-worktrees.md` (the lifecycle sentence and the `issue=215` example), `docs/automations/dependency-sweep.md` (its "normal issue workflow" wording; automation issues remain GitHub intake), `docs/adr/README.md` (the ADR-trigger list and the `informed: issue NNN` template line), `docs/workflow/research.md` (seven places naming issues as the actionable-work channel), `docs/workflow/exec-plans.md` (the lifecycle sentence), and `docs/domain-glossary.md` (open questions no longer "belong in GitHub issues"). Leave `docs/guardrails/agent-instructions.md` and `docs/exec-plans/completed/**` unchanged; their issue references are history. Delete `scripts/claim-issue.sh`, `scripts/list-ready-issues.sh`, the `claim` / `unclaim` / `ready-queue` recipes in the `justfile`, and the `issue=` branch in `scripts/just-pr.sh`; add a `task=N` option to `scripts/just-pr.sh` that runs `ahm task comment N` with the pull request URL, with cases in `scripts/test-just-pr.sh` and the `justfile`
usage comment updated. Keep `.github/labels.yml`, `scripts/sync-labels.py`, and `label-governance.yml` (PR labels and intake), and leave `.github/workflows/scheduled.yml` unchanged. Result: the workflow surface names ahm, not GitHub Issues. Proof: the search under Validation and Acceptance plus `just docs-check`, `just check-scripts`, and the routed pre-push gate.

**M5 --- Handoff.** Run the routed gates, update the ADR and workflow docs if discoveries changed them, run the `preflight` skill in a subagent, open the content pull request, and move this plan to `docs/exec-plans/completed/` when the migration is complete.

## Concrete Steps

All commands run from `/Users/travisennis/Projects/cake/cake` unless stated.

M1:

```
ahm init
# edit .ahm/config.json: set "strict_acceptance": true
git add .ahm/config.json .ahm/.gitignore .gitignore
ahm status          # exits 0
```

Edit `scripts/classify-changes.sh` (add `.ahm/*` to the code arm) and `scripts/test-classify-changes.sh` (a fixture that commits `.ahm/config.json` and asserts the `code` class), then:

```
just test-classify-changes
```

Write the ADR and the rubric edit, then:

```
panache format docs/adr/041-*.md docs/exec-plans/active/ahm-task-records-migration.md
just docs-check
just check-scripts
```

Create and start the migration task:

```
ahm task create "Migrate task tracking from GitHub Issues to ahm" \
  --labels "type:chore,area:config,area:docs" --priority P1 --effort XL
ahm task accept 001
ahm task start 001
```

M2 (content branch):

```
python3 scripts/import-gh-issues-to-ahm.py --fetch --output /tmp/ahm-import.json --baseline /tmp/ahm-baseline.json
ahm --dry-run task import --from-file /tmp/ahm-import.json
```

M3:

```
python3 scripts/import-gh-issues-to-ahm.py --import --output /tmp/ahm-import.json --report /tmp/ahm-import-report.json
ahm status
ahm task list --status Blocked | wc -l
ahm task ready | wc -l
python3 scripts/import-gh-issues-to-ahm.py --close-issues --report /tmp/ahm-import-report.json
```

M4:

```
rg -n "gh issue|gh project" AGENTS.md CONTRIBUTING.md docs/workflow docs/runbooks docs/automations docs/domain-glossary.md .agents .github/pull_request_template.md
just docs-check
just check-scripts
just pre-push
```

## Validation and Acceptance

M3 verification formula (counts captured in the same hour on both sides): Ready_after = gh_ready; Blocked_after = gh_blocked − trackers currently Blocked (1 on 2026-10-09); Open_after = gh_backlog − trackers currently Backlog (6 on 2026-10-09); the seven trackers import as `Tracking`. `ahm status` exits 0 and reports no errors; it is expected to warn with `task_blocked_missing_reason` for each imported Blocked task (\~44 on 2026-10-09), and that count is compared against the baseline. No `markdown_link_missing` findings appear. The seven trackers show lettered children (`322a`, `343a`, ...), and `ahm task ready` excludes a tracker while its children are open. Every migrated issue is closed on GitHub with a comment naming its ahm task, and each task's `external_ref` points back at the issue URL.

M1 acceptance: `ahm status` exits 0 in the checkout; `just test-classify-changes` passes with the `.ahm/*` fixture; `docs/adr/index.md` exists in the working tree and is ignored by git; the rubric contains the ahm allowance; `.cake/settings.toml` lists `~/.ahm`; `just docs-check` and `just check-scripts` pass. M4 acceptance: the `rg` sweep above finds no workflow instruction that treats GitHub Issues as the task authority; `just pr task=N` comments the pull request URL on task N, exercised by `scripts/test-just-pr.sh`.

## Idempotence and Recovery

`ahm init` reconciles and is safe to re-run. The classifier, fixture, ADR, and rubric edits are ordinary file edits. The import is all-or-nothing: exit 2 means malformed input, exit 1 means a semantic refusal with nothing written, and an ordinary write failure rolls back. Re-running a *successful* import creates duplicate records, because ahm has no delete command; recovery is deleting the duplicate record files under `~/.ahm/projects/cake-d8f8727b/tasks/` and running `ahm index`. Issue closure happens only after verification, and closing is reversible by reopening. If the store is lost, restore by re-importing the saved JSON or by re-fetching the imported issues by number (closed issues remain readable). The migration task (001) and its record are not in git; that is intended.

## Artifacts and Notes

M2 snapshot at `2026-10-10T01:18:09Z` (2026-10-09 local): 141 open / 243 closed issues, board counts Ready 64 / Blocked 45 / Backlog 32; 7 trackers with 32 open children; 56 comments. The import plans Pending 64 / Blocked 44 / Open 26 / Tracking 7. All 141 report `outcome: planned` and empty errors; the first top-level record is issue 46 → task 002. Five historical relative documentation links become absolute GitHub links. Dependency #46 → #60 is dropped and flagged for manual review because #60 was closed as not planned. Snapshot counts are illustrative until M3 refreshes the baseline.

M2 verification: `just ahm-import-check` passed nine offline fixtures; `just check-scripts` passed; `just check` passed outside the outer sandbox after the restricted run failed on localhost mock-server and filesystem permission denials; `just docs-check` passed outside the outer sandbox after its git invocation was denied; `git diff --check` passed. No Rust compatibility surface changed. Coverage, release build, and Linux runtime checks were not repeated for this script prototype; the full migration gate remains M5.

M2 preflight used three sequential passes for an external-integration change, including both new scripts in the review target. Context: root AGENTS.md (no nested instructions), task 001, this ExecPlan, task and ExecPlan workflows, CONTRIBUTING.md, ADR 041, and ahm's actual import implementation. Rules review kept M2 preview-only and left the plan active; correctness review fixed historical docs links and checked status, tracker, label-string, dependency and comment mappings against the import contract; simplification review kept a stdlib-only script without a second importer. Added regression fixtures for API errors and preview enforcement. No further in-scope findings remain; M3 must review the cancelled dependency and refresh the snapshot.

M2 artifacts live outside git: `/tmp/ahm-snapshot.json` (raw fetch), `/tmp/ahm-import.json`, `/tmp/ahm-baseline.json`, and `/tmp/ahm-dry-run.json` (ahm's structured allocation report). Replay with `python3 scripts/import-gh-issues-to-ahm.py --snapshot /tmp/ahm-snapshot.json --output /tmp/ahm-import.json --baseline /tmp/ahm-baseline.json`. The script rejects artifact paths within the checkout and distinct artifacts sharing a path. M3 must refresh and preserve these temporary artifacts with the store backup before importing; no task records or GitHub issues were mutated by M2.

M1 health verification on 2026-10-09: `ahm store path` resolved to `~/.ahm/projects/cake-d8f8727b/tasks`; `ahm status` exited 0 with home records, strict acceptance enabled, task 001 In Progress, and no validation findings. `git check-ignore docs/adr/index.md` confirmed the generated index is ignored. `just test-classify-changes`, `just docs-check`, and `just check-scripts` passed.

`just check` passed with elevated execution after the restricted attempt failed. A focused `bash_check_renders_allow_verdict` rerun identified a localhost mock-server bind denial (`Operation not permitted`). The successful gate passed 1710 unit tests (2 ignored), integration suites, Clippy, formatting, complexity, and script gates. `cargo test --all-features provisioned_grant_allows_writes_without_granting_parent_or_sibling -- --nocapture` also passed outside the outer sandbox, exercising macOS writes inside the grant, denial at parent and sibling paths, and read-only denial. Linux platform execution and the broader `just check-full` suite were not run locally; Linux coverage remains CI's responsibility. No Rust implementation changed.

Preflight reviewed the existing branch plan and all M1 files in three sequential passes: rules and documentation, correctness and source-of-truth, and simplification. Context included root AGENTS.md (no nested instructions), this plan, task 001, CONTRIBUTING.md, the security authority, ADR guidance, and ADRs 019/040/041. Useful findings were corrected: the ADR 040 link, actual ahm CLI syntax and lifecycle, and binary/source pin mismatch. The shared writable store is an intentional approved grant; no additional parser, corpus cases, or workflow rewrite was added. Contributor workflow replacement remains M4.

The dry-run report has this shape (illustrative):

```
$ ahm --dry-run task import --from-file /tmp/ahm-import.json
record 1: planned
  ref: 46
  id: 002
  path: ~/.ahm/projects/cake-d8f8727b/tasks/active/002.md
  parent: -
  depends_on: -
```

The measured backlog numbers used in this plan (141 open, 64/32/45 board, 39/62/40 acceptance, 47/29/18 dependencies, 5 links in 2 bodies, 30 parented issues, 50/58 comments) come from a 2026-10-09 re-measurement and are illustrative; the move-time baseline is authoritative.

## Interfaces and Dependencies

`ahm` (source build; the pin moves to the ahm HEAD at move time) provides `init`, `status`, `doctor`, `prime`, `task …`, `adr create`, `index`, and `store path`; its import contract is ADR 025 in `/Users/travisennis/Projects/ahm/docs/adr/025-import-task-batches-with-prevalidation-and-rollback.md`. `gh` provides issue, project, and REST access for the import and the closure step. `panache` formats and lints git-tracked Markdown. `scripts/classify-changes.sh` provides the docs/code classification consumed by the pre-push gate and CI, and `.github/workflows/ci.yml` runs it together with `scripts/test-classify-changes.sh`. No Rust code, tool schema, or protocol shape changes; the only non-documentation configuration is `.ahm/config.json`, `.gitignore`, `.cake/settings.toml`, and the two scripts.

## Outcomes & Retrospective

M1 has bootstrapped a healthy home store and the migration's task 001. The infrastructure keeps task records out of git, requires acceptance evidence, and routes `.ahm/` through code checks. M1 is integrated and M2 has produced a validated import prototype with offline regression fixtures. M3--M5 and completion of task 001 remain; the backlog has not moved yet.

Revision note (2026-10-09): Recorded M1 implementation, local health and script verification, preflight corrections, and the infrastructure source pin. Corrected task creation and the accept-before-start sequence against the installed CLI.

Revision note (2026-10-09, M2): Added the live snapshot and dry-run evidence, offline fixture gate, milestone-scoped preview decision, and three-pass preflight results. M3--M5 remain.
