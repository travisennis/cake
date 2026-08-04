# Sandbox Policy CLI Flag

This ExecPlan implements task 195: a `--sandbox` / `-s` CLI flag with three policies (`read-only`, `workspace-write`, `danger-full-access`) that replaces the hidden `CAKE_SANDBOX=off` env-var escape hatch with explicit, discoverable control while preserving backward compatibility.

This ExecPlan is a living document. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective are kept up to date as work proceeds. This document is maintained in accordance with [docs/workflow/exec-plans.md](../../workflow/exec-plans.md).

## Purpose / Big Picture

After this change, a user can choose how strict cake's filesystem sandbox is per run. Today there are only two states: sandbox-on (the default, which is read-write for the project dir and toolchain caches) and sandbox-off (only via the undocumented `CAKE_SANDBOX=off` environment variable). There is no way to ask cake to run the agent's shell commands with no writes at all.

After the change the user can run:

```
cake --sandbox read-only "Audit this repo for secrets"
cake -s read-only "Review the diff"            # short flag works
cake --sandbox danger-full-access "Run setup"  # no sandbox at all
cake "Do anything"                              # unchanged: workspace-write
```

The `read-only` policy is useful for auditing, untrusted prompts, and defensive runs: the agent may read the project and system paths but cannot mutate the project directory or toolchain caches. `workspace-write` is the current default and is byte-for-byte unchanged. `danger-full-access` is the explicit, discoverable replacement for `CAKE_SANDBOX=off`. The env var still works for backward compatibility when `--sandbox` is not passed; when `--sandbox` is passed it takes precedence over the env var.

## Progress

- [x] (2026-07-09) ExecPlan written; task exec_plan field wired.
- [x] Define `SandboxPolicy` enum and `resolve_sandbox_policy` in sandbox module.
- [x] Add `sandbox_policy` to `ToolContext`; thread resolved policy from main.
- [x] Add `--sandbox` / `-s` CLI flag to `CodingAssistant` (`Option<SandboxPolicy>`).
- [x] Partition `SandboxConfig` writable/readable based on policy (read-only).
- [x] Gate hardcoded macOS SCM CLI write rules behind read-only.
- [x] Update `BashExecutionArgs` to carry policy; keep `with_sandbox` test helper.
- [x] Add tests for all three policies + env-var precedence + parse rejection.
- [x] Update docs (sandbox.md, cli.md, README.md) and `--help` surface.
- [x] `cargo fmt`, `cargo clippy --all-targets --all-features -- -D warnings`, `just ci` (green; cargo-crap baseline recaptured).
- [x] (2026-07-09) ADR-014 created and accepted.

## Surprises & Discoveries

- Observation: `cargo clippy --all-targets --all-features -- -D warnings` flagged `let _ = std::fs::remove_file(&target)` in a new bash test with the `let_underscore_must_use` lint. The existing codebase uses `_ = expr;` (without `let`) for best-effort cleanup. Matching that pattern keeps clippy clean. Evidence: `cargo clippy` after switching to `_ = std::fs::remove_file(&target);` produced no warnings.
- Observation: The `cargo-crap` change-risk regression reported two regressions (`append_scm_cli_rules`, `with_sandbox`) caused by the new read-only branches. CONTRIBUTING.md documents recapturing the baseline after intentional complexity changes; `just change-risk-baseline` regenerated `ci/cargo-crap-baseline.json` and `just check-coverage` then reported `0 regressed`. Evidence: `just check-coverage` -> `PASS: No CRAP regression detected`.
- Observation: `cargo clippy`'s `doc_markdown` lint requires `CAKE_SANDBOX` to be backticked in doc comments. Evidence: clippy error on `src/main.rs:126` fixed by `` `CAKE_SANDBOX` ``.
- Observation: The read-only partition test initially used `~/.cargo`, but `CARGO_HOME` env overrides the `home.join(".cargo")` path computed by `extend_with_toolchain_paths`, so the asserted path was not the one selected. Switched to `~/.bun`, which has no env override. Evidence: `read_only_policy_moves_workspace_and_toolchain_to_readable` failed on `.cargo`, passed on `.bun`.

## Decision Log

