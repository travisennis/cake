---
status: accepted
date: 2026-10-09
decision-makers: Travis Ennis
---

# Store ahm task records in the user-level home store

## Context and Problem Statement

Cake's working backlog lives in GitHub Issues and Projects v2. Routine planning requires network access and remote mutations, although the work is local. The owner approved moving the working queue to ahm while retaining GitHub Issues for third-party reports. M1 bootstrapped the store; M3 imported the 141-issue working backlog and verified all task records against the frozen snapshot. M4 switched the workflow instructions and PR task-link tooling to ahm; M5 completed final validation and delegated preflight; the completed plan records content-PR handoff.

## Decision Drivers

- Let contributors manage local work without network requests.
- Keep planning churn out of product history while retaining reviewed ADRs.
- Share one backlog across local clones and worktrees of the same repository.
- Require acceptance evidence before completing tasks.

## Considered Options

- Retain GitHub Issues and Projects v2 as the working queue.
- Commit task records under `.ahm/tasks/`.
- Use ahm's user-level home store and keep GitHub Issues as intake.

## Decision Outcome

Chosen option: ahm's user-level home store, because local task transitions do not need a remote service or commits to the product repository. ahm makes no network requests. `.ahm/config.json` commits `tasks_location: "home"` and `strict_acceptance: true`; records live outside git in `~/.ahm/projects/cake-d8f8727b/tasks/`, with active, completed, and cancelled buckets. The key derives from the normalized `origin` remote `github.com/travisennis/cake`, rather than the checkout path. Clones and worktrees on one machine share a backlog; another machine needs its own initialized store and records. A fork or changed remote can resolve to another key.

A fresh checkout without its store exits 1 with `store_dir_unreadable` until `ahm init` runs. Initialization reconciles a partial store and generates indexes. ADR records remain committed under `docs/adr/`; `docs/adr/index.md` is generated and gitignored, unlike cake-repl's committed index.

The source-build pin at move time is ahm commit `e3dcacc0ac53ba416c8a09f8facde9ed6bc24457` (M1 used `020071747be4cda34e1783a30e97b49b7d9079a1`). Build that revision with `go build -o ~/go/bin/ahm ./cmd/ahm` from the ahm checkout. The installed binary was rebuilt from that clean source revision before the import; contributor setup is documented in [CONTRIBUTING.md](../../CONTRIBUTING.md).

After the move, GitHub Issues remains intake. Accepted reports become ahm tasks with the issue URL as `external_ref`, and the original issue is closed with a linking comment, except that rolling scheduled-failure issues stay open until a check run confirms resolution, preserving deduplication while failures persist. Task completion happens on the feature branch before the implementing commit; integration into `master` makes delivery true. An abandoned pull request reopens the task or returns it to `Pending`. Pull requests reference tasks explicitly; there is no automatic task closure.

### Security and Compatibility Impact

The repository's `[sandbox].writable` adds exactly `~/.ahm`. This deliberately permits writes to every project's task store under that directory, including shared records from other Cake worktrees. The reason is to let sandboxed ahm initialize and maintain the approved local backlog. The judge rubric permits routine additive and reversible ahm operations, while still assessing deletion, credential disclosure, uploads, and chained commands by their actual effects.

The bypass classes considered are broader path grants, symlink escapes, shell substitutions and chained destructive commands, and confusing another project's store with this project's records. No parser or command-name exemption is added. The existing sandbox path resolution and enforcement remain responsible for filesystem confinement on macOS Seatbelt and Linux Landlock; the grant uses the same settings resolution on both platforms. No fallback, fail-closed behavior, hook or toolbox authority changes. CLI output, tool protocols, sessions, and provider requests retain their contracts. `.ahm/*` takes the code verification route, including Markdown beneath that directory.

### Consequences

- Good, because routine backlog operations work offline and local worktrees use one queue.
- Good, because acceptance notes are required for completion: absent notes, `TODO` placeholders, and unchecked acceptance items refuse completion unless explicitly overridden with `--force`.
- Bad, because records are not versioned or synchronized by git. Back up `~/.ahm` together with the import JSON and imported issue-number list outside git. The M3 snapshot, import JSON, issue-number list, actual allocation report, verification and closure logs are preserved in `~/.ahm/backups/cake-migration-20261010T0130/` alongside the before/after store archives. Restore the full store archive to preserve task ids, task 001, relationships, and recorded progress as of that backup. Re-importing JSON or re-fetching closed issues reconstructs migration-time content; it is not an identity-preserving restore and cannot recover later edits. The original import started with task 001 present, so an empty-store replay assigns issue #46 to 001 instead of its published 002. Before using any reconstructed store, compare every allocated id and relationship with `import-report.json` and reconcile all published references; do not replace the authoritative store with an unverified reconstruction.
  Never replay an import into a store that already contains that batch, because successful imports allocate new records.
- Bad, because imported tasks without acceptance sections need notes added before completion or an explicit `--force`. Import normalization only removes acceptance checkboxes; it cannot supply missing evidence.
- Accepted limitation: imported Blocked tasks without reasons produce expected `task_blocked_missing_reason` warnings. Health checks must exit 0 with no errors; these warnings are compared against the import baseline.
- Accepted limitation: CI never exercises ahm or the machine-local backlog. Initialization and health verification remain local contributor checks.

## More Information

- [Migration ExecPlan](../exec-plans/completed/ahm-task-records-migration.md).
- [Project sandbox paths](019-project-customizable-sandbox-paths.md) and [startup grant preservation](040-bootstrap-writable-sandbox-grants.md).
- [Security and trust boundaries](../security.md).
- Verify storage with `ahm store path` and health with `ahm status`.
