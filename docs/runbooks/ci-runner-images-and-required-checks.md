# CI Runner Images and Required Checks

Use this runbook when changing a `runs-on` label, renaming a workflow job, changing a job's `name:`, or changing a matrix that expands into job names. Those edits reach beyond the workflow file: job names are the identifiers branch protection uses, and runner labels decide which operating system Cake's platform-specific code is exercised on.

## Required checks are job display names

The `master` ruleset requires status checks by display name, not by job id. Inspect the current list before changing any job name:

```sh
gh ruleset list --repo travisennis/cake
gh api repos/travisennis/cake/rulesets/<ruleset-id> \
  --jq '.rules[] | select(.type == "required_status_checks").parameters.required_status_checks[].context'
```

The contexts come from each job's `name:` in `.github/workflows/ci.yml`, with matrix values already expanded. `Clippy (all-features)` and `Clippy (no-default-features)` are two separate required contexts produced by one job through `name: Clippy (${{ matrix.features.name }})`. Renaming the job, editing the `features.name` matrix values, or adding a matrix dimension changes those strings, and a required context that no longer reports blocks every pull request while appearing to be a protection setting rather than a typo.

`Detect Changes` and `Coverage` are deliberately absent from the required list. `Coverage` carries an `if:` condition, and a conditional job makes a poor required context because its reporting depends on which paths a pull request touched.

`Linux Test` is required alongside `Test`: it reports on every run and covers the Landlock security path.

## Updating the ruleset

Ruleset edits are repository configuration, not a git change, so they do not travel with the branch. `gh ruleset` can read but not write; updates go through `gh api`. Fetch the ruleset, keep only its editable fields, edit the contexts, and put it back:

```sh
gh api repos/travisennis/cake/rulesets/<ruleset-id> > /tmp/cake-ruleset.json
jq '{name, target, enforcement, conditions, bypass_actors, rules}' \
  /tmp/cake-ruleset.json > /tmp/cake-ruleset.update.json
# Edit /tmp/cake-ruleset.update.json so the required contexts match the job
# names the new workflow actually produces.
gh api -X PUT repos/travisennis/cake/rulesets/<ruleset-id> \
  --input /tmp/cake-ruleset.update.json
```

Sequence the two changes so protection is never enforcing a name nothing reports: land the workflow rename first and update the ruleset immediately after, or add the new context alongside the old one and remove the old one once it stops reporting.

State in the pull request which contexts were inspected and which were changed. If you lack permission to edit the ruleset, open the pull request anyway and lead the handoff with the exact contexts a maintainer must change.

## Runner labels and platform coverage

Cake's sandbox is platform-specific, so the runner label decides which enforcement path CI actually executes.

`Test` and `Coverage` run on `macos-latest`, so the Seatbelt path in `src/clients/tools/sandbox/` is exercised there. `Test` requires the focused read-only enforcement tests to apply a real profile; it does not accept the nested-Seatbelt skip used by ordinary local test runs. `sandbox-exec` behavior and profile handling vary across macOS releases, so a `macos-latest` migration to a new image family can change sandbox test results with no change to Cake. When investigating a sandbox test that began failing without a related code change, check whether the runner image moved before looking for a regression; see [Debugging Sandbox Denials](debugging-sandbox.md).

The Linux side runs `cargo check --all-features` (`Linux Check`) on `ubuntu-latest`. `Linux Test` is pinned to `ubuntu-24.04`: it runs the sandbox module tests, then requires the cross-platform read-only allow/deny tests to execute against the runner kernel's Landlock enforcement. The required behavior test first executes an allowed read-only script and then verifies denied mutations, so sandbox initialization failure cannot create a false pass.

Kernel-dependent security tests must use an explicit Ubuntu image label whose kernel supports the configured Landlock ABI. A required enforcement test fails closed if that pinned image can no longer provide full enforcement; optional kernel-dependent tests should instead report an explicit skip on unsupported kernels. Do not use an unpinned `ubuntu-latest` runner for evidence that a specific Landlock ABI works.

## Release runners are a compatibility decision

`.github/workflows/release.yml` builds published artifacts. The Linux artifact is built on `ubuntu-latest`, which determines the glibc version the released binary links against and therefore the oldest Linux distribution that can run it.

Changing that label is a compatibility change to a published artifact, not runner maintenance. Decide the intended glibc floor first and record it; do not adjust the release matrix as part of a mechanical runner-label pass. The macOS release targets are cross-compiled from one runner and are less sensitive, but the same rule applies to dropping or adding a target.

## Validation

Workflow-only changes:

```sh
git diff --check
panache format --check <changed markdown>
```

Run `actionlint` against the changed workflows if it is available, and say so in the handoff if it is not. A workflow change cannot be fully validated locally: confirm the job names the run actually produces against the required contexts after the first CI run on the branch.