- Decision: Model the CLI flag as `Option<SandboxPolicy>` (no clap `default_value`) and resolve to a `SandboxPolicy` before building `ToolContext`. Rationale: The acceptance criterion "`--sandbox` CLI flag takes precedence over `CAKE_SANDBOX` env var if both are set" requires knowing whether the user passed the flag explicitly. With `default_value = "workspace-write"` the resolved value is indistinguishable between "user passed `--sandbox workspace-write`" and "user passed nothing". Using `Option` makes presence explicit: `Some(policy)` always wins; `None` falls back to the env var and then to the `workspace-write` default. The default behavior (no flag) is still `workspace-write`, so existing users see no change. Date/Author: 2026-07-09, cake agent.
- Decision: For `read-only`, move every workspace-write path except temp dirs from `SandboxConfig.writable` into `SandboxConfig.readable`, and keep temp dirs read-write so commands can still produce intermediate output (pipes, mktemp). Rationale: Matches the task design notes, which say "Keep temp directory access (but only read+write to temp, not workspace)". Temp dirs staying writable preserves the Bash tool's overflow-output temp-file behavior. Date: 2026-07-09.
- Decision: Gate the macOS `append_scm_cli_rules` profile helper so it emits read-only rules when the policy is `read-only`. Rationale: Those rules hardcode read-write access to `~/.config/gh`, `~/.cache/gh`, the glab dirs, etc. The toolchain path list in `extend_with_toolchain_paths` already includes those paths and is moved to readable in read-only mode, so leaving the Seatbelt helper read-write would re-grant writes the partition removed. Date: 2026-07-09.
- Decision: Keep `is_sandbox_disabled()` and `CAKE_SANDBOX=warn` behavior as-is. Rationale: `is_sandbox_disabled()` is still consulted by `resolve_sandbox_policy` for backward compatibility and by existing sandbox test guards. Changing the `warn` semantics is out of scope. Date: 2026-07-09.
- Decision: Drop `build_with_additional_dirs` instead of keeping it as a convenience wrapper. Rationale: The original plan kept it "to preserve the public API," but `SandboxConfig` is `pub(super)` inside a private module, so there is no external API to preserve and the wrapper was dead code that broke the `-D warnings` clippy gate. Removed it entirely; `SandboxConfig::build` now delegates directly to `build_with_policy`. Date: 2026-07-09.
- Decision: Use `_ = expr;` (not `let _ = expr;`) for best-effort cleanup in the new `test_sandbox_read_only_blocks_write_in_cwd` test. Rationale: clippy's `let_underscore_must_use` lint fires on `let _ = <must_use>`; the existing codebase already uses the underscore-assignment form for best-effort drops. Date: 2026-07-09.
- Decision: Recapture the `cargo-crap` baseline (`ci/cargo-crap-baseline.json`). Rationale: Two new read-only branches (`append_scm_cli_rules`, the remapped `with_sandbox` test helper) intentionally increased complexity. CONTRIBUTING.md directs committing a refreshed baseline after intentional coverage/complexity changes. Date: 2026-07-09.

## Outcomes & Retrospective

All task 195 acceptance criteria are satisfied. The user-facing outcome is delivered: `cake --sandbox read-only|workspace-write|danger-full-access` works (long and short forms), `cake --help` shows the flag with the three choices, the default (`workspace-write`) is byte-for-byte unchanged, and `CAKE_SANDBOX=off` still maps to `danger-full-access` when no flag is passed while `--sandbox` takes precedence when both are set.

What was achieved: the full sandbox-policy spectrum is now discoverable via the CLI; the read-only partition correctly moves the workspace, toolchain, runtime/SCM, and settings dirs from `writable` to `readable` while keeping temp dirs read-write; macOS Seatbelt SCM-CLI rules gate to read-only; Linux Landlock enforces read-only automatically because it groups `SandboxConfig.writable` as read-write and everything else as read-only/read+exec. Tests cover all three policies (with macOS integration tests gated by `skip_if_sandbox_unavailable`), env-var backward compat, CLI precedence, the read-only partition, and invalid-value parse rejection.

What remains (deferred, see task Future Considerations): settings-file sandbox policy override, per-session policy persistence on continue/resume, named sandbox profiles in `settings.toml`, and a possible `danger-semi-safe` (network-only) policy. Linux cross-compile clippy (`cargo clippy --target x86_64-unknown-linux-gnu`) was not run on this macOS host; the read-only Landlock path derives its enforcement from the partitioned `SandboxConfig`, so the invariants hold, but a CI Linux run is the authoritative check. ADR-014 records the durable decision.

