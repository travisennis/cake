# Admit Sandbox-Granted Directories As Bash Working Directories

This ExecPlan is a living document, maintained per `docs/workflow/exec-plans.md`. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must stay current while issue #588 is implemented.

## Purpose / Big Picture

Cake's Bash tool accepts an optional per-call `cwd` that selects the directory a command starts in. Today that directory must be inside the invocation working directory, so a project the user already granted with `--add-dir`, a `[sandbox]` settings entry, or a skill directory cannot be selected, and an agent that needs to work in one falls back to writing `cd` into the command text. After this change, an agent can start a command in any directory the sandbox already grants, and under `--sandbox danger-full-access` in any existing directory, while a directory outside every grant is still rejected before the command-safety judge runs and before any process is spawned.

A human can see the change working by granting a directory and asking Cake to run a command there. With `cake --add-dir /tmp/project`, a Bash call carrying `cwd: "/tmp/project"` and the command `pwd -P` prints `/tmp/project` and exits 0, and the same call with the path replaced by a directory that was never granted fails with a tool error naming what is allowed and never spawns a process. The omission case is unchanged: a Bash call without `cwd` runs in the invocation working directory.

The decision behind this behavior is recorded in `docs/adr/032-bash-cwd-sandbox-grants.md`, which partially supersedes ADR 031's workspace-only admission rule. This plan implements that decision; it does not restate its reasoning, only the required behavior.

## Progress

- [x] (2026-09-17) Inspected the Bash tool, the sandbox grant table, the cwd tests, and the baseline complexity data.
- [x] (2026-09-17) Recorded the decision in ADR 032 and annotated ADR 031 with a partial-supersession note.
- [x] (2026-09-18) Carried the user grant set on `SandboxConfig` and added the `is_path_selectable` predicate, with focused unit tests (Milestone 1).
- [x] (2026-09-18) Admitted granted directories in `resolve_bash_cwd` and built one `SandboxConfig` per call (Milestone 2).
- [x] (2026-09-18) Updated the focused tests, including the fixtures that relied on temp directories being ungranted.
- [x] (2026-09-18) Updated `bash-description.txt`, the `cwd` schema text, `docs/security.md`, `docs/integrations.md`, and the two provider request snapshots (Milestone 3).
- [x] (2026-09-18) Ran the focused tests, `just cc-check`, `cargo insta test --accept`, `just docs-fmt`, `just docs-check`, `just fmt-check`, `just clippy`, and `just check`.
- [x] (2026-09-18) Recorded the platform expectation: the sandboxed runs are proved by the macOS `Test` CI job; the Linux jobs select `clients::tools::sandbox`, which now includes the new predicate tests, and do not select tests in the `bash` module.
- [x] (2026-09-18) Filled Outcomes & Retrospective and moved this plan to `docs/exec-plans/completed/` before opening the pull request.

## Surprises & Discoveries

- Observation: the focused Bash test helper `execute_bash_with_judge_in` in `src/clients/tools/bash.rs` forces `context.sandbox_policy = SandboxPolicy::DangerFullAccess` and sets no user grant directories. Evidence: the helper body assigns that policy after `ToolContext::from_current_process()`. Consequence: under the accepted decision, every one of its calls admits any existing directory, so the two tests that assert an outside directory is rejected cannot demonstrate anything through this helper and must be rerun under `WorkspaceWrite`. Resolved: the missing-path and file-path cases stay with the helper (canonicalization rejects them under every policy), the ungranted-directory case moved to a `WorkspaceWrite` context built by `admission_context_at`, and the symlink case moved with it.

- Observation: `path_outside_cwd_for_sandbox_test()` in `src/clients/tools/bash_tests.rs` returns the account home directory, which is an ancestor of granted toolchain caches but is not itself a grant. Evidence: `SandboxConfig::build_with_policy` adds `home.join(".cargo")` and similar cache directories, not `home`. Consequence: it remains a valid ungranted fixture for admission tests, unlike a `tempfile` directory, which is a writable temp grant. This held.

