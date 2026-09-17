# Add an Explicit Bash Command Working Directory

This ExecPlan is a living document, maintained per `docs/workflow/exec-plans.md`. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must stay current while issue #575 is implemented.

## Purpose / Big Picture

Agents will be able to send an optional `cwd` with a Bash tool call to run one command in a project subdirectory without embedding `cd` in the shell text. Omitting `cwd` will preserve current behavior. A human can verify the feature by asking Cake to run `pwd -P` with `cwd` set to an existing workspace subdirectory and observing that the output names that directory, while an outside or missing directory fails before command execution.

## Progress

- [x] (2026-09-17) Inspected the Bash schema, executor, judge preflight, sandbox configuration, and focused tests.
- [x] (2026-09-17) Recorded the security and compatibility decision in ADR 031.
- [x] (2026-09-17) Added parsing, validation, execution, judge-context, and schema tests.
- [x] (2026-09-17) Updated model-visible, integration, and security documentation plus Chat Completions and Responses snapshots.
- [x] (2026-09-17) Ran focused verification, snapshot verification, formatting, documentation checks, and the repository gate.
- [x] (2026-09-17) Filled Outcomes & Retrospective; the plan is ready to move to `docs/exec-plans/completed/`.

## Surprises & Discoveries

- Observation: The default macOS Xcode linker refused to run because the Xcode license had not been accepted. Evidence: the initial `cargo test`, `just snapshots`, and `just check` attempts failed while linking `rustls`, `aws-lc-rs`, or `aws-lc-sys` with exit 69. The separately installed Command Line Tools compiler and SDK provided a non-invasive workaround, after which the same checks passed.

## Decision Log

- Decision: Constrain `cwd` to an existing directory beneath the canonical invocation working directory, resolve relative values from that directory, and use the canonical result for every judge and execution surface. Rationale: explicit subdirectory execution is useful, while accepting an arbitrary path would widen an untrusted model's filesystem authority. Date/Author: 2026-09-17 / Travis Ennis.
- Decision: Keep the selected directory per-call rather than mutating `ToolContext`. Rationale: tool calls are independently scheduled and persisted, so implicit cross-call shell state would be surprising and difficult to reason about. Date/Author: 2026-09-17 / Travis Ennis.

## Outcomes & Retrospective

The Bash provider-facing schema now accepts an optional per-call `cwd`. Relative paths are resolved from the invocation workspace, canonicalized, and restricted to existing directories within that workspace. The effective path is used for the child process, judge request, repository digest, and sandbox-denial diagnostics, while omitted `cwd` retains existing behavior. Focused Bash tests, all 1,510 unit tests plus 2 ignored tests, all integration suites, snapshot tests, formatting, documentation checks, and `just check` passed with the Command Line Tools linker workaround. No sandbox platform implementation or dependency changed.

## Context and Orientation

`src/clients/tools/bash.rs` defines the Bash provider schema, parses JSON arguments, runs the command, performs the LLM judge preflight, and formats sandbox-denial diagnostics. `ToolContext.cwd` is the invocation working directory and is also the root used by `SandboxConfig::build` in `src/clients/tools/sandbox/mod.rs`. The macOS and Linux sandbox adapters preserve the command's configured current directory while applying the same grants. `src/clients/tools/bash-description.txt` is the model-visible operational description, and `src/clients/tools/bash_tests.rs` contains focused unit and integration tests.

The effective cwd must be canonicalized before use. A relative request is joined to `ToolContext.cwd`; an absolute request is checked directly. A request is valid only when it exists, is a directory, and is inside the canonical invocation directory. This preserves the existing sandbox root: no sandbox path collection needs to change.

## Plan of Work

First extend `BashExecutionArgs` and `bash_tool` in `src/clients/tools/bash.rs` with an optional `cwd` string. Resolve and validate it at the start of the common execution path so both production calls and focused test helpers reject invalid paths before judge evaluation or process spawn. Use the resolved path when building the child command, constructing `JudgeRequest`, computing the repository digest, and scanning failed commands for denied paths.

Then update `src/clients/tools/bash-description.txt` and the Bash tool section of `docs/integrations.md` to describe per-call cwd behavior and its workspace restriction. Add focused tests for schema exposure, argument validation, relative resolution, execution in a subdirectory, outside/missing/non-directory rejection, and omitted-argument compatibility. Keep the existing sandbox platform implementations unchanged and verify the full repository gate.

## Concrete Steps

All commands run from `/Users/travisennis/Projects/cake/cake-1`.

1. Edit `src/clients/tools/bash.rs`, `src/clients/tools/bash_tests.rs`, `src/clients/tools/bash-description.txt`, and `docs/integrations.md`.
2. Run `cargo test clients::tools::bash` and inspect the focused test result.
3. Run `cargo fmt --all -- --check` and `just check`.
4. Update this plan's progress and outcomes, then move it with `git mv docs/exec-plans/active/bash-command-working-directory.md docs/exec-plans/completed/bash-command-working-directory.md`.

## Validation and Acceptance

The existing schema still requires only `command`, and a schema consumer sees optional `cwd` with a description explaining relative resolution and the workspace restriction. A call without `cwd` executes from `ToolContext.cwd`. A call with `cwd: "nested"` executes from `<ToolContext.cwd>/nested`, and `pwd -P` reports its canonical path. A missing path, file path, or path outside the workspace returns a validation error and does not spawn Bash. The judge request and sandbox-denial path use the selected canonical directory. Existing timeout, output, sandbox, and judge tests continue to pass.

## Idempotence and Recovery

All source and documentation edits are additive and safe to repeat after rereading the file. Test commands do not mutate the repository except for ordinary build artifacts. If validation exposes a path or platform issue, fix the implementation and rerun the focused test before the full gate. If the plan move has already happened, update the completed path in place rather than recreating an active copy.

## Artifacts and Notes

The final diff and focused test transcript demonstrate the new schema and behavior. The two provider request snapshots were updated because the Bash schema and model-visible description changed. No dependency update was needed.

## Interfaces and Dependencies

The public provider-facing interface changes additively: the `Bash` function schema in `src/clients/tools/bash.rs` accepts optional `cwd`. Internal execution continues through `BashExecutionArgs`, `prepare_bash_command`, `bash_judge_preflight`, `repo_state_digest`, and `sandbox_denials`. No new dependency or sandbox platform API is required.