Lessons: (1) Modeling the CLI field as `Option<SandboxPolicy>` rather than using a clap `default_value` was the right call for precedence resolution and avoided silent-override ambiguity. (2) Recapturing `cargo-crap` baselines after intentional branch additions is a documented, expected step; treat it as part of the acceptance gate rather than a CI surprise.

## Context and Orientation

cake is a one-shot Rust 2024 CLI. A single invocation builds an agent that calls tools; the Bash tool runs shell commands inside an OS-level filesystem sandbox.

Key files and how they fit together:

- `src/main.rs` defines the clap `CodingAssistant` struct (the CLI) and assembles a `ToolContext` (see below) from the parsed flags before running the agent.
- `src/clients/tools/mod.rs` defines `ToolContext`, the directory context shared by all tools. It carries `cwd`, `temp_dirs`, `additional_dirs` (from `--add-dir`), `skill_dirs` (parent dirs of `SKILL.md` files), and `settings_dirs` (persistent read-write dirs from `settings.toml`). It is re-exported from `src/clients/mod.rs` as `crate::clients::ToolContext`.
- `src/clients/tools/sandbox/mod.rs` defines `SandboxConfig` (the lists of `writable`, `system_paths`, and `readable` paths), `SandboxConfig::build` (which derives those lists from a `ToolContext`), `SandboxStrategy` (the trait that applies the config to a command), `detect_platform` (returns the macOS or Linux strategy), and `is_sandbox_disabled` (reads `CAKE_SANDBOX`). The sandbox module is private (`mod sandbox;` in `tools/mod.rs`); its public items are reachable from `bash.rs` as `super::sandbox::...`.
- `src/clients/tools/sandbox/macos.rs` generates a Seatbelt `sandbox-exec` profile from a `SandboxConfig` via `generate_profile`. Some rules are hardcoded helpers (`append_git_rules`, `append_ssh_agent_rules`, `append_scm_cli_rules`, `append_keychain_rules`, `append_device_rules`).
- `src/clients/tools/sandbox/linux.rs` applies Landlock rules from a `SandboxConfig` via `apply_landlock_rules`: writable paths get read-write, system paths get read+execute, readable paths get read-only.
- `src/clients/tools/bash.rs` defines `BashExecutionArgs` (currently with a `use_sandbox: bool` set from `is_sandbox_disabled()` in `from_json`), the `execute_bash` entry point, and `execute_bash_with_args` which builds the sandbox config, applies the strategy when `use_sandbox`, and classifies output.

Terms:

- A **sandbox policy** is one of `ReadOnly`, `WorkspaceWrite`, or `DangerFullAccess`. `WorkspaceWrite` is the default and reproduces today's behavior exactly.
- **Seatbelt** is macOS's `sandbox-exec` profile language (deny-default, with explicit `file-read*` / `file-read* file-write*` allow rules). **Landlock** is the Linux LSM that restricts the child process in `pre_exec`.

## Plan of Work

### 1. Define the policy type and resolver (sandbox/mod.rs)

Add a public enum `SandboxPolicy` with unit variants `ReadOnly`, `WorkspaceWrite`, `DangerFullAccess`. Derive `Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum`. Mark `WorkspaceWrite` as `#[default]`. clap's `ValueEnum` renames variants to kebab-case automatically (`read-only`, `workspace-write`, `danger-full-access`), which is what the user types.

Add a function `resolve_sandbox_policy(cli: Option<SandboxPolicy>) -> SandboxPolicy` in `sandbox/mod.rs`: if `cli` is `Some`, return it; otherwise consult `is_sandbox_disabled()` and return `DangerFullAccess` when the env var disables sandboxing; otherwise return `WorkspaceWrite`.

Re-export `SandboxPolicy` and `resolve_sandbox_policy` from `src/clients/tools/mod.rs` (`pub use sandbox::{SandboxPolicy, resolve_sandbox_policy};`) and from `src/clients/mod.rs` (`pub use tools::{SandboxPolicy, resolve_sandbox_policy};`) so `main.rs` can name them as `crate::clients::SandboxPolicy`.

