# Align In-Process Read Grants With Bash Sandbox Grants

This ExecPlan is a living document, maintained per `docs/workflow/exec-plans.md`. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be kept current as work proceeds.

## Purpose / Big Picture

Cake's in-process file tools and its Bash sandbox disagree about which files the model may read. Bash can read Rust stdlib and cargo-registry source files under `~/.rustup` and `~/.cargo` because those are built-in sandbox grants. `Read` refuses the same files as "outside the working directory" because its validator only knows the working directory, temp directories, configured directories, `--add-dir`, and skill directories. The model then falls back to Bash and loses `Read`'s range, binary, size, and line-alignment handling.

After this change, `Read` and `ReadImage` can read an existing regular file under the same ordinary filesystem grants Bash already has, without a per-run `--add-dir`. `Edit` and `Write` can still change the working directory, temp directories, and configured writable directories, but built-in toolchain and system paths stay read-only to them. Credential-bearing paths such as `~/.ssh`, `~/.aws`, `~/.config/gh`, `~/.cargo/credentials.toml`, and `~/.npmrc` remain unreadable to the in-process tools even when the user grants an ancestor such as `~`.

A reviewer can see the change working with a synthetic home directory: put a source file under `$HOME/.cargo/registry/src/...`, run `Read` on it without `--add-dir`, and see the file contents; ask `Edit` to change the same file and see the existing read-only rejection; create `$HOME/.ssh/id_ed25519` with synthetic contents and see `Read` reject the path without putting the contents in the error. On Linux, `/proc/self/environ`, `/dev/fd`, and `/sys` remain unreadable through the in-process tools even though Bash's Landlock profile grants them.

The durable decision is recorded in `docs/adr/056-share-bash-read-grants-with-in-process-file-tools.md` (proposed). This plan implements that decision once it is accepted; it does not replace the judgment in the task or ADR.

## Progress

- [x] (2026-11-08) Read task 171, `AGENTS.md`, `docs/workflow/exec-plans.md`, `docs/security.md`, the sandbox module, the path validator, and the tool tests.
- [x] (2026-11-08) Started task 171 and wrote this ExecPlan. No implementation code has changed.
- [x] (2026-11-09) Draft ADR 056 (`docs/adr/056-share-bash-read-grants-with-in-process-file-tools.md`) as proposed. Acceptance remains the gate before code changes.
- [x] (2026-11-09) Accept ADR 056 and add `PathGrants`, `DeniedPath`, the credential deny list, and the Bash/in-process projections. `SandboxConfig::build_with_policy` now projects from `PathGrants`; the in-process validator still uses the legacy path list, so behavior is unchanged.
- [x] (2026-11-09) Cache `PathGrants` on `ToolContext`, make `sandbox_policy` private with `sandbox_policy()`/`set_sandbox_policy()`, add `refresh_path_grants()`, and make `SandboxConfig::build` project from the cached value. Tests that change policy use the setter; tests that change path inputs refresh. Added direct cache and policy-rebuild tests.
- [x] (2026-11-09) Milestone 2: `validate_path` and prospective-write scheduling classify against the cached `PathGrants`; the credential denylist is checked before read-write and read-only grants. Built-in toolchain and system paths are readable to the in-process tools and remain write-rejected. Existing read-only and outside-working-directory diagnostics remain. A prospective write through a dangling symlink is rejected before the write can create its target. Added validator-level denial, synthetic toolchain-source, prospective-denied-write, and dangling-symlink tests.
- [ ] Milestone 3: add the acceptance and security tests, including synthetic HOME, denied symlinks, Linux-only paths, and the parity test.
- [ ] Milestone 4: update documentation and model-visible description snapshots.
- [ ] Milestone 5: run the focused tests, `just check`, documentation checks, and the platform verification available locally and in CI.
- [ ] Fill Acceptance Notes, Outcomes & Retrospective, move this plan to `docs/exec-plans/completed/`, and complete task 171 before the final commit.

## Surprises & Discoveries

