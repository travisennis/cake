# CI Runner Images

Use this runbook when changing a `runs-on` label, renaming a workflow job, changing a job's `name:`, or changing a matrix that expands into job names. Runner labels decide which operating system Cake's platform-specific code is exercised on, and a rename changes the check name that appears in the GitHub UI and in `gh` output. `master` has no required-check ruleset, only a safety-only ruleset that blocks deletion and force-pushes, so a rename cannot block a pull request; keep names stable anyway so CI history stays readable.

## Job names

Job `name:` values are the check names GitHub reports. `Clippy (all-features)` comes from the Clippy job; renaming the job or changing its `name:` changes that string. `Format`, `Licenses`, `Complexity`, `Docs`, `Build`, and `Linux Check` are likewise stable names.

`Conventional PR Title` comes from `.github/workflows/pr-title.yml`. It runs on pull request creation, pushes, reopening, and edits, so changing a title invalidates the previous result before squash merge. Keep the workflow unfiltered by base branch so stacked pull requests receive the check too.

## Runner labels and platform coverage

Cake's sandbox is platform-specific, so the runner label decides which enforcement path CI actually executes.

`Format`, `Clippy (all-features)`, `Complexity`, `Docs`, and `Build` run on `macos-latest`, so the Seatbelt path in `src/clients/tools/sandbox/` is the one compiled and linked there. `sandbox-exec` behavior and profile handling vary across macOS releases, so a `macos-latest` migration to a new image family can change sandbox behavior with no change to Cake. When investigating a sandbox failure, check whether the runner image moved before looking for a regression; use the `debugging-cake-sandbox` skill.

The Linux side runs `cargo check --all-features` (`Linux Check`) on `ubuntu-latest`. It compiles the Landlock path but does not execute it, so verify Landlock behavior by hand on Linux when a change touches that path.

## Release runners are a compatibility decision

`.github/workflows/release.yml` builds published artifacts. The Linux artifact is built on `ubuntu-latest`, which determines the glibc version the released binary links against and therefore the oldest Linux distribution that can run it.

Changing that label is a compatibility change to a published artifact, not runner maintenance. Decide the intended glibc floor first and record it; do not adjust the release matrix as part of a mechanical runner-label pass. The macOS release targets are cross-compiled from one runner and are less sensitive, but the same rule applies to dropping or adding a target.

## Validation

Workflow-only changes:

```sh
git diff --check
panache format --check <changed markdown>
```

Run `actionlint` against the changed workflows if it is available, and say so in the handoff if it is not. A workflow change cannot be fully validated locally: confirm the job names the run actually produces after the first CI run.
