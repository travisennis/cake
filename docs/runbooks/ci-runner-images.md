# CI Runner Images

Use this runbook when changing a `runs-on` label, renaming a workflow job, changing a job's `name:`, or changing a matrix that expands into job names. Runner labels decide which operating system Cake's platform-specific code is exercised on, and a rename changes the check name that appears in the GitHub UI and in `gh` output. `master` has no required-check ruleset, only a safety-only ruleset that blocks deletion and force-pushes, so a rename cannot block a pull request; keep names stable anyway so CI history stays readable.

## Job names

Job `name:` values are the check names GitHub reports. `Clippy (all-features)` comes from the Clippy job; renaming the job or changing its `name:` changes that string. `Detect Changes` and `Coverage` are deliberately not treated as stable gates: `Coverage` carries an `if:` condition, so whether it reports depends on which paths a pull request touched.

`Conventional PR Title` comes from `.github/workflows/pr-title.yml`. It runs on pull request creation, pushes, reopening, and edits, so changing a title invalidates the previous result before squash merge. Keep the workflow unfiltered by base branch so stacked pull requests receive the check too.

`Linux Test` runs alongside `Test` and covers the Landlock security path.

## Runner labels and platform coverage

Cake's sandbox is platform-specific, so the runner label decides which enforcement path CI actually executes.

`Test` and `Coverage` run on `macos-latest`, so the Seatbelt path in `src/clients/tools/sandbox/` is exercised there. `Test` runs the focused read-only enforcement tests against a real profile; it does not accept the nested-Seatbelt skip used by ordinary local test runs. `sandbox-exec` behavior and profile handling vary across macOS releases, so a `macos-latest` migration to a new image family can change sandbox test results with no change to Cake. When investigating a sandbox test that began failing without a related code change, check whether the runner image moved before looking for a regression; use the `debugging-cake-sandbox` skill.

The Linux side runs `cargo check --all-features` (`Linux Check`) on `ubuntu-latest`. `Linux Test` is pinned to `ubuntu-24.04`: it runs the sandbox module tests, then requires the cross-platform read-only allow/deny tests to execute against the runner kernel's Landlock enforcement. The behavior test first executes an allowed read-only script and then verifies denied mutations, so sandbox initialization failure cannot create a false pass.

Kernel-dependent security tests must use an explicit Ubuntu image label whose kernel supports the configured Landlock ABI. The enforcement test fails closed if that pinned image can no longer provide full enforcement; optional kernel-dependent tests should instead report an explicit skip on unsupported kernels. Do not use an unpinned `ubuntu-latest` runner for evidence that a specific Landlock ABI works.

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