### 2. Thread the policy through ToolContext (tools/mod.rs, main.rs)

Add `pub sandbox_policy: SandboxPolicy` to the `ToolContext` struct. Keep `with_temp_dirs` (a `const fn`) taking the same parameters and set `sandbox_policy: SandboxPolicy::WorkspaceWrite` as the field default; this avoids touching the many test call sites. Change `ToolContext::new` to take a `sandbox_policy` argument and set it (there is exactly one production caller, in `main.rs`). `from_current_process` defaults the field to `WorkspaceWrite`.

In `main.rs`, add the CLI field, resolve it via `resolve_sandbox_policy(self.sandbox)`, and pass it into `ToolContext::new`.

### 3. Add the CLI flag (main.rs)

Add to `CodingAssistant`:

```
/// Select the sandbox policy for model-generated shell commands
/// (read-only, workspace-write, danger-full-access). Default:
/// workspace-write. Takes precedence over CAKE_SANDBOX.
#[arg(short, long, value_enum, value_name = "POLICY")]
pub sandbox: Option<SandboxPolicy>,
```

Using `Option` (with no `default_value`) is intentional; see the Decision Log.

### 4. Partition SandboxConfig by policy (sandbox/mod.rs)

Add `SandboxConfig::build_with_policy(policy, cwd, temp_dirs, additional_dirs, settings_dirs, skill_dirs)` containing the current `build_with_additional_dirs` logic followed by a read-only partition step. When `policy == ReadOnly`:

- Build the writable list exactly as today (cwd, temp, toolchain caches, settings dirs), deduplicated with canonical forms (this is `full_writable`).
- Set the effective `writable` to the temp dirs only (deduplicated with canonical forms). Temp dirs stay read-write so pipes and overflow temp files work.
- Move every entry of `full_writable` that is not a temp dir into `readable` (these are the workspace dir and the toolchain/runtime/SCM/settings caches). Deduplicate `readable` afterward.

Keep `build_with_additional_dirs` as a thin wrapper delegating to `build_with_policy(SandboxPolicy::WorkspaceWrite, ...)` so the public API is preserved. `build(context)` reads `context.sandbox_policy` and calls `build_with_policy` with it, so `DangerFullAccess` and `WorkspaceWrite` produce the same config (the policy only changes whether the sandbox is applied at all and whether toolchain dirs are writable; `DangerFullAccess` skips application entirely in `execute_bash_with_args`).

### 5. Gate macOS SCM rules behind read-only (sandbox/macos.rs)

Pass the policy into `generate_profile` (or a `bool read_only` derived from it). In `append_scm_cli_rules`, when read-only, emit `file-read*` (subpath) rules instead of `file-read* file-write*`, so the hardcoded SCM CLI write grants do not re-open writes that the config partition closed. The git, ssh-agent, keychain, and device helpers are left as-is; none of them grant writes to project or toolchain directories.

### 6. Update BashExecutionArgs (bash.rs)

Replace the `use_sandbox: bool` field with `policy: SandboxPolicy`. In `from_json`, default the policy to `WorkspaceWrite` (the env-var/CLI resolution happens earlier in `main.rs` and is carried by `ToolContext`). In `execute_bash`, set `args.policy = context.sandbox_policy` before invoking `execute_bash_with_args`. In `execute_bash_with_args`, compute `let use_sandbox = args.policy != SandboxPolicy::DangerFullAccess;` and use that local for the existing `if args.use_sandbox` apply step and for the `is_sandbox_initialization_failure` / `is_sandbox_violation` classification calls. `SandboxConfig::build(context)` already honors the policy via `context.sandbox_policy`, so no change is needed at the build call site.

Keep the `#[cfg(test)] with_sandbox(use_sandbox: bool)` helper, mapping `true` to `SandboxPolicy::WorkspaceWrite` and `false` to `SandboxPolicy::DangerFullAccess` so the existing unsandboxed test helper keeps working unchanged.

### 7. Tests

