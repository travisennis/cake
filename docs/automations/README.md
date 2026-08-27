# Automation conventions

An automation is recurring repository maintenance that runs on a schedule rather than in response to a request: dependency sweeps, toolchain and runner-image checks, advisory audits. This document defines what automations may do and how they report. Each automation's own procedure lives in its own document in this directory.

Cake is the agent CLI such an automation would run, so an agent-run automation opening pull requests here is Cake operating on itself. No agent-run automation exists yet, and the mechanism for running Cake on a schedule is undecided; see [#231](https://github.com/travisennis/cake/issues/231).

## The specification lives in the repository

Keep the scheduler's prompt slim. It should identify the role and point at the automation's document in this directory; the document holds the actual procedure, scope, and limits.

The prompt is invisible to review. A document in this directory is versioned, diffable, and changes through the same pull request process as everything else, so behavior drift is visible rather than silent.

## Marking machine-authored output

Automation-authored pull request and issue comments start with the stable prefix `Cake automation note:`.

An automation reading a thread needs to tell its own prior output from human feedback. Without a stable marker it will treat its own note as a maintainer's instruction, and a learning loop built on that is reinforcing its own noise.

Treat comments carrying the prefix as automation state, never as guidance.

## Current automations

### Dependency sweep

[`dependency-sweep.md`](dependency-sweep.md) defines the manual, review-only dependency-maintenance process. It starts with a deterministic version-surface report, keeps Dependabot as the owner for Cargo and GitHub Actions, and covers the Rust toolchain, local tools, Cargo-installed tools, and workflow tool inputs that Dependabot does not own. A future scheduler may invoke the same procedure after [#231](https://github.com/travisennis/cake/issues/231) decides how Cake runs unattended.

The report and procedure use the stable status vocabulary `no-work`, `actionable`, and `review-required`. They never merge or automerge changes. Safe work is grouped into bounded pull requests; ambiguous or high-risk work stops for a maintainer or produces an issue.

### Scheduled checks

`.github/workflows/scheduled.yml` runs weekly, Sundays at 00:00 UTC, and on manual dispatch. It is a plain CI cron rather than an agent role, and it opens no pull requests.

  | Job        | Check                                | Gates |
  | ---------- | ------------------------------------ | ----- |
  | `deny`     | `cargo deny check advisories`        | yes   |
  | `outdated` | `cargo outdated`                     | no    |
  | `udeps`    | `cargo +nightly udeps --all-targets` | yes   |
  | `msrv`     | `cargo check` on the MSRV toolchain  | yes   |
  | `docs`     | `cargo doc` with warnings denied     | yes   |

The `outdated` job remains informational by design: its report stays in the workflow log and does not open a pull request. The dependency sweep owns review of that signal and all non-Dependabot surfaces. A failed scheduled job is tracked through the issue workflow described in [#236](https://github.com/travisennis/cake/issues/236); fixing the historical documentation failure was [#230](https://github.com/travisennis/cake/issues/230).

[Dependency and supply chain posture](../dependencies.md) is the policy these checks partially enforce.
