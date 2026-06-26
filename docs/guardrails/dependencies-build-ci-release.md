# Dependencies, Build, CI, And Release

## Scope

Read this before changing dependencies, Cargo features, Rust toolchain pins, build scripts, CI workflows, coverage baselines, release artifacts, or install behavior.

## Compatibility Surfaces

- `Cargo.toml` and `Cargo.lock` consistency.
- Rust toolchain and MSRV policy encoded in workflow pins.
- Feature flags and platform-specific compiled behavior.
- Release binary behavior, size, licensing, and dependency security posture.

## Required Checks

- Do not update dependencies unless explicitly asked.
- For dependency changes, run `just check-deps`; it is not part of `just ci`.
- When changing Rust version pins, run `just rust-version-check`.
- Follow [CONTRIBUTING.md](../../CONTRIBUTING.md) for final verification.

## Common Failure Modes

- Updating `Cargo.toml` without `Cargo.lock`, or the reverse.
- Changing dependency features without considering binary size or platform cfgs.
- Forgetting scheduled MSRV compatibility pins.
- Treating release workflow changes as ordinary CI-only edits.

## Related Docs

- [CONTRIBUTING.md](../../CONTRIBUTING.md)
- [ARCHITECTURE.md](../../ARCHITECTURE.md)
- [ADR 006: Linux Landlock Release Artifacts](../adr/006-linux-landlock-release-artifacts.md)