- Observation: `ci/cargo-crap-baseline.json` was generated on 2026-09-14 (commit `faa1b44`), before ADR 031's implementation landed, so `resolve_bash_cwd`, `parse_bash_call`, and `SandboxConfig::is_path_writable` are absent from it and are therefore not ratcheted, while `SandboxConfig::is_path_allowed` is present at CC 1. Consequence: new branches belong in `resolve_bash_cwd` and in a new predicate; growing `is_path_allowed` would fail `just cc-check`. This held: `just cc-check` reported 1097 functions checked, 43 new, 0 over allowed.

- Observation: `ToolContext::from_current_process` treats the process `TMPDIR` as a writable grant, so every `tempfile` fixture created by an admission test is already admitted by the temp grant and cannot show whether a settings or `--add-dir` grant was what admitted it. Evidence: the first draft of the positive admission test passed with the grant removed. Consequence: admission tests build their context through `admission_context_at`, which uses `ToolContext::with_temp_dirs` with an empty temp list, so a fixture directory is selectable only because the test granted it.

- Observation: creating a fixture directory under the account home (`tempfile::TempDir::new_in(&outside)`) is refused with `Operation not permitted` when the test process is itself inside an enforcing sandbox, which is the normal state when a Cake session runs `just test`. Evidence: a draft of `bash_cwd_runs_in_a_settings_directory_grant` without the `skip_if_sandbox_unavailable()` guard panicked at `should create test workspace: PermissionDenied`. Consequence: the two tests that actually run a command in a granted directory keep the `skip_if_sandbox_unavailable()` guard, which also guards fixture creation; local runs skip them, and the macOS `Test` CI job is where they execute for real. `SandboxConfig` literals in `src/clients/tools/sandbox/macos.rs` and `src/clients/tools/sandbox/linux.rs` gained `user_grants: Vec::new()`, and the two renamed Bash tests are `bash_cwd_rejects_missing_path_and_file_path_before_spawn` and `bash_cwd_rejects_symlink_that_escapes_every_grant`.

## Decision Log

- Decision: admit a `cwd` when the effective policy is `DangerFullAccess`, when the canonical path is inside the canonical invocation working directory, or when `SandboxConfig` reports it inside a read-write grant or a user read-only grant. Rationale: this is option 4 plus option 2 from issue #588's review, and it is what ADR 032 records. Date/Author: 2026-09-17 / Travis Ennis.

- Decision: the built-in read-and-execute set from `get_read_execute_paths()` is not an admission source, so `/etc` and `/usr` remain unselectable. Rationale: it is a diagnostic predicate coarse enough to admit paths the OS denies, and no user grants those paths for work; `DangerFullAccess` covers the arbitrary-directory case. Date/Author: 2026-09-17 / Travis Ennis.

- Decision: capture the user grant set on `SandboxConfig` before `partition_read_only` runs. Rationale: under the `ReadOnly` policy that function demotes settings directories from `writable` into `read_execute`, so deriving the grant set from the final list would hide them. Date/Author: 2026-09-17 / Travis Ennis.

- Decision: build `SandboxConfig` once in `parse_bash_call` and pass it to `prepare_bash_command` and `sandbox_denials`. Rationale: issue #588 asks for it, and admission must read the same config the denial diagnostics read. Date/Author: 2026-09-17 / Travis Ennis.

- Decision: the rejection message names the boundary and the grant classes rather than enumerating every grant. Rationale: skill directories can be numerous, and the classes are what the model needs to choose a different directory. Date/Author: 2026-09-17 / Travis Ennis.

- Decision: read the policy from `context.sandbox_policy` inside `resolve_bash_cwd` rather than adding a separate policy parameter, and drop the now-unused `context` parameter from `prepare_bash_command` and `sandbox_denials`. Rationale: the policy is already on the context, and the two functions reach the config through the explicit parameter instead. Date/Author: 2026-09-18 / Travis Ennis.

