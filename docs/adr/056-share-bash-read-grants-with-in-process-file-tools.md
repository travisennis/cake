---
status: accepted
date: 2026-11-09
decision-makers: Travis Ennis
informed: task 171
---

# Share Bash read grants with in-process file tools

## Context and Problem Statement

Cake authorizes filesystem access twice. Model-generated Bash commands run under an OS sandbox (Seatbelt on macOS, Landlock on Linux) whose path grants include built-in toolchain and system paths. In-process `Read`, `ReadImage`, `Edit`, and `Write` use a validator that knows only the invocation directory, Cake-managed temporary directories, configured writable directories, `--add-dir`, and skill directories. The two layers disagree today: Bash can read a Rust standard-library file under `~/.rustup/toolchains/...` or a crate source file under `~/.cargo/registry/src/...`, while `Read` rejects the same file as outside the working directory. The model then falls back to Bash and loses `Read`'s range selection, binary detection, size limits, and line-alignment handling.

The obvious repair --- feed every Bash grant into the in-process validator --- is unsafe. Bash's built-in grants include writable integration state such as `~/.config/gh`, which stores an OAuth token, and other credential-bearing toolchain state. Bash's Linux read-execute set also includes `/proc`, `/dev`, and `/sys`, so a blanket copy would make `/proc/self/environ` readable through a tool that is auto-approved and has no rule-based secret masking yet. The grant classes also differ on the write side: Bash may write toolchain caches such as `~/.cargo`, `~/.rustup`, and `~/.npm`, while `Edit` and `Write` must treat the same paths as read-only.

A shared internal model is therefore needed, but the shared model must project different views for the OS sandbox and the in-process tools. The user-facing grant sources, trust rules, and CLI surface should not change.

## Decision Drivers

- Give `Read` and `ReadImage` the same ordinary read reach as Bash for build, toolchain, and system files, preserving their range, binary, size, and line-alignment behavior instead of forcing a Bash fallback.
- Keep `Edit` and `Write` limited to the existing writable surface: the working directory, temporary paths, and configured writable directories, subject to policy demotion.
- Deny credential-bearing paths before any ordinary grant, including when the user grants an ancestor such as `~`.
- Prevent Linux OS-only paths, especially `/proc/self/environ`, from entering the in-process read set.
- Make one grant owner authoritative so the OS sandbox, in-process validator, Bash `cwd` admission, scheduling, and diagnostics cannot drift.
- Preserve existing `--add-dir`, `directories`, `[sandbox]`, tilde-expansion, project-trust, and session behavior.
- Build the grant value once per invocation; a `Read` must not rebuild the sandbox or re-read environment overrides.
- Keep Bash's denial diagnostics honest: they describe OS grants, while the in-process denylist is an additional file-tool policy.
- Fail closed on canonicalization errors and symlinks that resolve into denied paths or outside every grant.
- Make the security boundary testable with synthetic homes, explicit allow/deny cases, parity checks, and Linux CI.

## Considered Options

1. **Keep the status quo and document the Bash fallback.** Rejected. The fallback loses `Read`'s handling and pushes ordinary source inspection through the command gate, where the model pays a stochastic policy decision for a read the OS sandbox already permits.
2. **Copy `SandboxConfig` grants verbatim into `classify_resolved_path`.** Rejected. This exposes credential-bearing integration state, makes toolchain caches writable to `Edit` and `Write`, and admits Linux `/proc`, `/dev`, and `/sys`.
3. **Maintain a second in-process grant list beside `SandboxConfig`.** Rejected. Two owners can drift, and a parity test can only detect the drift after the fact. The existing user-grant path already demonstrates that one owner is practical.
4. **One `PathGrants` owner with OS-sandbox and in-process projections, plus a credential denylist.** Chosen. It preserves one source of truth while allowing the two enforcement layers to differ only where their authority intentionally differs.

## Decision Outcome

Chosen option 4.

`PathGrants` becomes the single owner of resolved filesystem grants and is built once when `ToolContext` is constructed, after settings, trust, skill discovery, and writable-directory preparation. It carries:

- `read_write`: the working directory, Cake-managed temporary directories, configured writable directories (`settings_dirs`), and linked-worktree Git directories. Under the `read-only` policy, non-temporary entries are demoted to the in-process read-only view, matching the existing Bash demotion.
- `read_only`: built-in toolchain and system paths, `--add-dir`, `[sandbox].read_only`, skill directories, and Cake state directories. Built-in toolchain paths remain here under `workspace-write` even though Bash may write them.
- `sandbox_writable`: the built-in toolchain subset that Bash may write under `workspace-write`.
- `sandbox_only_read`: OS-only paths excluded from the in-process view, currently Linux `/proc`, `/dev`, and `/sys`.
- `denied`: credential-bearing files and directories.
- `user_grants`: the existing Bash `cwd` admission set, captured before read-only demotion.
- `policy`: the policy the value was built for.

`SandboxConfig` becomes a projection. Its OS `writable` set is `read_write + sandbox_writable`; its OS read-execute set is `read_only` minus `sandbox_writable`, plus `sandbox_only_read`. It copies `user_grants` and `policy` and keeps its existing predicates. `SandboxConfig::build` projects from the `ToolContext`'s cached grants; `build_with_policy` builds a `PathGrants` and projects it. The OS predicates do not apply the denylist: Bash continues to be governed by the OS sandbox and the external gate, and its diagnostics continue to describe what the OS profile allows.