- Observation: the task text says "Read (and Grep/Edit/Write through the same validator)", but the current tree has no `Grep` tool. The tool prompt says "There is no Glob, Grep, or LS tool" in `src/clients/tools/mod.rs`. The shared validator is currently used by `Read`, `ReadImage`, `Edit`, `Write`, Bash prospective-write scheduling, and Bash script evidence. This plan covers those surfaces and does not invent a Grep tool; a future tool that calls `validate_path` inherits the same grants and denylist.

- Observation: Bash may write built-in toolchain caches such as `~/.cargo`, `~/.rustup`, and `~/.npm`, but the task requires those same paths to be read-only to in-process `Edit` and `Write`. A single `read_write` vector shared verbatim by Bash and the file tools cannot express both requirements. The implementation therefore needs a file-tool projection and an OS-sandbox writable projection that are derived from one grant owner.

- Observation: `SandboxConfig::build_with_policy` reads `HOME`, `CARGO_HOME`, `RUSTUP_HOME`, `GOPATH`, `GOMODCACHE`, and `GOCACHE` on every call. Caching `PathGrants` at `ToolContext` construction changes the test pattern: tests that currently pin `HOME` around `SandboxConfig::build(&context)` must move context construction inside the pinned scope, or construct the grant value directly. This is an intentional improvement: a run must not change its filesystem grants when the process environment changes mid-run.

- Observation: the deny list must distinguish files from directories. A directory entry such as `~/.ssh` denies the whole subtree; a file entry such as `~/.npmrc` must deny only that file. Treating `~/.cargo/credentials.toml` as a directory subtree would also deny `~/.cargo/registry/src/...`, which is the primary file the task wants `Read` to reach. Conversely, matching a file entry with `starts_with` would deny unrelated siblings such as `~/.npmrc.backup`.

- Observation: `cargo test` can expose a `CARGO_HOME` override that points toolchain discovery at the real cargo home rather than the synthetic test HOME. Tests that need deterministic toolchain grants should use a path with no environment override (for example `~/.bun`) or pin every override explicitly.

- Observation: the deny-list canonical form must cover a path whose leaf does not exist yet. `DeniedPath` originally stored only `path.canonicalize()`, which fails for a missing `~/.npmrc`; with `HOME` under `/var` -> `/private/var`, the lexical forms differ and a prospective write bypassed the file deny entry. Deny entries and candidates now canonicalize the deepest existing ancestor and append missing components lexically.

- Observation: macOS `/private/var` is part of Bash's built-in read-and-execute set, so the in-process projection also treats arbitrary `/var/folders` temp paths as read-only. Tests that need a path outside every grant while using an explicit context with empty temp directories must place the outsider under `/tmp` or another ungranted root; tests using `ToolContext::from_current_process` still get the process temp directories as read-write grants.

- Observation: a dangling symlink is not an existing path. `std::fs::write` follows it and creates its target, so validating only the link spelling would let a working-directory link create a file under a denied or ungranted target. Prospective-write resolution now rejects any dangling symlink in the unresolved suffix before classification.

- Observation: `validate_path_with_dirs` and the `get_additional_dirs`/`get_skill_dirs`/`get_settings_dirs`/`get_temp_directories` accessors had no callers once the validator consumed `PathGrants`, so they were removed rather than kept as dead test-only paths.

## Decision Log

- Decision: create ADR 056 before implementation, with status `proposed` until it is accepted before code changes. The ADR records the shared grant model, the file-tool versus Bash projections, the credential-denylist precedence, the Linux OS-only exclusions, and the residual risk. Rationale: `docs/adr/README.md` requires an ADR for sandbox behavior, filesystem access, secrets, and security-sensitive changes, and this change alters the in-process read authority. Date/Author: 2026-11-09 / cake.

