# Collapse Equivalent Sandbox Path Classes

This ExecPlan is a living document, maintained per `docs/workflow/exec-plans.md`. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be kept current as work proceeds.

This plan implements issue 273 and ADR 023 (`docs/adr/023-two-sandbox-filesystem-path-classes.md`).

## Purpose / Big Picture

Cake users keep the same sandbox behavior, but maintainers get one internal collection for each effective filesystem authority: read/write/execute and read/execute. macOS Seatbelt and Linux Landlock will translate the same two collections, and the shared configuration builder will be the only owner of GitHub CLI and GitLab CLI filesystem paths. A maintainer can see the result in the smaller `SandboxConfig`, generated Seatbelt profile tests, Landlock classification tests, and real enforcement tests that execute a read-only script but deny its attempted write.

## Progress

- [x] (2026-08-26 00:00Z) Inspected issue 273, the sandbox implementation, security authority, platform tests, ADR workflow, and ExecPlan workflow; claimed the issue and created branch `refactor/sandbox-path-classes`.
- [x] (2026-08-26 00:00Z) Recorded ADR 023 and this ExecPlan before code changes.
- [x] (2026-08-26 16:20Z) Attempted the required macOS baseline; the test failed before execution because this cake session is already inside Seatbelt and nested profiles are unavailable.
- [x] (2026-08-26 16:35Z) Collapsed `SandboxConfig` and both platform translators to two ordinary filesystem path classes.
- [x] (2026-08-26 16:35Z) Removed the duplicate macOS SCM grants and added profile assertions that each shared SCM path emits exactly once.
- [x] (2026-08-26 16:40Z) Strengthened the read-only allow/deny test for both macOS and Linux and made real platform enforcement required in CI.
- [x] (2026-08-26 16:45Z) Updated `docs/security.md` and the CI runner runbook; focused tests and `just ci` pass locally.
- [x] (2026-08-26 16:50Z) Completed all three L-change preflight passes and applied the worthwhile findings.
- [x] (2026-08-26 17:19Z) Opened pull request #359 with the required security labels and issue linkage.
- [ ] Obtain passing required real Seatbelt and Landlock results in pull-request CI, complete the issue acceptance record, and archive this plan.

## Surprises & Discoveries

- Observation: `system_paths` and `readable` already receive identical `ReadFile | ReadDir | Execute` rights in Landlock and identical `file-read*` path rules plus global `process-exec` in Seatbelt. Evidence: `LandlockSandbox::prepare_ruleset` and `MacOsSandbox::generate_profile`.
- Observation: SCM CLI paths are already in the shared writable list and are demoted by `SandboxConfig::partition_read_only`, but macOS emits the same eight paths again through `append_scm_cli_rules`. Evidence: `SandboxConfig::extend_with_toolchain_paths` and `MacOsSandbox::append_scm_cli_rules`.
- Observation: macOS has real Seatbelt tests in `src/clients/tools/bash_tests.rs`, but Linux CI previously ran only sandbox module tests that did not require kernel enforcement. Evidence: the prior `.github/workflows/ci.yml` job `Linux Test`.
- Observation: this cake session is already inside Seatbelt, so `CAKE_REQUIRE_SANDBOX_TESTS=1 cargo test --all-features test_sandbox_read_only` fails with nested-profile `Operation not permitted` before exercising the generated profile. Evidence: both focused tests failed at `skip_if_sandbox_unavailable`; a direct `sandbox-exec` probe returned the same denial.
- Observation: real Landlock testing depends on the runner kernel supporting Cake's configured ABI V5. Evidence: `LandlockSandbox::prepare_ruleset` requests `ABI::V5`; the current GitHub `ubuntu-24.04` runner image documents kernel 6.17, which supports that ABI.

## Decision Log

- Decision: Use two explicit internal names, `writable` and `read_execute`, instead of retaining the ambiguous `readable` name. Rationale: each name states its effective authority and makes accidental loss of execute permission less likely. Date/Author: 2026-08-26, cake agent.
- Decision: Keep SCM CLI paths in the shared toolchain/integration path builder and remove the macOS-specific duplicate emitter. Rationale: the shared builder already feeds both platforms and read-only demotion, so it is the correct single owner. Date/Author: 2026-08-26, cake agent.
- Decision: Do not change specialized macOS rules. Rationale: device, SSH agent, Keychain, Mach, process, and network rules do not map only to ordinary filesystem read/write classes and are outside the accepted refactor. Date/Author: 2026-08-26, cake agent.
- Decision: Record ADR 023 because repository policy requires an ADR for security-sensitive sandbox-boundary work, even though the selected outcome preserves external behavior. Rationale: make the two-authority invariant and preserved platform-only capabilities durable. Date/Author: 2026-08-26, cake agent.
- Decision: Require the real Seatbelt and Landlock behavior tests in CI because the current nested local session cannot supply macOS proof and macOS cannot execute Linux Landlock. Rationale: the allow action in the same test prevents sandbox initialization failure from creating a false deny pass. Date/Author: 2026-08-26, cake agent.
- Decision: Pin `Linux Test` to `ubuntu-24.04`. Rationale: the required kernel-dependent test needs a stable image whose documented kernel supports Landlock ABI V5; `ubuntu-latest` is a moving target. Date/Author: 2026-08-26, cake agent.