- Decision: keep `user_grants` populated with only directories that exist, matching `push_dirs_with_canonical`, and store both the lexical and canonical form. Rationale: admission canonicalizes the candidate anyway, and matching the existing grant-push practice keeps the two path classes comparable. Date/Author: 2026-09-18 / Travis Ennis.

## Outcomes & Retrospective

Delivered, in the terms of the acceptance criteria:

1. A `cwd` inside a read-write grant or a user read-only grant is admitted and canonicalized. `SandboxConfig` now carries `user_grants` (settings directories, `--add-dir` directories, and skill directories in lexical and canonical form) captured before `partition_read_only`, and `is_path_selectable` returns true for a read-write grant or a user grant. Tests: `bash_cwd_admits_settings_add_dir_and_skill_directory_grants`, `bash_cwd_admits_a_settings_directory_grant_under_read_only`, and the sandbox-module tests `is_path_selectable_admits_read_write_and_user_grants_only` and `is_path_selectable_survives_the_read_only_demotion`.
2. A `cwd` outside the workspace and every grant fails before judge evaluation and before spawn. `bash_cwd_rejects_a_directory_outside_every_grant_before_spawn` drives the same call through a context that does grant the path (so the fixture is admissible when granted) and one that does not, asserts the tool error names the workspace boundary and the `--add-dir`, `[sandbox]`, and skill-directory grant classes, and asserts the marker file the command would have created does not exist. `bash_cwd_rejects_built_in_system_paths` covers `/etc` and `/usr`.
3. The symlink guard holds. `bash_cwd_rejects_symlink_that_escapes_every_grant` shows a link whose canonical target is outside every grant is rejected, and `bash_cwd_accepts_a_symlink_whose_target_is_granted` shows a link into a grant is accepted, both because canonicalization runs before the grant check.
4. Nothing became writable that was not writable before. No sandbox profile or platform adapter changed: the only edit under `src/clients/tools/sandbox/` outside `mod.rs` is `user_grants: Vec::new()` on six test-only `SandboxConfig` literals in `macos.rs`. `bash_cwd_selects_an_add_dir_grant_under_read_only_without_widening_authority` asserts the `read-only` policy still denies a write inside the directory it selected as `cwd`.
5. The models are recorded in ADR 032, including the `DangerFullAccess` and read-only-grant decisions. `bash_cwd_admits_any_existing_directory_under_danger_full_access` pins the first.
6. The child, the judge request, the repository digest, and the denial diagnostics still take one canonical path: `resolve_bash_cwd` returns it once, `execute_bash_with_args` passes that one value to `prepare_bash_command`, `bash_judge_preflight`, and `sandbox_denials`, and `bash_cwd_runs_in_a_settings_directory_grant` asserts it on both the child's `pwd -P` output and the recorded judge request's `cwd` field.
7. One `SandboxConfig` is built per Bash call, in `parse_bash_call`; `prepare_bash_command` and `sandbox_denials` now take it as a parameter instead of building their own.