- Decision: one `PathGrants` value owns grant construction. It carries `read_write` (cwd, temp, settings writable paths, and linked-worktree git directories), `read_only` (built-in system paths, built-in toolchain paths, `--add-dir`, `[sandbox].read_only`, skill directories, and Cake state directories), `sandbox_writable` (the toolchain paths Bash may write under `WorkspaceWrite`), `sandbox_only_read` (OS-only paths excluded from file tools), `denied`, `user_grants`, and the policy it was built for. Rationale: one owner prevents the two enforcement layers from drifting again, while the extra projections preserve the intentional Bash-versus-file-tool asymmetry. Date/Author: 2026-11-08 / cake.

- Decision: `read_only` includes the built-in toolchain paths even under `WorkspaceWrite`, while `sandbox_writable` repeats the subset Bash may write. `SandboxConfig::from_grants` builds OS `writable` as `read_write + sandbox_writable` and OS `read_execute` as `read_only` minus `sandbox_writable`, plus `sandbox_only_read`. Rationale: `Read` accepts toolchain source; `Edit` and `Write` reject it as read-only; Bash keeps its current cache-write behavior. Date/Author: 2026-11-08 / cake.

- Decision: check `denied` before `read_write` and `read_only`, after canonicalizing the requested path. A denied directory denies its subtree; a denied file denies only that exact path. Store both lexical and canonical forms of every deny entry. Rationale: this makes a symlink from the working directory or from a granted tree to `~/.ssh/id_ed25519` fail closed, while leaving sibling files and toolchain source readable. Date/Author: 2026-11-08 / cake.

- Decision: the initial denylist is the task's reviewed set, not a complete credential inventory. It contains `~/.ssh`, `~/.aws`, `~/.azure`, `~/.config/gcloud`, `~/.kube`, `~/.docker/config.json`, `~/.config/gh`, `~/.config/glab-cli`, `~/.npmrc`, `~/.yarnrc`, `~/.yarnrc.yml`, `~/.cargo/credentials`, `~/.cargo/credentials.toml`, `~/.netrc`, `~/.git-credentials`, and `~/Library/Keychains`. General credential-file discovery and masking remain in 041b and 041c. A user-granted `~` can still expose a credential file outside this list; that residual risk is documented rather than silently claimed as covered. Date/Author: 2026-11-08 / cake.

- Decision: keep the existing read-only error wording for `validate_path_for_write` and prospective writes in this task. Add one new denial message for a credential path that names the path and the policy but never the contents. Task 068 owns centralizing the read-only wording; this task does not create a third read-only variant. Date/Author: 2026-11-08 / cake.

- Decision: keep the directory fields on `ToolContext` as construction inputs for compatibility with the existing test suite, but make `sandbox_policy` private with `sandbox_policy()` and `set_sandbox_policy()` so a policy change rebuilds the cached grants. Add `refresh_path_grants()` for test code that mutates `cwd`, `temp_dirs`, `additional_dirs`, `settings_dirs`, or `skill_dirs` after construction. Rationale: the production run never mutates those fields, and a full private-field refactor would enlarge the diff without improving the production invariant. Date/Author: 2026-11-08 / cake.

- Decision: canonicalize a deny entry or validation candidate by resolving its deepest existing ancestor and appending any missing components lexically. This keeps a not-yet-existing credential leaf such as `~/.npmrc` comparable across `HOME` symlink spellings without inventing the file or reading it. Date/Author: 2026-11-09 / cake.

- Decision: reject a prospective write whose unresolved suffix contains a dangling symlink. Rationale: the write would follow the link and create a target that the validator never classified. Fail closed rather than validate the link spelling. Date/Author: 2026-11-09 / cake.

- Decision: no new dependencies, no settings keys, no CLI flags, no session records, and no changes to `--add-dir`, `directories`, `[sandbox]` merge, tilde expansion, or project-trust semantics. Date/Author: 2026-11-08 / cake.

## Outcomes & Retrospective

Pending implementation. When the plan is complete, record the delivered files, the exact grant and denial behavior, the parity and platform test results, the Linux CI result, the documentation assessment, and any residual risk that remains outside the initial denylist.

## Context and Orientation

