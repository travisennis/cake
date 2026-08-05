# Automation conventions

An automation is recurring repository maintenance that runs on a schedule rather than in response to a request: dependency sweeps, toolchain and runner-image checks, advisory audits. This document defines what automations may do and how they report. Each automation's own procedure lives in its own document in this directory.

Cake is the agent CLI such an automation would run, so an agent-run automation opening pull requests here is Cake operating on itself. The conventions below exist to keep that legible.

## The specification lives in the repository

Keep the scheduler's prompt slim. It should identify the role and point at the automation's document in this directory; the document holds the actual procedure, scope, and limits.

The prompt is invisible to review. A document in this directory is versioned, diffable, and changes through the same pull request process as everything else, so behavior drift is visible rather than silent.

## Marking machine-authored output

Automation-authored pull request and issue comments start with the stable prefix `Cake automation note:`.

An automation reading a thread needs to tell its own prior output from human feedback. Without a stable marker it will treat its own note as a maintainer's instruction, and a learning loop built on that is reinforcing its own noise.

Treat comments carrying the prefix as automation state, never as guidance.

## Do nothing visibly

An automation that finds no change to make must not create a branch, commit, or pull request. Report the check and stop.

The point of a scheduled check is usually that it finds nothing. An automation whose only visible output is a pull request has an incentive to manufacture one, and a repository whose maintainer learns to skim automation pull requests has lost the check.

## One owner per surface

Each automation states which files and version surfaces it owns, and which belong to something else. Dependabot owns the surfaces its manifests cover. Where an automation's scope touches a surface another automation or tool already maintains, the document says so explicitly.

Two automations updating one pin produce conflicting pull requests and a race whose winner depends on schedule order.

## Feedback updates the document

When a maintainer corrects an automation's output --- a rejected pull request, a review comment, a failed validation --- update that automation's document before the same class of change is attempted again.

An automation cannot learn between runs. Its document is its memory, so a correction that is not written down will be re-litigated on the next run. This mirrors the escalation rule in [AGENTS.md](../../AGENTS.md): repeated findings of the same class indicate a design problem, not a patching problem.

## Current automations

### Scheduled checks

`.github/workflows/scheduled.yml` runs weekly, Sundays at 00:00 UTC, and on manual dispatch. It is a plain CI cron rather than an agent role, and it opens no pull requests.

  | Job        | Check                                | Gates |
  | ---------- | ------------------------------------ | ----- |
  | `deny`     | `cargo deny check advisories`        | yes   |
  | `outdated` | `cargo outdated`                     | no    |
  | `udeps`    | `cargo +nightly udeps --all-targets` | yes   |
  | `msrv`     | `cargo check` on the MSRV toolchain  | yes   |
  | `docs`     | `cargo doc` with warnings denied     | yes   |

The `outdated` job runs with `|| true`, so it cannot fail and its report reaches only the workflow log. It reports staleness without producing a queue anyone works. Whether it should gate, route somewhere, or be removed is part of #80.

[Dependency and supply chain posture](../dependencies.md) is the policy these checks partially enforce.
