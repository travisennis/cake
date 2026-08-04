---
status: accepted
date: 2026-07-09
---

# Sandbox Policy CLI Flag

## Context and Problem Statement

Cake's Bash tool runs model-generated shell commands in an OS-level filesystem sandbox (Seatbelt on macOS, Landlock on Linux). Before this change there were only two sandbox states:

1. **On** (default): read-write access to the project directory, temp directories, and a broad set of toolchain/runtime caches; read-only access to system and config paths.
2. **Off**: only via the undocumented `CAKE_SANDBOX=off` environment variable. No CLI flag existed, and there was no way to ask cake to deny writes entirely while still allowing reads.

Users wanted three things that the two-state model could not provide: (a) an explicit, discoverable CLI control instead of an undocumented env var, (b) a read-only mode for auditing, untrusted prompts, or defensive runs, and (c) a backward-compatible path so existing `CAKE_SANDBOX=off` users did not break.

## Decision Drivers

- **Discoverability**: The sandbox strictness should be visible in `cake --help`, not hidden behind an env var.
- **Security posture**: There should be a sandbox state strictly more restrictive than the default that denies all mutations to the workspace and toolchain caches while still allowing reads and command execution.
- **Backward compatibility**: `CAKE_SANDBOX=off` (and its aliases `0`/`false`/`no`) must keep working; existing users must see no behavior change when they pass no flags.
- **Precedence clarity**: When both the CLI flag and the env var are set, the explicit CLI choice should win so users can override environment defaults on a per-run basis.
- **Cross-platform parity**: macOS Seatbelt and Linux Landlock should enforce the same three policies via the same `SandboxConfig` path lists.

## Considered Options

- **Option A: Three-state `--sandbox` CLI flag with values `read-only`, `workspace-write`, `danger-full-access`.** A `clap` `ValueEnum` with `WorkspaceWrite` as the default. `CAKE_SANDBOX=off` stays supported as a fallback when no flag is passed; the flag takes precedence when both are set.
- **Option B: A `--read-only` boolean flag.** Add only a read-only toggle, leaving `CAKE_SANDBOX=off` as the sole "off" path.
- **Option C: Settings-file-only policy.** Put the policy in `settings.toml` only, with no CLI flag.

## Decision Outcome

Chosen option: **Option A**, because it satisfies all decision drivers in one user-visible surface. The three values cover the full strictness spectrum, the flag is discoverable via `--help`, `danger-full-access` is the explicit replacement for `CAKE_SANDBOX=off`, and the `Option<SandboxPolicy>` field type (no clap `default_value`) makes CLI-vs-env precedence unambiguous: an explicit flag always wins, and the env var only applies when the flag is absent.

### Consequences

- Good, because users can now choose sandbox strictness per run: `cake --sandbox read-only "Audit this repo"`.
- Good, because the read-only policy denies writes to the workspace and toolchain caches while keeping temp directories read-write so commands can still produce intermediate output (pipes, mktemp, overflow temp files). This is the strictest mode cake has offered.
- Amended (2026-07-09, task 252): the OS sandbox only wraps Bash, so the read-only policy must also remove the Edit and Write tools from the agent's registry --- they mutate files in-process and would otherwise bypass the policy entirely. Under `read-only` the agent is offered only Bash (sandboxed) and Read; the system prompt tool list, session header, and stream-json `tools` field reflect the reduced set. Removing the tools (rather than registering them and returning a policy error) keeps the advertised tool list truthful and avoids wasted model turns.
- Amended (2026-07-09, tasks 253/254): read-only enforcement fixes --- Landlock now grants Execute on read-only paths so read-only sessions can still run workspace/toolchain binaries on Linux (matching macOS Seatbelt semantics), and the macOS keychain file rule is gated to `file-read*` under read-only like the SCM CLI rules.
- Good, because `CAKE_SANDBOX=off` continues to work for backward compatibility, mapped to `danger-full-access` when no `--sandbox` flag is passed.
- Good, because the default (`workspace-write`) reproduces the historical sandbox behavior byte-for-byte; no existing user sees a change.
- Neutral: The macOS Seatbelt profile helper for SCM CLI dirs (`~/.config/gh`, `~/.cache/gh`, the glab dirs, etc.) must check the policy and emit `file-read*` instead of `file-read* file-write*` when read-only, because those rules would otherwise re-grant writes that the partitioned `SandboxConfig` removed.
- Bad, because two new code branches (the read-only partition and the SCM CLI read-only gating) add a small amount of complexity. Tests cover both branches, and the `cargo-crap` baseline was recaptured to reflect the intentional change.

## More Information

- Task 195: Add `--sandbox` CLI Flag With Read-Only, Workspace-Write, and Danger-Full-Access Policies.
- ExecPlan: `docs/exec-plans/completed/sandbox-policy-flag.md` (see the Decision Log for the `Option<SandboxPolicy>` vs `default_value` rationale and the temp-dir-retention rationale).
- `docs/security.md` (Sandbox policies).
- `src/clients/tools/sandbox/mod.rs` --- `SandboxPolicy`, `resolve_sandbox_policy`, `SandboxConfig::build_with_policy`, `SandboxConfig::partition_read_only`.
- `src/clients/tools/sandbox/macos.rs` --- `append_scm_cli_rules` read-only gating.