Cake is a Rust 2024 binary-only coding assistant. Model-generated Bash commands run under an OS filesystem sandbox: Seatbelt through `sandbox-exec` on macOS and Landlock on Linux. The sandbox is deny-by-default. Its ordinary path grants have two effective classes: read/write/execute and read/execute. Platform-only capabilities such as macOS Keychain service, SSH agent sockets, `lsopen`, and Apple Events are not ordinary path grants and stay in the platform translators.

The current grant builder is `SandboxConfig::build_with_policy` in `src/clients/tools/sandbox/mod.rs`. It seeds `writable` with the working directory, process temp directories, linked-worktree git directories, the built-in toolchain paths from `extend_with_toolchain_paths`, and the settings directories supplied by `ToolContext.settings_dirs`. It seeds `read_execute` with `get_read_execute_paths`, `ToolContext.additional_dirs` (from `--add-dir` and `[sandbox].read_only`), and `ToolContext.skill_dirs`. Under the `ReadOnly` policy, `partition_read_only` moves every non-temp writable path into `read_execute` and leaves only temp directories writable. `DangerFullAccess` builds the same configuration but skips applying it to Bash.

The in-process validator is in `src/clients/tools/mod.rs`. `validate_path` canonicalizes the requested path and calls `classify_resolved_path`. That classifier currently grants read/write access to `cwd`, temp directories, and settings directories, read-only access to `additional_dirs` and `skill_dirs`, and nothing else. `validate_path_for_write` rejects read-only grants. `resolve_nonexistent_path` applies the same classifier to the deepest existing parent for prospective writes. `Read` and `ReadImage` call `validate_path_in_cwd`; `Edit` and `Write` call `validate_path_for_write` or `resolve_path_for_write_scheduling`; Bash calls the validator for `cwd` admission, prospective scheduling, and script evidence.

`ToolContext` is constructed in `src/main.rs` after settings, trust, skill discovery, and writable-grant preparation. It currently carries `cwd`, `temp_dirs`, `additional_dirs`, `skill_dirs`, `settings_dirs`, `sandbox_policy`, gates, limits, and the Bash session registry. The sandbox configuration is rebuilt from those fields on demand. `SandboxConfig` also carries `user_grants`, captured before read-only demotion, for Bash `cwd` admission.

The model-visible surfaces that can change are the tool result text in `src/clients/tools/mod.rs`, the ReadImage description in `src/clients/tools/read-image-description.txt`, and the provider request snapshots that embed tool descriptions. `Read`, `Edit`, and `Write` do not currently describe their path grants, so they need no wording change unless the denial text requires one.

Task 171 is the authority for the desired behavior. `docs/security.md` is the authority for the trust boundary, `docs/configuration.md` owns user-facing settings, and `ARCHITECTURE.md` owns the short architecture statement. ADR 019 owns project-customizable sandbox paths, ADR 023 owns the two ordinary path classes, ADR 032 owns Bash `cwd` admission, and ADR 053 owns project-config trust. This task does not change those user-grant or trust decisions.

The bypass classes this security-sensitive change must defend against are:

- a denied credential file becoming readable because it is nested inside a user-granted ancestor or a built-in toolchain grant;
- a symlink from the working directory or a granted tree to a denied path bypassing the lexical check;
- a file deny entry accidentally becoming a subtree grant, or a directory deny entry failing to cover its children;
- a toolchain path becoming writable through `Edit` or `Write` merely because Bash may write it;
- `/proc/self/environ`, `/dev/fd`, or `/sys` becoming readable through the in-process grant projection on Linux;
- the deny list being bypassed by prospective-write scheduling;
- a provider request or session JSONL receiving the contents of a denied file through an error message;
- a policy change after `ToolContext` construction leaving stale grants;
- an OS-sandbox-only path leaking into the in-process set, or an in-process read grant disappearing from the OS set after the projection split.

## Plan of Work

