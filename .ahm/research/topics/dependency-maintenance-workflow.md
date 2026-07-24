# Dependency Maintenance Workflow

Status: synthesized
Created: 2026-07-24
Updated: 2026-07-24
Related tasks: 295
Related plans: -
Confidence: high

## Summary

Cake would benefit from automated dependency discovery that opens controlled
pull requests while leaving review and merge decisions to maintainers.
Renovate is the leading option because Cake's dependency surface extends beyond
`Cargo.toml` and GitHub Actions into Rust toolchain pins, `.mise.toml`,
standalone `cargo install` commands, and tool versions embedded in workflow
inputs. Dependabot remains a viable GitHub-native alternative, but it would
need a supplementary mechanism for those non-manifest versions.

No implementation choice has been made. The next decision is whether the
project is comfortable installing the Renovate GitHub App or prefers the
narrower, GitHub-native Dependabot workflow.

## Current Cake Dependency Surfaces

- Rust crates in `Cargo.toml` and `Cargo.lock`.
- GitHub Actions referenced from `.github/workflows/`.
- Stable Rust duplicated across `.mise.toml`, `rust-toolchain.toml`, and
  workflow `toolchain` inputs.
- A separate MSRV pin in `.github/workflows/scheduled.yml`.
- Standalone Cargo tools installed by `just setup`, including cargo-edit,
  cargo-deny, cargo-insta, cargo-llvm-cov, cargo-crap, Panache, prek, and
  Cocogitto.
- Additional scheduled-workflow tools such as cargo-outdated and cargo-udeps.
- Tool versions embedded in workflow inputs, including `panache@3.0.0`.
- The `just` tool is currently configured as `latest` in `.mise.toml`.

The weekly scheduled workflow already audits advisories, reports outdated and
unused dependencies, checks MSRV compatibility, and builds documentation. It
detects problems but does not provide a consistent update queue.

## Options Considered

### Renovate

Renovate has built-in managers for Cargo and GitHub Actions. Its custom regex
manager can discover and update versions in arbitrary files, which makes it
suitable for Cake's Rust/tooling pins and duplicated toolchain version.
Package rules support grouping, schedules, release-age policies, and separate
treatment of major updates.

Relevant documentation:

- <https://docs.renovatebot.com/modules/manager/cargo/>
- <https://docs.renovatebot.com/modules/manager/github-actions/>
- <https://docs.renovatebot.com/modules/manager/regex/>
- <https://docs.renovatebot.com/configuration-options/>

### Dependabot

Dependabot supports Cargo and GitHub Actions, scheduled version updates,
dependency groups, security updates, and configurable cooldowns. Its main
advantage is being GitHub-native. Its limitation for Cake is that it does not
provide an equivalent general-purpose custom manager for versions embedded in
`justfile`, `.mise.toml`, Rust toolchain files, and arbitrary workflow inputs.

Relevant documentation:

- <https://docs.github.com/en/code-security/concepts/supply-chain-security/dependabot-version-updates>
- <https://docs.github.com/en/code-security/how-tos/secure-your-supply-chain/secure-your-dependencies/auto-update-actions>
- <https://docs.github.com/en/code-security/tutorials/secure-your-dependencies/optimizing-pr-creation-version-updates>

## Recommended Workflow

Start with Renovate in review-only mode:

1. Run version discovery weekly.
2. Apply a seven-day minimum release age to routine version updates. Do not
   delay security updates.
3. Keep build, CI, and developer tools exactly pinned.
4. Group compatible Cargo patch and minor updates.
5. Group GitHub Actions updates separately.
6. Group developer and CI tools separately.
7. Update the stable Rust version atomically across `.mise.toml`,
   `rust-toolchain.toml`, and workflow inputs.
8. Keep MSRV changes separate and manually reviewed.
9. Keep major-version updates as individual pull requests.
10. Limit concurrent maintenance pull requests to approximately three.
11. Do not enable automerge initially. Consider patch-only automerge only after
    the workflow has demonstrated reliable CI and review behavior.
12. Use Renovate's dependency dashboard as the update queue.

Routine green updates should not require ahm tasks. Create a task when an update
requires migration work, exposes a project defect, is intentionally deferred,
or needs a durable design decision.

## Periodic Human Review

Automation should be complemented by a short monthly maintenance review:

- Review ignored or unresolved security advisories.
- Check for abandoned or unmaintained direct dependencies.
- Review updates that automation cannot manage.
- Resolve stale dependency-dashboard entries and long-lived update pull
  requests.
- Confirm pinned developer and CI tools still match contributor setup and
  workflow usage.
- Reassess whether low-risk patch updates are safe to automerge.

## Follow-ups

- Decide between Renovate and Dependabot.
- If Renovate is selected, decide whether to use the hosted GitHub App or a
  self-hosted runner.
- Inventory which `cargo install` tools should be exact pins and whether one
  file should become their source of truth.
- Define grouping boundaries and release-age exceptions before enabling update
  pull requests.
- Decide whether the existing scheduled outdated-dependency job should remain
  informational, be simplified, or be replaced after automation is stable.