- Unit test `resolve_sandbox_policy`: `Some(ReadOnly)` returns `ReadOnly`; `None` under `CAKE_SANDBOX=off` returns `DangerFullAccess`; `None` otherwise returns `WorkspaceWrite`; `Some(WorkspaceWrite)` returns `WorkspaceWrite` even when `CAKE_SANDBOX=off` is set (CLI precedence).
- Unit test `SandboxConfig::build_with_policy(ReadOnly, ...)`: the cwd and a representative toolchain path (e.g. `~/.cargo`) are in `readable`, not `writable`; temp dirs remain in `writable`.
- macOS integration tests (gated by `skip_if_sandbox_unavailable`): `--sandbox read-only` rejects `touch` in the project dir; `workspace-write` allows it; `danger-full-access` allows it.
- Parse test: an invalid value (e.g. `--sandbox nope`) is rejected by clap.

### 8. Docs

Update `docs/design-docs/sandbox.md` (add a "Sandbox Policies" subsection with the flag, the three values, precedence over `CAKE_SANDBOX`, and the backward-compat note), `docs/design-docs/cli.md` (add the flag to the struct listing and the options description), and `README.md` (the Options list and the Filesystem Sandbox section). Keep `CAKE_SANDBOX` documented as still supported.

## Concrete Steps

Run all commands from the repository root (`/Users/travisennis/Projects/cake`).

After each milestone, run a narrow check:

```
cargo check --tests
cargo test sandbox
cargo test bash
```

Acceptance gate before handoff:

```
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
just ci
```

To see the flag surface:

```
cargo run -- --help | grep -A2 sandbox
```

Expected: a `-s, --sandbox <POLICY>` line with the three values described.

To verify read-only blocks writes end-to-end on macOS:

```
cargo run --quiet -- --sandbox read-only "use the Bash tool to run: touch cake-ro-probe-$RANDOM"
```

The tool result should report `Operation not permitted` or `Permission denied`.

## Validation and Acceptance

The task acceptance notes are the source of truth; in summary:

- `cake --sandbox read-only` runs Bash with a sandbox that denies writes to the workspace and toolchain caches.
- `cake --sandbox workspace-write` behaves identically to the current default.
- `cake --sandbox danger-full-access` runs Bash without sandbox restrictions (like `CAKE_SANDBOX=off`).
- `cake -s read-only` works (short flag).
- Default (no `--sandbox`) is unchanged (workspace-write).
- `CAKE_SANDBOX=off` still disables the sandbox when `--sandbox` is not given.
- `--sandbox` takes precedence over `CAKE_SANDBOX` when both are set.
- macOS Seatbelt and Linux Landlock both enforce read-only correctly.
- `cake --help` shows the flag with the three choices.
- Tests cover all three policies, read-only rejection, env-var backward compat, CLI precedence, and invalid-value rejection.
- `cargo fmt` and `just ci` pass.

The new unit tests for `resolve_sandbox_policy` and `build_with_policy` must pass on all platforms; the macOS integration tests are gated so they skip when the platform sandbox cannot be enforced.

## Idempotence and Recovery

All changes are additive. The default policy is `WorkspaceWrite`, so runs without `--sandbox` are byte-for-byte unchanged. Re-running the implementation steps is safe because each step is a localized edit. If a step fails (for example a clippy lint), fix and re-run the same command; nothing mutates shared state.

## Artifacts and Notes

(to be populated with short transcripts during implementation)

## Interfaces and Dependencies

In `src/clients/tools/sandbox/mod.rs`, define:

```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum SandboxPolicy {
    ReadOnly,
    #[default]
    WorkspaceWrite,
    DangerFullAccess,
}

pub fn resolve_sandbox_policy(cli: Option<SandboxPolicy>) -> SandboxPolicy;
```

In `src/clients/tools/sandbox/mod.rs`, add:

```
pub fn build_with_policy(
    policy: SandboxPolicy,
    cwd: &std::path::Path,
    temp_dirs: &[std::path::PathBuf],
    additional_dirs: &[std::path::PathBuf],
    settings_dirs: &[std::path::PathBuf],
    skill_dirs: &[std::path::PathBuf],
) -> Self;
```

In `src/clients/tools/mod.rs`, the `ToolContext` struct gains:

```
pub sandbox_policy: SandboxPolicy,
```

and `ToolContext::new` gains a `sandbox_policy: SandboxPolicy` parameter.

In `src/main.rs`:

```
#[arg(short, long, value_enum, value_name = "POLICY")]
pub sandbox: Option<SandboxPolicy>,
```

No new crate dependencies; `clap` is already a dependency and `ValueEnum` is already used by `OutputFormat`.