Before code changes, accept ADR 056. It describes the current Bash-versus-file-tool gap, the single `PathGrants` owner, the file-tool and OS-sandbox projections, the credential denylist precedence, the Linux OS-only exclusions, and the residual risks. If acceptance changes the decision, update the ADR and this plan before editing Rust files; do not create a second ADR for the same boundary.

Milestone 1 adds `PathGrants` and moves grant construction. In `src/clients/tools/sandbox/mod.rs`, define `PathGrants` and `DeniedPath` with the fields named in the Decision Log. Move the path computation currently in `SandboxConfig::build_with_policy` into `PathGrants::build`. The builder takes the policy, working directory, temp directories, `--add-dir` directories, settings directories, skill directories, and an optional home directory. It preserves the existing environment overrides for `CARGO_HOME`, `RUSTUP_HOME`, `GOPATH`, `GOMODCACHE`, and `GOCACHE`. It preserves existing behavior for symlinked and canonical path forms and for missing paths. `PathGrants::build` computes the file-tool `read_write` and `read_only` sets, the `sandbox_writable` toolchain subset, `sandbox_only_read` (Linux `/proc`, `/dev`, `/sys`), `denied`, `user_grants`, and the policy.

Then make `SandboxConfig` a projection. Keep its existing `writable`, `read_execute`, `policy`, and `user_grants` fields and its existing `is_path_allowed`, `is_path_writable`, and `is_path_selectable` predicates. `SandboxConfig::from_grants` clones `read_write` into `writable`, clones `read_only` into `read_execute` while removing entries present in `sandbox_writable`, appends `sandbox_only_read`, and copies `user_grants` and `policy`. `SandboxConfig::build(&context)` consumes `context.path_grants`; `SandboxConfig::build_with_policy(...)` builds a `PathGrants` and projects it. Do not add deny logic to the OS predicates: Bash's denial diagnostics must continue to describe what the OS sandbox allows, while the in-process validator applies the denylist.

Add `path_grants: PathGrants` to `ToolContext`. Build it once in the private constructor used by both `ToolContext::new` and `ToolContext::with_temp_dirs`, after all directory inputs are known. Add `refresh_path_grants()` for tests that mutate path inputs after construction. Make `sandbox_policy` private, add `sandbox_policy()` and `set_sandbox_policy()`, and update the production reads in `src/clients/agent.rs`, `src/clients/tools/bash.rs`, and `src/cli/session_factory.rs`. Update test helpers that assign the policy or mutate path fields in `src/clients/tools/bash_tests.rs`, `src/clients/tools/bash.rs`, `src/clients/tools/script_evidence_tests.rs`, `src/clients/tools/sandbox/grants_tests.rs`, and `src/clients/agent/agent_tests.rs` to use the setter or refresh helper. Move context construction inside `temp_env::with_var("HOME", ...)` scopes where a test currently pins HOME only around `SandboxConfig::build`.

Milestone 2 wires the validator. Replace `validate_path_with_dirs` with a grant-aware `validate_path_with_grants(path_str, grants)`. `validate_path` canonicalizes and calls it with `context.path_grants`. `classify_resolved_path` takes the canonical path and `&PathGrants`; it returns a denial result first, then `ReadWrite`, then `ReadOnly`, then no grant. Add a `Denied` variant to `PathAccess` or an equivalent classifier result so `validate_path_with_grants` can return the credential-denial message before constructing a `ValidatedPath`. Keep `ValidatedPath` limited to allowed paths. Update `resolve_nonexistent_path` to use the same classifier and to reject a denied existing parent before the read-only branch. Keep the existing read-only and outside-working-directory messages for their existing cases. Add one helper for the credential-denial message so it has one owner.

Keep `validate_path_with_dirs` only as a `#[cfg(test)]` wrapper if that reduces test churn. The wrapper may build `PathGrants` with the default policy and no home directory; production paths must never call it. Update the existing classifier and prospective-write tests to use `PathGrants` directly where the test needs built-in or denied paths.