Measured: `cargo test --all-features` reports 1523 unit tests plus every integration suite passing with 0 failures; `just cc-check` reports 1097 functions checked, 43 new, 0 over allowed; `cargo fmt -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `just docs-fmt`, `just docs-check`, and `just check` all pass; `cargo insta test --accept` accepted exactly the two intended provider snapshots.

Remaining before the pull request: the macOS `Test` CI job is the only place the two command-running tests execute against real Seatbelt enforcement, because a Cake session cannot apply a nested Seatbelt profile (ADR 016). The Linux expectation, to confirm from the `Linux Test` job, is that a directory admitted only by the coarse predicate still starts the process because Landlock has no access right governing `chdir`, while macOS denies the read at the first file access. The `Linux Test` job selects `clients::tools::sandbox`, so it runs the two new predicate tests, and it does not select tests in the `bash` module.

Retrospective: the three implementation surprises were all about test fixtures rather than the admission rule, which is the shape the ADR predicted --- the risk was in how "granted" is decided, and the decision kept the grant table as the only source. The one thing worth changing next time is to check where a test process actually runs its fixtures before writing a fixture plan: `TMPDIR` being a grant, and the account home being unwritable from inside an enforcing sandbox, both invalidated the plan's first fixture sketches.

## Context and Orientation

The Bash tool definition, argument parsing, execution, judge preflight, and sandbox-denial diagnostics all live in `src/clients/tools/bash.rs`. `parse_bash_call` parses the model's JSON arguments and calls `resolve_bash_cwd` to turn the optional `cwd` string into the effective working directory for the call. The effective directory then travels as an explicit argument to `execute_bash_with_args`, which uses it for the child process, the command-safety judge request (`JudgeRequest::new`), the repository digest (`repo_state_digest`), and `sandbox_denials`. A Bash call's `cwd` was already resolved and validated once, before the judge, by ADR 031.

`src/clients/tools/sandbox/mod.rs` builds the sandbox grant table. `SandboxConfig` holds `writable` (read, write, execute) and `read_execute` (read and execute, no write), plus the `policy` it was built for. `build_with_policy` seeds `writable` with the invocation directory, the process temp directories, linked-worktree git directories, HOME toolchain caches, and the settings directories supplied by `ToolContext.settings_dirs`. It seeds `read_execute` with `get_read_execute_paths()` (the built-in system paths), then `ToolContext.additional_dirs` (from `--add-dir` and `[sandbox]` read-only entries) and `ToolContext.skill_dirs`, both read-only. For the `ReadOnly` policy, `partition_read_only` moves everything except temp directories from `writable` into `read_execute`. `DangerFullAccess` builds the same config but `prepare_bash_command` never applies it.

`SandboxConfig` exposes two predicates today. `is_path_allowed` reports membership in `writable` or `read_execute`, and is the coarse convenience predicate the denial scan uses for reads and executions. `is_path_writable` reports membership in `writable` only, and is what the denial scan uses for writes. Both canonicalize the candidate and compare it, and its canonical form, against each grant with `Path::starts_with`. Both are documented as a best-effort hint for naming a missing grant rather than an exact model of Seatbelt or Landlock enforcement.

`src/clients/tools/bash_tests.rs` holds the focused tests. `path_outside_cwd_for_sandbox_test()` returns an ungranted stand-in path (the account home directory, or `None` when it sits inside the checkout). `context_with_policy_at` builds a `ToolContext` with an explicit workspace and policy; `execute_bash_with_judge_in` runs an entire call in an explicit fixture directory but always under `DangerFullAccess`. Sandbox tests are gated `#[cfg(any(target_os = "macos", target_os = "linux"))]` and call `skip_if_sandbox_unavailable()` first. The macOS `Test` CI job runs the whole suite with real Seatbelt enforcement; the Linux jobs select tests by name substring, and tests in the `bash` module are not selected there, which is why platform behavior for this change must be verified on macOS.

`src/clients/tools/bash-description.txt` is embedded verbatim in the provider request snapshots `chat_request_full_with_agents_and_skills` and `responses_request_full_with_agents_and_skills`, so editing it changes the description file plus one line in each of those two snapshots. `docs/security.md` and `docs/integrations.md` state the current user-visible rule.

Complexity gates matter here. `just cc-check` compares every function against `ci/cargo-crap-baseline.json`: a function in the baseline may not exceed its recorded cyclomatic complexity, and a function absent from it may not exceed the CC 10 target. Nothing in this plan may add a `?` or a branch to a baseline function.

## Milestones

