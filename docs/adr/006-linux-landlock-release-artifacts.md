---
status: superseded by ADR-010
date: 2026-05-18
---

# Linux Landlock Release Artifacts

## Context

Cake treats the Bash tool's OS filesystem sandbox as the enforcement boundary for model-requested shell commands. On Linux, that sandbox uses Landlock, which was originally implemented as an optional Cargo feature because the dependency is Linux-only.

Before this decision, Linux release artifacts were built with the default feature set. Those binaries failed closed when sandboxing was enabled because Landlock support was not compiled in. Users could rebuild with `--features landlock`, but the distributed Linux binary did not match the expected sandboxed default.

## Decision

Official Linux release artifacts must be built with the `landlock` Cargo feature enabled. The global Cargo default feature set remains empty, and source builds continue to opt into Landlock explicitly with `--features landlock`.

CI must continue to cover both feature modes on Linux:

- `--all-features`, proving the Landlock path compiles and tests.
- `--no-default-features`, proving the fail-closed fallback still compiles.

## Rationale

- Users who download a Linux release binary should get the same sandboxed Bash default that macOS users get from the distributed binary.
- Keeping `default = []` avoids making a Linux-only dependency appear to be a cross-platform default feature.
- Retaining the no-default build keeps the explicit fail-closed behavior tested for source builds or downstream packaging choices that omit Landlock.

## Consequences

- **Positive**: Official Linux binaries enforce the Landlock sandbox by default on kernels where Landlock fully enforces the ruleset.
- **Positive**: Source builders and downstream packagers have an explicit, documented feature switch.
- **Negative**: The Linux release artifact now depends on the Landlock crate and may have a slightly larger binary or dependency surface.
- **Negative**: Older Linux kernels still require `CAKE_SANDBOX=off` for Bash execution because cake fails closed when Landlock cannot fully enforce the ruleset.

## Alternatives Considered

- **Enable `landlock` in Cargo `default` features**: Rejected because Cargo defaults are global, while Landlock is Linux-only. This makes the dependency policy less clear for non-Linux development.
- **Keep Landlock source-build only**: Rejected because official Linux artifacts would continue to fail closed for normal Bash usage unless users disabled sandboxing or rebuilt cake themselves.

## Superseded By

This ADR was superseded by [ADR 010](./010-linux-landlock-default-dependency.md), which makes Landlock a target-specific non-optional dependency on Linux so that all Linux builds (including `cargo install`) automatically include Landlock without requiring a feature flag.

## References

- Task 100: Decide Linux Landlock Default Feature Policy
- Task 179: Reassess Linux Landlock Default Build Policy
- `Cargo.toml`
- `.github/workflows/release.yml`
- `docs/design-docs/sandbox.md`

## Supersession

Superseded by [ADR-010](010-linux-landlock-default-dependency.md).