Milestone 3 adds the security tests. Use synthetic home directories created by `tempfile` and pin `HOME` with `temp_env` while constructing `ToolContext`. Do not read the real user's credential files. Add a test that places a Rust source file under `$HOME/.cargo/registry/src/...` and another under `$HOME/.rustup/...`, then proves `Read` returns the contents without `--add-dir` and `validate_path_for_write` rejects the same path as read-only. Add a test that `Write` and `Edit` leave the toolchain file byte-identical after the rejection. Add the deny tests for `~/.npmrc`, `~/.cargo/credentials.toml`, `~/.config/gh/hosts.yml`, `~/.ssh/id_ed25519`, and `~/.netrc` with synthetic contents; grant `$HOME` as a settings directory so the test proves the deny list wins over an explicit ancestor grant; assert the error names the path but never contains the synthetic secret. Add a symlink from the working directory and from a granted tree to a denied file and assert both fail. Add a symlink to a
path outside every grant and assert it still fails as outside. Add a test for a prospective write under a denied directory. Add a test that a file entry does not deny a sibling (for example `~/.npmrc.backup` stays readable when its parent is granted) and that a directory entry denies a nested file.

Add the parity tests in `src/clients/tools/sandbox/mod.rs` and `src/clients/tools/sandbox/grants_tests.rs`. The basic cache and policy-rebuild tests landed with Milestone 1. The parity test enumerates the in-process `read_write` and `read_only` grants, skips denied paths and the OS-only projection, and asserts `validate_path` accepts each existing path. The projection test asserts `SandboxConfig::build(&context).writable` equals `read_write + sandbox_writable` and its `read_execute` equals `read_only` without `sandbox_writable`, plus `sandbox_only_read`. Add a `ReadOnly` policy test proving a settings directory is demoted for the in-process view, temp stays writable, and `Edit`/`Write` remain unregistered. Add Linux-only tests for `/proc/self/environ`, `/dev/fd/0`, and a `/sys` path; keep the existing non-regular-file rejection test for `/dev` on macOS.

Milestone 4 updates documentation and snapshots. Update `docs/security.md` with the shared-grant parity rule, the file-tool read-only projection, the credential denylist and its precedence, the Linux exclusions, and the residual risk that Bash still reads denied paths and the list is not exhaustive. Update `docs/configuration.md` so `[sandbox]`, `directories`, `--add-dir`, built-in toolchain paths, and the deny list all describe the same behavior for Bash and the in-process tools. Update `ARCHITECTURE.md` to name `PathGrants` as the owner of both projections. Update the module and function doc comments in `src/clients/tools/mod.rs` and the `SandboxSettings` comment in `src/config/settings.rs` so they no longer say the validator knows only the working directory and `--add-dir`. Update `src/clients/tools/read-image-description.txt` because it currently says ReadImage accepts only the working directory, temp, or `--add-dir`. Do not change `Read`, `Edit`, or `Write` descriptions unless
their denial text is model-visible and inaccurate. Regenerate the two provider request snapshots after the description change and inspect that only the intended lines moved.

Milestone 5 runs the focused and full gates. Run the focused sandbox, tool, read, edit, write, and script-evidence tests; `cargo fmt -- --check`; strict Clippy; `just cc-check`; `just snapshots`; `just docs-check`; and `just check`. On Linux, run the Linux-only tests if a compatible environment is available; otherwise confirm that the existing Linux CI job selects the new `clients::tools::sandbox` tests and the in-process grant tests. The macOS tests run locally with the real Seatbelt path where the process allows it; record any nested-Seatbelt skip accurately. No paid provider calls are part of this plan.

## Concrete Steps

All commands run from `/Users/travisennis/Projects/cake/cake` unless stated otherwise.

ADR 056 is drafted as proposed. Review it, accept it with `ahm adr accept 056`, and reference it from task 171 before editing Rust files.

After Milestone 1, run the focused sandbox tests and the complexity gate:

```
cargo test --bin cake clients::tools::sandbox
just cc-check
```

After Milestone 2 and the core tests, run:

