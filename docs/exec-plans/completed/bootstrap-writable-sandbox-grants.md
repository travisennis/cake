# Bootstrap Writable Sandbox Grants

This ExecPlan is maintained per `docs/workflow/exec-plans.md` for issue #93.

## Purpose / Big Picture

A user who grants a new state or cache directory can write there on the first agent run without preparing it manually. Cake creates configured writable targets before tools start, but read-only runs and settings inspection do not create them. The sandbox grants only the declared target, not its parents.

## Progress

- [x] (2026-10-08) Travis accepted creation; issue #93 moved through Ready and was claimed.
- [x] (2026-10-08) Recorded ADR 040 and enumerated security bypass classes.
- [x] (2026-10-08) Implemented policy-aware preparation and focused CLI/platform allow/deny tests.
- [x] (2026-10-08) Passed final Rust, Markdown, coverage/CRAP gates and three-pass security preflight.
- [x] (2026-10-08) Completed repository records for review; PR handoff follows under issue #93.

## Surprises & Discoveries

Linux's `LandlockSandbox::prepare_rule_paths` discards absent paths. Retaining settings entries alone cannot solve bootstrap without creating an existing object or widening authority to a parent, which this plan rejects. Empty configured paths must stay ignored because recursive directory creation can succeed without creating an object for an empty path. Preparation errors need a typed configuration error to preserve exit code `3`. The first Linux CI run failed because the read-only probe used `touch` on an existing file, which updates timestamps that Landlock does not restrict. The corrected test attempts a content append and verifies unchanged contents; production enforcement is unchanged.

## Decision Log

Travis accepted ADR 040 on 2026-10-08: create missing configured writable directories during run initialization after policy resolution. Do not create them during settings reads or in read-only mode. Preserve warnings for missing read-only and invalid writable targets, warn on automatic creation, and abort on creation errors. Writable policies include danger-full-access.

## Outcomes & Retrospective

Both writable settings keys now provision absent directories during writable agent-run initialization. Read-only and diagnostic runs do not provision them. CLI tests confirm tilde/relative resolution, creation logs, missing read-only diagnostics, and failure before provider requests with exit code `3`. Real macOS Seatbelt and in-process validation tests allow target writes and deny parent/sibling writes; read-only OS demotion remains enforced. `just check`, `just docs-check`, formatting/whitespace checks, and `just check-coverage` pass (95.86% line coverage, no CRAP or CC exceedances). Linux Clippy is blocked by the missing `x86_64-linux-gnu-gcc`; the existing Linux Test CI job selects the new platform test for Landlock execution. Directory creation remains a trusted ambient-authority side effect and may leave partial parents on failure, as documented in ADR 040. No dependency, snapshot, or baseline changes were required.

## Context and Orientation

`src/main.rs` assembles agent-run resources; before this change, `valid_settings_dirs` filtered missing writable targets. `src/cli/sandbox_grants.rs` now owns their runtime preparation, with typed errors classified in `src/exit_code.rs`. `ToolContext` carries grants to both the in-process file tools and `SandboxConfig`; the latter produces Seatbelt profiles on macOS and Landlock rules on Linux. A grant is the permission to access the configured filesystem target. Its missing parents may need to be created, but must not become grants themselves.

## Plan of Work

Add a small runtime preparation helper in `src/cli/sandbox_grants.rs` and expose it within the crate from `src/cli/mod.rs`. Resolve the sandbox policy in `src/main.rs` before calling this helper. Replace the old writable filter with fallible preparation, preserving the separate read-only filter. Test both settings keys, missing parents, repeated initialization, read-only mode, existing files, creation errors, and symlinked paths.

Add platform sandbox coverage using a configured target outside the workspace: initialize it, write within it, and deny writes to a sibling and created parent. Exercise in-process Write validation against the same prepared target. Add CLI coverage for help/settings inspection and read-only startup. Update `docs/configuration.md` and `docs/security.md` with provisioning and its limits.

Run the focused tests, `just check`, and Markdown verification from the repository root. Perform three sequential preflight passes for rules, correctness, and simplification. Record any Linux execution gap accurately. Update the issue's acceptance and documentation notes, complete this plan, move it to `docs/exec-plans/completed/`, and open a labeled PR closing #93.

## Concrete Steps

From the repository root, use `cargo test sandbox_grants` for focused helper tests and `cargo test --test sandbox_grants` for CLI behavior. Run `cargo fmt`, `just check`, `just docs-check`, and `git diff --check` before handoff. The focused tests must pass with absent targets and deny ungranted writes.

## Validation and Acceptance

Both writable settings keys create their targets and a tool can write within them during the same run. Read-only policy creates no configured targets and demotes existing ones. Settings reads and help create none. Invalid targets retain diagnostics and creation failures stop initialization with context. Symlinked ancestors resolve consistently, and parent/sibling writes stay denied under macOS and Linux platform enforcement and in-process validation.

## Idempotence and Recovery

Reinitializing an existing directory preserves it. A failed recursive creation may leave intermediate parents; report that limitation and never remove them automatically. Tests use temporary directories. No migration, dependency change, baseline regeneration, or destructive cleanup is required.

## Artifacts and Notes

Issue #93 and ADR 040 record the accepted choice and security impact. The issue will carry verification and preflight evidence before the pull request opens.

## Interfaces and Dependencies

The new helper accepts the two resolved writable path lists and the existing `SandboxPolicy`, and returns a fallible vector of prepared `PathBuf` grants. Existing `ToolContext`, `SandboxConfig`, and platform strategies continue to enforce those grants. No settings keys, tool schemas, protocol shapes, or new dependencies are added.