Milestone 1, the grant set on the config. At the end of this milestone `SandboxConfig` can answer, for an arbitrary canonical path, whether the user granted it, and the answer is unaffected by the read-only policy's demotion of settings directories. Work happens in `src/clients/tools/sandbox/mod.rs`: add a private `user_grants` field holding the canonicalized `settings_dirs`, `additional_dirs`, and `skill_dirs` (both lexical and canonical forms, matching the existing `push_dirs_with_canonical` practice), populate it in `build_with_policy` from the pre-partition inputs, and add a `pub(super) fn is_path_selectable(&self, path: &Path) -> bool` that returns true for a read-write grant or a user grant. Verify with focused unit tests in the same file's test module: a settings directory is selectable when the policy is `ReadOnly`, `/etc` and `/usr` are not selectable, a `tempfile` directory is selectable because it is a temp grant, and the account home directory is not selectable. Run
`cargo test clients::tools::sandbox` and `just cc-check`; the new predicate must be at or below CC 10 and no baseline function may move.

Milestone 2, admission in the working-directory resolver. At the end of this milestone an accepted directory produces a canonical working directory used by every downstream surface, a rejected one produces a tool error before the judge and before spawn, and only one `SandboxConfig` is built per call. Work happens in `src/clients/tools/bash.rs`: `resolve_bash_cwd` gains a `policy: SandboxPolicy` and a `config: &SandboxConfig` parameter and admits the canonical candidate when the policy is `DangerFullAccess`, when it starts with the canonical invocation directory, or when `is_path_selectable` returns true, rejecting otherwise with a message naming the invocation workspace and the grant classes. `parse_bash_call` builds the config once and returns it alongside the parsed arguments and the effective directory, and `execute_bash_with_args` forwards it to `prepare_bash_command` (which stops calling `SandboxConfig::build`) and to `sandbox_denials` (which also stops building its own). Verify
with the focused admission tests described under Validation and Acceptance. Run `cargo test clients::tools::bash` and `just cc-check`.

Milestone 3, model-visible and contract documentation. At the end of this milestone every prose surface states the new rule. Update the `cwd` description in `bash_tool`'s schema and the first line of `src/clients/tools/bash-description.txt`, the sandbox paragraph in `docs/security.md`, and the Bash `cwd` paragraph in `docs/integrations.md`, then regenerate the two affected snapshots. Run `cargo insta test --accept`, then `just docs-fmt` and `just docs-check`.

Milestone 4, platform verification and the full gate. At the end of this milestone the sandboxed behavior is demonstrated on macOS and the Linux expectation is recorded. Run `just fmt-check`, `just clippy`, `just test`, and `just check` from `/Users/travisennis/Projects/cake/cake-1`, then confirm the macOS `Test` CI job passes after the branch is pushed and note in the issue whether the Linux jobs select the new tests.

## Plan of Work

Start in `src/clients/tools/sandbox/mod.rs`. Add the `user_grants` field to `SandboxConfig` with a doc comment that states it exists for working-directory admission and that the platform profiles never read it. Populate it in `build_with_policy` next to the existing `push_dirs_with_canonical` calls and before the `partition_read_only` call, so the read-only policy's reclassification cannot hide a settings directory. Add `is_path_selectable` beside `is_path_writable`, reusing the same canonicalization and `starts_with` comparison, with a doc comment recording that it is a best-effort predicate that gates admission as well as diagnostics, and that the OS still governs access. Do not modify `is_path_allowed`.

Then in `src/clients/tools/bash.rs`, change `resolve_bash_cwd` to accept the resolved policy and a borrowed `SandboxConfig`, replace the workspace-only rejection with the disjunction from Milestone 2, and rewrite the rejection message. Keep the existing canonicalization error messages for a missing or inaccessible path and for a non-directory path, since they describe conditions the operating system reports and they are already asserted by tests. Change `parse_bash_call` to build the config once and return it; change `execute_bash_with_args` to take it and forward it; delete the two `SandboxConfig::build` calls that `prepare_bash_command` and `sandbox_denials` currently make. Update the three direct `execute_bash_with_args` call sites in `src/clients/tools/bash_tests.rs` and the one indirect call site in bash.rs's test helper.