```
cargo test --bin cake clients::tools::read
cargo test --bin cake clients::tools::edit
cargo test --bin cake clients::tools::write
cargo test --bin cake clients::tools::script_evidence
cargo test --bin cake clients::tools
```

After Milestone 4, regenerate and verify the snapshots and documentation:

```
INSTA_UPDATE=always cargo test --bin cake request_full_with_agents_and_skills
just snapshots
just docs-check
```

Before handoff, run the full local gate:

```
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
just check
```

A successful focused run ends with a line of the form:

```
test result: ok. N passed; 0 failed; 0 ignored; 0 measured; M filtered out
```

A successful complexity gate ends with a pass line, not an exceedance:

```
CC gate: N functions checked, M new, 0 over allowed
PASS: No cyclomatic complexity exceedances
```

The snapshot diff after the ReadImage description change must contain only the description lines in the two provider request snapshots. If any other snapshot changes, stop and inspect it rather than accepting it.

## Validation and Acceptance

The change is accepted only when all of the following hold.

A synthetic `$HOME/.cargo/registry/src/.../lib.rs` and a synthetic `$HOME/.rustup/toolchains/.../lib.rs` are readable through `Read` without `--add-dir`, and the same paths are rejected as read-only by `validate_path_for_write`. A synthetic toolchain file remains byte-identical after attempted `Edit` and `Write` calls. The test uses a `tempfile` home and does not touch the real user's toolchain.

The initial deny list is effective even when `$HOME` is granted as a settings directory: `~/.npmrc`, `~/.cargo/credentials.toml`, `~/.config/gh/hosts.yml`, `~/.ssh/id_ed25519`, and `~/.netrc` are rejected by `Read` with a message that names the path and the credential policy but does not contain the synthetic secret. A symlink from the working directory or from a granted directory to a denied path is rejected after canonicalization. A symlink to a path outside every grant is rejected as outside. A prospective `Write` under a denied directory is rejected before the file is created.

A directory deny entry covers a nested file; a file deny entry does not cover a sibling such as `~/.npmrc.backup`. This proves the file-versus-directory matching rule is not implemented as a blanket `starts_with` over file paths.

The parity test enumerates every existing in-process grant from `PathGrants`, excluding denied paths and the OS-only projection, and `validate_path` accepts each one. `SandboxConfig::build(&context)` exposes the same vectors projected as OS writable and read/execute grants. A HOME change after context construction does not change the context's grant vectors, proving the grants are cached once per invocation.

Under `--sandbox read-only`, a settings directory previously granted writable is rejected by `validate_path_for_write`, temp directories remain writable in the grant table, and `Edit` and `Write` remain absent from the registry. Under `workspace-write`, a settings directory remains writable and a built-in toolchain path is read-only to the file tools. `--add-dir`, `directories`, `[sandbox].read_only`, `[sandbox].writable`, skill directories, Cake state directories, tilde expansion, and project-trust behavior are unchanged; the existing settings, trust, and sandbox tests pass.

On Linux, `/proc/self/environ`, `/dev/fd/0`, and a `/sys` path remain rejected by the in-process validator even though the OS sandbox grants those paths. On macOS, `/dev/null` still reaches the existing non-regular-file rejection rather than becoming a readable regular file. The denial text never echoes file contents, and a denied path is rejected before `Read` opens it.

`just check` passes, including formatting, strict Clippy, the full unit and integration suites, the complexity gate, the script gates, and the routed documentation gate. `just docs-check` passes. The macOS `Test` CI job and the Linux `Test`/Landlock job pass after the branch is pushed; any local nested-Seatbelt skip is recorded rather than hidden.

## Idempotence and Recovery

Every source and documentation edit is safe to repeat after rereading the file. Focused tests and the full gate only write build artifacts and temp fixtures. `cargo insta` accepts snapshots idempotently, but the diff must be inspected first. Synthetic HOME fixtures clean themselves up.

