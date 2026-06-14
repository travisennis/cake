---
status: accepted
date: 2026-06-06
---
# Linux Landlock As Default Dependency

## Context

Cake uses Landlock for OS-level filesystem sandboxing on Linux. Originally,
Landlock was an optional Cargo feature (`landlock`) that had to be explicitly
enabled. ADR 006 decided that official Linux release artifacts would be built
with `--features landlock`, but source builds and `cargo install` required the
user to know about the feature flag.

The practical problem was:

1. A user running `cargo install cake` on Linux got a binary **without**
   Landlock — no warning, no build error, just a silently less-secure binary.
2. The `landlock` feature flag created an unnecessary distinction between
   "official release" and "source build" that users should not need to care
   about.
3. The fail-closed error message referred users to `--features landlock`, which
   required a rebuild — a poor first-run experience.

## Decision

Make Landlock a **target-specific non-optional dependency** on Linux using
Cargo's `[target.'cfg(target_os = "linux")'.dependencies]` mechanism. This
eliminates the `landlock` feature entirely:

- On Linux, Landlock is always compiled in — no feature flag needed.
- On macOS and Windows, the dependency is not resolved at all.
- `cargo install cake` on Linux automatically includes Landlock.

## Rationale

- **Consistent behavior**: `cargo install cake` and the official release binary
  behave identically on Linux.
- **No user-facing feature flag**: Users don't need to know about Landlock as a
  Cargo feature. It's just part of the Linux build.
- **No global default pollution**: Because the dependency is declared under
  `[target.'cfg(target_os = "linux")'.dependencies]`, it never affects macOS,
  Windows, or any other non-Linux platform.
- **Simpler CI**: The release workflow no longer needs a conditional
  `--features landlock` for the Linux target.

## Consequences

- **Positive**: All Linux builds (release, debug, `cargo install`) include
  Landlock by default.
- **Positive**: No more silent security gap for `cargo install` users.
- **Positive**: Simpler `Cargo.toml` — no `[features]` section needed anymore.
- **Positive**: Simpler release workflow — no per-target conditional build
  flags.
- **Positive**: The fail-closed path for kernels without Landlock support is
  unchanged — it's a runtime check, not a compile-time one.
- **Negative**: The `landlock` Cargo feature is removed, so downstream
  packagers who were relying on `--features landlock` to opt in no longer have
  that toggle. On Linux, they get Landlock unconditionally; on other platforms
  the feature was inoperative anyway.
- **Negative**: The `--no-default-features` test on Linux no longer tests a
  Landlock-less build, since Landlock is not a feature. The runtime fail-closed
  behavior is still exercised on Linux kernels without Landlock support.

## Alternatives Considered

- **Make `landlock` a default feature**: Rejected because Cargo `[features]`
  are global — `default = ["landlock"]` would attempt to compile the Linux-only
  `landlock` crate on macOS and Windows, causing build failures.
- **Keep the old policy (ADR 006)**: Rejected because it created a security
  trap for `cargo install` users.

## References

- Task 100: Decide Linux Landlock Default Feature Policy
- Task 179: Reassess Linux Landlock Default Build Policy
- ADR 006: Linux Landlock Release Artifacts (superseded)
- `Cargo.toml`
- `.github/workflows/release.yml`
- `docs/design-docs/sandbox.md`