Then update the tests. In `src/clients/tools/bash_tests.rs`, rework `bash_cwd_rejects_missing_file_and_outside_paths_before_spawn` so that its remaining cases are a missing path and a file path, both still valid under `DangerFullAccess`, and move the outside-grant case to a new test that runs through a `WorkspaceWrite` context and uses `path_outside_cwd_for_sandbox_test()` as the fixture. Rework `bash_cwd_rejects_symlink_that_escapes_workspace` the same way: the symlink target must be an ungranted directory rather than a `tempfile` directory, and the context must be `WorkspaceWrite`. Add positive tests for a settings directory, an `--add-dir` directory, a skill directory, and a settings directory under the `ReadOnly` policy, and a `DangerFullAccess` test that admits a directory no grant covers. Add rejection tests for a system directory (`/etc` or `/usr`) and for an `--add-dir` directory selected under a context that lacks that grant.

Then update the prose and snapshots, and finally run the full gate.

## Concrete Steps

All commands run from `/Users/travisennis/Projects/cake/cake-1`.

1. Edit `src/clients/tools/sandbox/mod.rs` and its test module; run `cargo test clients::tools::sandbox` and `just cc-check`.
2. Edit `src/clients/tools/bash.rs` and `src/clients/tools/bash_tests.rs`; run `cargo test clients::tools::bash` and `just cc-check`.
3. Edit `src/clients/tools/bash-description.txt`, `docs/security.md`, and `docs/integrations.md`; run `cargo insta test --accept`, then `just docs-fmt` and `just docs-check`.
4. Run the gate: `just fmt-check`, `just clippy`, `just test`, `just check`.

A successful focused run ends with a line like this, and no failures:

```
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

The baseline check prints nothing and exits 0 when every function is within its allowance:

```
just cc-check
(no output)
```

## Validation and Acceptance

Behavior, in the terms a reviewer can check by hand:

1. A call whose `cwd` names a directory inside a read-write grant runs there. With a context whose workspace is a `tempfile` directory and a settings directory granted read-write, `cwd` set to the settings directory, and the command `pwd -P`, the output names the canonical settings directory and the footer shows exit 0.
2. A call whose `cwd` names a directory granted read-only by `--add-dir` or by a skill directory is admitted and runs there, under both `WorkspaceWrite` and `ReadOnly`.
3. A call whose `cwd` names a directory outside every grant fails with a tool error before judge evaluation and before spawn, on a `WorkspaceWrite` context, and the message names the invocation workspace and the grant classes. The test asserts that no marker file the command would have created exists afterward, which is how the existing rejection tests prove no spawn happened.
4. A symlink inside the workspace whose canonical target is outside every grant is rejected, and a symlink whose canonical target is inside a grant is accepted.
5. Under `DangerFullAccess`, any existing directory is admitted, including the ungranted fixture path, while a missing path and a file path are still rejected.
6. Under every policy, the child process, the judge request's `cwd`, the repository digest input, and the sandbox-denial scan receive the same canonical path. The existing assertions on `JudgeRequest.cwd` and on the denial diagnostic continue to hold.
7. No path becomes writable that was not writable before. The Seatbelt and Landlock profile generation code is untouched, so a `ReadOnly` context still denies a write inside a granted directory that the test selects as `cwd`.

Verification commands and what they prove: `cargo test clients::tools::bash` proves the admission matrix and the spawn guard; `cargo test clients::tools::sandbox` proves the grant-set predicate including the `ReadOnly` demotion; `just cc-check` proves no function exceeded its complexity allowance; `cargo insta test --accept` proves the two provider snapshots match the new wording; `just check` proves the repository gate. macOS behavior is proved by the macOS `Test` CI job, since a Cake session cannot apply a nested Seatbelt profile (ADR 016) and the Linux jobs do not select tests in the `bash` module. The Linux expectation, to be confirmed or refuted by the Linux jobs, is that Landlock has no access right governing `chdir`, so a directory admitted only by the coarse predicate starts the process and fails at the first file access, while macOS fails at the read.

## Idempotence and Recovery

Every source and documentation edit is safe to repeat after rereading the file. Running `cargo insta test --accept` repeatedly is safe and leaves no pending snapshots. The test commands write only build artifacts and temp fixtures, and no step deletes tracked files. If `just cc-check` reports an exceedance, move the branch into a new function or into `resolve_bash_cwd`, which has no baseline entry; do not regenerate the baseline for this change. If the document checks reject the new prose, run `just docs-fmt` and re-run `just docs-check`. If a snapshot mismatch appears that is not the two Bash description lines, stop and inspect it rather than accepting it. If the plan move has already happened, update the completed path in place instead of recreating an active copy.

## Artifacts and Notes

Focused transcripts, from `/Users/travisennis/Projects/cake/cake-1`:

```
$ cargo test --bin cake clients::tools::bash
test result: ok. 131 passed; 0 failed; 0 ignored; 0 measured; 1394 filtered out