The in-process validator canonicalizes the requested path, checks `denied` first, then `read_write`, then `read_only`. A denied directory denies its subtree; a denied file denies exactly that path. Every denied entry is stored in lexical and canonical form so a symlink from the working directory or a granted tree to a denied target fails closed, while a sibling such as `~/.npmrc.backup` remains readable. A symlink that resolves outside every grant is rejected as outside. Prospective-write resolution uses the same classifier, so a denied existing parent fails before a file is created.

`Read` and `ReadImage` accept both the read-write and read-only projections. `Edit` and `Write` accept only read-write paths, so built-in toolchain and system paths keep the existing read-only rejection. The credential-denial diagnostic names the path and the policy but never the file contents.

The initial denylist is `~/.ssh`, `~/.aws`, `~/.azure`, `~/.config/gcloud`, `~/.kube`, `~/.docker/config.json`, `~/.config/gh`, `~/.config/glab-cli`, `~/.npmrc`, `~/.yarnrc`, `~/.yarnrc.yml`, `~/.cargo/credentials`, `~/.cargo/credentials.toml`, `~/.netrc`, `~/.git-credentials`, and `~/Library/Keychains`. The deny list wins over explicit user grants in this change. It is an interim boundary until rule-based secret masking (task 041b) or a later opt-in setting can relax it. Bash can still read these paths subject to its OS sandbox and the external gate; this decision does not claim the denylist is a filesystem boundary for Bash.

Linux `/proc`, `/dev`, and `/sys` are deliberately absent from the in-process projection even though the Landlock profile grants them. macOS platform-specific capabilities and specialized rules such as the Keychain service, SSH agent socket, `lsopen`, and Apple Events remain outside `PathGrants`. On macOS, `/dev` remains in the OS read set; `/dev/null` still reaches the existing non-regular-file rejection rather than becoming a readable regular file.

The grant value is cached for the invocation. A policy change rebuilds it through `ToolContext::set_sandbox_policy`; tests that mutate path inputs after construction use an explicit refresh helper. No `Read` rebuilds the sandbox, and no environment change after construction changes the run's grants.

There are no new settings keys, CLI flags, session records, provider schemas, Cargo dependencies, or platform sandbox APIs. `--add-dir`, `directories`, `[sandbox]` merge and tilde expansion, project trust, and Bash `cwd` admission keep their existing semantics.

### Consequences

- Good, because `Read` can inspect Rust standard-library and cargo-registry sources without `--add-dir`, so the model gets `Read`'s range, binary, size, and line handling instead of falling back to Bash.
- Good, because `Edit` and `Write` cannot modify toolchain caches merely because Bash may write them.
- Good, because one owner supplies the OS profile, the in-process validator, the parity test, and the Bash grant predicates, so the layers can no longer drift silently.
- Good, because the deny list wins over an ancestor grant and the denial never echoes file contents.
- Good, because `--add-dir`, configured directories, skills, Cake state, and existing trust behavior remain compatible.
- Bad, because the deny list is incomplete and path-based. A user-granted `~` can still expose a credential file that is not listed, and Bash can still read every denied path.
- Bad, because the in-process read surface becomes larger and must be covered by allow, deny, symlink, prospective-write, parity, and platform tests; an error in the projection could widen authority.
- Bad, because `ToolContext` now owns a cached security-relevant value, so tests that mutate path fields or the process environment after construction need the refresh/rebuild path.
- Bad, because Linux-only exclusions cannot be fully exercised on a macOS development host; the Linux CI job carries part of the verification.
- Neutral, because the OS sandbox profile, external gate, and Bash behavior are unchanged; the change is in-process authorization plus the internal ownership of the grant table.
- Neutral, because the denylist is intentionally interim. Task 041b's masking work can reduce or replace it without changing the shared-owner model.

## More Information

- Related decisions: [ADR 019](019-project-customizable-sandbox-paths.md) (project-customizable sandbox paths), [ADR 023](023-two-sandbox-filesystem-path-classes.md) (two OS path classes), [ADR 032](032-bash-cwd-sandbox-grants.md) (Bash `cwd` admission and user grants), [ADR 040](040-bootstrap-writable-sandbox-grants.md) (writable grant bootstrap), [ADR 043](043-serialize-same-path-reads-with-same-path-mutations.md) (canonical path and same-path scheduling), and [ADR 053](053-trust-project-configuration-by-capability.md) (project trust).
- Task 171 owns the implementation. The ExecPlan is `docs/exec-plans/active/align-in-process-read-grants.md`.
- Implementation surfaces: `src/clients/tools/sandbox/mod.rs` (`PathGrants`, `SandboxConfig`), `src/clients/tools/mod.rs` (`ToolContext`, the validator, and the classifier), and `src/main.rs` (context construction).
- Documentation to update with implementation: `docs/security.md` (parity rule, denylist precedence, Linux exclusions, residual risk), `docs/configuration.md` (the same grant behavior for Bash and the in-process tools), `ARCHITECTURE.md` (grant ownership), and the ReadImage description.
- Verification must include synthetic-home toolchain reads, denied files nested under a granted ancestor, symlinks from the working directory and a granted tree to denied paths, prospective writes under a denied directory, file-versus-directory deny matching, a grant parity test, the cached-grant invariant, read-only policy demotion, and Linux `/proc`, `/dev`, and `/sys` rejection. No paid provider calls are required.
- The denylist is not a complete credential inventory. General credential-file discovery and masking remain in tasks 041b and 041c, and the residual risk must be stated in `docs/security.md` rather than implied away.