## Outcomes & Retrospective

Implementation and local verification are complete. The sandbox now has one internal collection for each effective ordinary filesystem authority, both platform translators consume those two collections, and the shared configuration is the only owner of SCM CLI paths. Focused profile and classification tests pass, and `just ci` passes with 93.85% line coverage, no CRAP regression, and no cyclomatic-complexity exceedance. Pull request #359 is open. Required real Seatbelt and Landlock enforcement results remain pending in pull-request CI; this plan stays active until those results are recorded.

## Context and Orientation

Cake is a Rust 2024 CLI. Model-generated Bash commands run under a deny-default operating-system filesystem sandbox: Seatbelt through `sandbox-exec` on macOS and Landlock on Linux. `src/clients/tools/sandbox/mod.rs` builds `SandboxConfig` from the working directory, temporary directories, linked Git worktree directories, built-in toolchain and integration paths, user settings, `--add-dir`, and skill paths. The selected `SandboxPolicy` either keeps workspace and configured writable paths writable, demotes them to read-and-execute under `ReadOnly`, or skips OS sandbox application under `DangerFullAccess`.

`src/clients/tools/sandbox/macos.rs` translates `SandboxConfig` to a Seatbelt profile. Seatbelt uses `file-read* file-write*` for writable paths and `file-read*` plus the global `process-exec` rule for read-and-execute paths. `src/clients/tools/sandbox/linux.rs` translates the same two collections to Landlock rules. It uses all filesystem rights for writable paths and `ReadFile | ReadDir | Execute` for read-and-execute paths.

`src/clients/tools/bash_tests.rs` contains real sandbox execution tests. On macOS, `CAKE_REQUIRE_SANDBOX_TESTS=1` turns an unavailable Seatbelt sandbox into a test failure instead of a skip. `.github/workflows/ci.yml` owns the Linux runner and must explicitly execute a real Landlock path test.

Before editing this security boundary, the bypass classes to defend against are:

- A system, configuration, device, user read-only, skill, or read-only-demoted path could be omitted while the two old collections are merged, causing an unintended denial.
- A read-and-execute path could accidentally receive write authority, directly or through duplicate SCM rules, causing an authority expansion.
- A configured file could become a directory-subtree grant, allowing sibling access; conversely, directory grants could become file-only.
- Original and canonical forms could stop being granted, causing symlink-dependent behavior changes or accidental access through a broader ancestor.
- Removing the macOS SCM emitter could omit one of the eight GitHub CLI or GitLab CLI paths or fail to demote it under `ReadOnly`.
- A deny assertion could pass only because the platform sandbox failed to initialize. The same real-platform test must first prove an allowed read/execute action succeeds.
- A read-only executable could run and then mutate its own read-only path. The enforcement test must prove execution succeeds while the attempted mutation fails.
- Platform-only macOS rules could be changed as collateral churn. The diff and profile tests must show those rules remain untouched.
- Partial or unavailable Landlock enforcement could be mistaken for success. Existing fail-closed status handling must stay unchanged, and the Linux test must require a successful allowed command before checking denial.

## Plan of Work

First, run the focused current macOS tests with real Seatbelt required. Record whether the baseline passes before implementation.

In `src/clients/tools/sandbox/mod.rs`, replace `system_paths` and `readable` with one `read_execute` collection. Build it from the existing system and read-only path sources, additional directories, and skill directories. Keep original and canonical path handling and read-only policy demotion. Rename helpers and tests only where needed to make the two authorities explicit. Keep the eight SCM CLI paths in one shared list used by the common writable path builder.

In `src/clients/tools/sandbox/linux.rs`, reduce `RulePaths` to `writable` and `read_execute`, then emit one read/execute Landlock loop. Keep full-enforcement checks and pre-fork rule creation unchanged. Update focused classification tests.

In `src/clients/tools/sandbox/macos.rs`, generate ancestor and access rules from `writable` and `read_execute`. Remove `append_scm_cli_rules` and its call so SCM paths flow only through the shared config. Keep Git, SSH agent, Keychain, device, process, Mach, network, and locking rules unchanged. Update profile tests to prove one SCM rule is emitted in workspace-write, it is read-only after demotion, and no write rule remains under read-only policy.