$ cargo test --bin cake clients::tools::sandbox
test result: ok. 59 passed; 0 failed; 0 ignored; 1458 filtered out

$ cargo test --all-features --quiet
test result: ok. 1523 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out
(plus every integration suite: 11, 18, 16, 15, 7, 5, 17, 16, 11, and 9 tests, all ok)

$ just cc-check
CC gate: 1097 functions checked, 43 new, 0 over allowed
PASS: No cyclomatic complexity exceedances

$ cargo fmt -- --check        # no output, exit 0
$ cargo clippy --all-targets --all-features -- -D warnings   # Finished, exit 0
$ just docs-fmt               # 133 files left unchanged
$ just docs-check             # exit 0
$ just check                  # Fast local checks passed!
```

The snapshot diff is exactly the two intended lines: the `cwd` parameter description in `cake__clients__chat_completions__tests__chat_request_full_with_agents_and_skills.snap` and `cake__clients__responses__tests__responses_request_full_with_agents_and_skills.snap`, plus the embedded `bash-description.txt` sentence in each.

The two command-running tests print their skip notice when a Cake session runs them, because this process cannot apply a nested Seatbelt profile:

```
skipping macOS sandbox integration test: sandbox-exec cannot apply profiles in this process context
```

The initial evidence for the design is in `docs/adr/032-bash-cwd-sandbox-grants.md` and in the issue #588 discussion.

## Interfaces and Dependencies

New and changed internal interfaces, all private to the crate:

- `crate::clients::tools::sandbox::SandboxConfig::user_grants`, a `Vec<PathBuf>` field holding user-granted directories in lexical and canonical form, populated before `partition_read_only`. Chosen over deriving the set from `read_execute` because the read-only policy merges user grants with the built-in system paths there.
- `crate::clients::tools::sandbox::SandboxConfig::is_path_selectable(&self, path: &Path) -> bool`, `pub(super)`, true for read-write grants and user grants. Chosen as a new predicate instead of widening `is_path_allowed`, whose baseline complexity is 1 and whose meaning is "readable or executable".
- `resolve_bash_cwd(context: &ToolContext, config: &SandboxConfig, requested: Option<&Path>) -> Result<PathBuf, String>`, extended with the config used for the grant check; the policy comes from `context.sandbox_policy`.
- `parse_bash_call(context: &ToolContext, arguments: &str) -> Result<(BashExecutionArgs, PathBuf, SandboxConfig), String>`, extended to return the single built config.
- `execute_bash_with_args(context: &ToolContext, args: BashExecutionArgs, cwd: PathBuf, sandbox_config: &SandboxConfig, call_id: Option<String>) -> Result<ToolResult, ToolError>`, extended to take the shared config.
- `prepare_bash_command(args: &BashExecutionArgs, cwd: &Path, sandbox_config: &SandboxConfig, judge_events: &[CompensationEventTelemetry])` and `sandbox_denials(command: &str, cwd: &Path, config: &SandboxConfig, sandbox_applied: bool, success: bool, output: &str, stderr: &str)`, both extended to take the shared config and neither taking `context` any longer.

No dependency, schema field, persisted record, or platform sandbox API changes. The provider-facing schema for `cwd` keeps its `string` type; only its description text changes.