If a test fails because a `ToolContext` was mutated after construction, call `refresh_path_grants()` or move construction inside the `temp_env::with_var` scope. Do not weaken the cached-grant invariant to make a test pass. If a toolchain path becomes writable to `Edit` or `Write`, check the projection split before changing the test: `read_only` and `sandbox_writable` must both contain the toolchain path, and `validate_path_for_write` must see the read-only classification.

If `just cc-check` reports an exceedance, move branches into small helpers or split the classifier; do not regenerate `ci/cargo-crap-baseline.json` for this change. If an unrelated snapshot changes, restore it and inspect the tool description path. If implementation changes the decision, update ADR 056 and this plan before continuing rather than silently diverging from the accepted boundary.

The deny list is a security boundary in the in-process projection only. Do not test it against real credential files, do not print real values in failures, and do not add a fallback that treats a deny-list read failure as an empty file. Bash remains subject to the OS sandbox and the external gate; this task must not claim the denylist is a filesystem boundary.

## Artifacts and Notes

The planned focused verification is:

```
cargo test --bin cake clients::tools::sandbox
cargo test --bin cake clients::tools::read
cargo test --bin cake clients::tools::edit
cargo test --bin cake clients::tools::write
cargo test --bin cake clients::tools::script_evidence
cargo test --bin cake clients::tools
```

The planned repository verification is:

```
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
just cc-check
just snapshots
just docs-check
just check
```

Expected observable behavior, not just test names:

```
Read $HOME/.cargo/registry/src/.../lib.rs
-> File: .../lib.rs
-> the requested source lines

Edit the same path
-> the existing read-only error
-> the file bytes are unchanged

Read $HOME/.ssh/id_ed25519 after granting $HOME
-> a denial naming the path and the credential policy
-> the error does not contain the file contents

Read /proc/self/environ on Linux
-> outside the in-process grants, not the file contents
```

No paid provider calls are required. The plan intentionally uses synthetic files and temp directories, never the operator's real credentials.

## Interfaces and Dependencies

New and changed internal interfaces, all private to the crate:

- `crate::clients::tools::sandbox::PathGrants` with `read_write`, `read_only`, `sandbox_writable`, `sandbox_only_read`, `denied`, `user_grants`, and `policy`. It is the single owner of grant construction and is cached on `ToolContext`.
- `PathGrants::build(policy, cwd, temp_dirs, additional_dirs, settings_dirs, skill_dirs, home) -> PathGrants` and its test-facing helpers. It replaces the path computation currently inside `SandboxConfig::build_with_policy`.
- `DeniedPath` and `DenyKind::{File, Directory}`, with lexical and canonical forms. `PathGrants::is_denied(&Path) -> bool` checks file equality or directory subtree membership.
- `SandboxConfig::from_grants(&PathGrants) -> SandboxConfig` and the existing `SandboxConfig::build`/`build_with_policy` signatures. `SandboxConfig` continues to expose `writable`, `read_execute`, `policy`, `user_grants`, and the existing predicates.
- `ToolContext::path_grants`, `ToolContext::refresh_path_grants(&mut self)`, `ToolContext::sandbox_policy(&self)`, and `ToolContext::set_sandbox_policy(&mut self, SandboxPolicy)`. The policy field becomes private; path fields remain construction inputs with a documented refresh requirement.
- `validate_path_with_grants(path_str, &PathGrants) -> Result<ValidatedPath, String>` and the rewritten `classify_resolved_path(&Path, &PathGrants)`. The existing `validate_path`, `validate_path_in_cwd`, `validate_path_for_write`, `resolve_path_for_write_scheduling`, and `resolve_path_for_read_scheduling` keep their public shapes.
- ADR 056, `docs/adr/056-share-bash-read-grants-with-in-process-file-tools.md`, records the decision. No new Cargo dependency, settings key, CLI flag, session record, provider schema, or platform sandbox API is introduced.

The plan assumes the current tree's `Read`, `ReadImage`, `Edit`, `Write`, and Bash script-evidence surfaces. There is no Grep tool to update. A future Grep implementation that calls the shared validator must use the same `PathGrants` and denylist rather than creating a second grant table.
