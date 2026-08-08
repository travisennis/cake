---
status: accepted
date: 2026-08-08
decision-makers: Travis Ennis
informed: issue 71
---

# Project-Customizable Sandbox Paths

## Context and Problem Statement

Cake's Bash tool runs model-generated shell commands inside an OS filesystem sandbox (Seatbelt on macOS, Landlock on Linux) with a deny-default policy: everything is denied unless explicitly allowed. Today the allowed set comes from hard-coded toolchain paths in `SandboxConfig::extend_with_toolchain_paths` (covering cargo, npm, gh, codex, and dozens of other ecosystems), plus `--add-dir` (one invocation, read-only) and `directories` in settings (persistent, read-write).

Projects sometimes need to run other AI CLIs or custom binaries from outside the default sandbox paths. Today `~/.local/bin/claude` is blocked, and `~/.codex` paths are hard-coded in the source. There is no settings-driven way for a project (or a user's global settings) to declare which extra paths the sandbox should allow, and no way to grant read-only access to a specific executable file rather than an entire directory subtree.

## Decision Drivers

- **Customizability**: Projects must be able to declare extra sandbox paths through `settings.toml`, without recompiling cake or inventing `--add-dir` flags.
- **Least privilege**: A read-only grant should be able to name a single executable file (e.g. `~/.local/bin/claude`) without granting access to its whole directory.
- **Consistency with `directories`**: Merge semantics should match the existing `directories` behavior (union across global settings, project settings, and the selected profile).
- **Fails closed**: A misconfigured or nonexistent path must never widen access; at most it is ignored with a log warning.
- **Codex migration path**: Hard-coded codex paths must be removable without breaking codex users, by moving those grants into user settings.

## Considered Options

- **Option A: Structured `[sandbox]` settings section with `read_only` and `writable` path lists.** `read_only` grants read + execute; `writable` grants read + write + execute. Entries may be files or directories. Merged as a union across global, project, and profile settings.
- **Option B: Reuse a single flat list (extend `directories` with a read-only variant).** A `directories_ro`-style flat list would work but cannot express the two access classes with one schema and would split the "sandbox path grants" concept across unrelated keys.
- **Option C: Settings-file-only with no in-process `validate_path` integration.** Declaring sandbox paths without feeding the Read/Edit/Write/Grep in-process validation would let Bash (OS-sandboxed) and the file tools (in-process) disagree about what is allowed.

## Decision Outcome

Chosen option: **Option A**, because it gives projects a single, explicit `[sandbox]` section with the two access classes they need, matches the existing `directories` union merge so precedence is unsurprising, and feeds the same path lists into both the OS sandbox and the in-process `validate_path` checks so the two enforcement layers cannot diverge.

### Consequences

- Good, because a project can declare `[sandbox].read_only = ["~/.local/bin/claude"]` and run `claude` from sandboxed Bash while sibling binaries in `~/.local/bin` stay denied.
- Good, because a project can declare `[sandbox].writable = ["~/.claude", "~/.cache/claude"]` so Claude Code can write its state.
- Good, because `~` is expanded in `[sandbox]` keys and in the existing `directories` key (fixing a latent silent-ignore bug where `directories = ["~/foo"]` never matched).
- Good, because `--sandbox read-only` demotes `[sandbox].writable` paths to read-only, matching the existing `directories` demotion.
- Neutral: nonexistent paths are ignored with a file-only log warning; user-visible presentation of ignored grants is deferred to a later task.
- Security: project-level `.cake/settings.toml` grants are fully trusted by design, the same trust model as `.cake/hooks.json` and the rest of project `.cake/` configuration. There is no deny-list and no trust prompt in this change; the accepted trust model is documented in `docs/security.md`.
- Breaking change: hard-coded codex grants are removed from the source in a separate commit after the `[sandbox]` mechanism lands. Users who relied on codex sandbox access without configuring settings must add the paths once to `~/.config/cake/settings.toml`; the migration is documented.

## More Information

- Issue 71: Add project-customizable sandbox paths via `[sandbox]` settings.
- ExecPlan: `docs/exec-plans/completed/project-customizable-sandbox-paths.md`.
- `docs/configuration.md` (Settings: `[sandbox]`).
- `docs/security.md` (Trust boundary and sandbox grants).
- `src/config/settings.rs` --- `SandboxSettings`, merge and tilde expansion.
- `src/clients/tools/sandbox/mod.rs` --- `SandboxConfig::build_with_policy`.
- `src/clients/tools/sandbox/macos.rs` --- file-vs-directory Seatbelt rules.
- `src/clients/tools/sandbox/linux.rs` --- `path_beneath_rules` file handling.
- `src/clients/tools/mod.rs` --- `validate_path_with_dirs`.