In `src/clients/tools/bash_tests.rs`, make the existing read-only file execution test run on macOS and Linux. Strengthen its allowed script so it attempts to create a sibling marker: the script must start, but the marker must not appear. Keep the separate sibling-executable denial. Make the read-only workspace write-denial test run on both platforms. In `.github/workflows/ci.yml`, add these real Landlock tests to `Linux Test` after the focused sandbox module tests.

Update `docs/security.md` to state that ordinary filesystem grants are represented by two effective classes and that specialized platform capabilities stay separate. Do not change the user-facing policy or settings guarantees.

Finally, run formatting, focused tests, `cargo test`, `just ci`, the preflight skill, and diff review. Update this plan, the issue acceptance record, and documentation assessment. Move this file to `docs/exec-plans/completed/`, commit focused paths, push, and open a pull request with `Closes #273`, exactly one `type:security`, `area:sandbox`, and `risk:security-sensitive`.

## Concrete Steps

All commands run from `/Users/travisennis/Projects/cake` on branch `refactor/sandbox-path-classes`.

1. Run `CAKE_REQUIRE_SANDBOX_TESTS=1 cargo test --all-features test_sandbox_read_only` and expect the allowed script test and write-denial test to pass under real Seatbelt.
2. Edit the three files in `src/clients/tools/sandbox/`, then run `cargo test --all-features clients::tools::sandbox` and expect all focused unit and profile tests to pass.
3. Edit and run the cross-platform enforcement tests with `CAKE_REQUIRE_SANDBOX_TESTS=1 cargo test --all-features test_sandbox_read_only`; on macOS both tests must pass under Seatbelt. Linux CI must run the same filter under Landlock.
4. Run `cargo fmt -- --check`, `cargo test`, and `just ci`; each must exit zero.
5. Run the preflight skill, apply worthwhile findings, then review `git diff --check` and `git diff --stat`.

## Validation and Acceptance

The implementation is accepted when `SandboxConfig` and each platform translator have only writable and read/execute ordinary filesystem classes; the eight SCM CLI paths have one source owner; generated Seatbelt profile tests show no duplicate SCM grant; and a real sandbox test first executes a read-only script, then proves the script cannot create a file beside itself and a sibling executable is denied.

Real macOS enforcement must pass in the required `Test` CI step because this nested local session cannot apply another Seatbelt profile. Real Linux enforcement must pass in the required `Linux Test` CI job, which runs the same allow/deny behavior through Landlock on pinned `ubuntu-24.04`. `cargo test` and `just ci` must pass. The security impact statement is: no CLI, configuration, allowed path, permission, fallback, or fail-closed behavior changes; only equivalent internal classes and duplicate policy ownership are collapsed.

## Idempotence and Recovery

All tests and formatting commands are safe to rerun. Enforcement tests use temporary paths and check for denied markers; temporary directories clean themselves up. If a test exposes a changed authority, restore the relevant old source list or translator rule and compare generated profiles before proceeding. Do not weaken assertions or bypass unavailable platform enforcement. If `ci/cargo-crap-baseline.json` changes, inspect the coverage result and regenerate only through `just change-risk-baseline`; never resolve that file manually.

Issue and pull-request comments are additive. The issue body link update is safe to repeat if the exact implementation-record section is checked first.

## Artifacts and Notes

The required local gate passed with 1,324 unit tests plus integration suites, 93.85% total line coverage, no CRAP regression, and no cyclomatic-complexity exceedance. `cargo test --all-features clients::tools::sandbox` passed 56 focused tests before the final simplification pass. The optional `just clippy-linux` could not run because this host lacks the `x86_64-unknown-linux-gnu` Rust target and cross compiler. Required real-platform results remain pending in pull-request CI.

Revision note (2026-08-26): Updated the living sections after implementation and preflight, recorded the nested-Seatbelt baseline limitation, and pinned the Linux enforcement runner after review identified the Landlock ABI dependency.

Revision note (2026-08-26): Recorded pull request #359 and kept the plan active while its required real-platform enforcement checks run.

## Interfaces and Dependencies

`crate::clients::tools::sandbox::SandboxConfig` remains private to the tool implementation but ends with `writable: Vec<PathBuf>`, `read_execute: Vec<PathBuf>`, and `policy: SandboxPolicy`. `SandboxConfig::build` and `build_with_policy` keep their signatures. `SandboxStrategy::apply` keeps its signature and fail-closed behavior. No dependency changes are required. The CLI, settings schema, tool schema, and sandbox policy enum remain unchanged.
