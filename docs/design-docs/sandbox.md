# Filesystem Sandbox

Cake sandboxes commands executed by the Bash tool to restrict filesystem access. This prevents LLM-generated commands from reading or writing files outside the project directory and essential system paths.

## Overview

When the Bash tool executes a command, cake wraps it in an OS-level sandbox that enforces a deny-default filesystem policy. Only explicitly allowed paths are accessible:

  | Access Level            | Paths                                                                                                                                                                                                                                                            | Purpose                                                                                                                                                                                               |
  | ----------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
  | **Read-write**          | Current working directory, temp directories, `~/.cargo`, `~/.rustup`, `~/.cache/sccache`, `~/.config/gh`, `~/.config/glab-cli`, `~/.config/mise`, `~/.asdf`, `~/.volta`, and related cache/state directories                                                     | Project files, build artifacts, toolchain caches, SCM CLI configs                                                                                                                                     |
  | **Read-only + execute** | `/usr`, `/bin`, `/sbin`, system paths, `/Library`, `/System/Library`, `/Applications`, `/opt/homebrew`, `/opt/local` (macOS); `/usr`, `/bin`, `/sbin`, `/lib`, `/lib64`, `/etc/alternatives`, `/snap` (Linux)                                                    | Running system tools and compilers                                                                                                                                                                    |
  | **Read-only**           | `/etc`, `/dev`, `/var`, `/proc`, `/sys` (Linux); `/etc`, `/private/etc`, `/private/var`, `/dev`, `/var` (macOS); `~/.config/git`, `~/.gitattributes`; **plus any directories added via `--add-dir`**; **plus skill directories (parent dirs of SKILL.md files)** | Configuration, device access, git config, user-specified reference directories, skill scripts. Read-only paths still allow executing scripts/binaries on both platforms (execution is not a mutation) |
  | **Denied**              | Everything else                                                                                                                                                                                                                                                  | Home directory (except allowed paths), other projects, etc.                                                                                                                                           |

## Platform Support

### macOS --- sandbox-exec (Seatbelt)

On macOS, cake uses `sandbox-exec` with a dynamically generated [Seatbelt profile](https://reverse.put.as/wp-content/uploads/2011/09/Apple-Sandbox-Guide-v1.0.pdf). The profile uses a deny-default policy and explicitly allows:

- **Filesystem**: read-write for cwd/temp/toolchain/SCM/runtime paths, read-only+exec for system paths, read-only for config/device paths
- **Process**: `process-fork`, `process-exec` (needed for bash and subcommands)
- **IPC**: `mach-lookup` (needed for dyld, DNS, system frameworks)
- **Signals**: allowed (needed for process management)
- **Network**: fully allowed (the sandbox restricts filesystem only, not network)
- **Devices**: `/dev` (read-only access to device files)
- **System**: `sysctl-read`, `file-ioctl` (needed for terminal operations)

Sandbox profiles and the runtime applicability probe are written under the
same secure per-process directory used for Bash output artifacts. On Unix,
the directory is `<temp_dir>/cake-<uid>-<random>/`, has `0o700` permissions,
and is revalidated before each use so profile creation fails closed if its
ownership, type, or permissions cannot be secured.

Requires `/usr/bin/sandbox-exec` (present on all standard macOS installations) and a process context where macOS allows `sandbox-exec` to apply a Seatbelt profile. Cake probes this at runtime. If the probe reports `sandbox_apply: Operation not permitted`, cake recognizes that the process is already constrained by an outer Seatbelt sandbox, emits a warning, and runs Bash without applying its own nested profile. The inherited sandbox is then the effective enforcement boundary and may not match cake's selected path policy exactly. All other probe failures fail closed rather than running without filesystem sandbox enforcement.

### Linux --- Landlock LSM

On Linux, cake uses [Landlock](https://landlock.io/), a Linux Security Module available since kernel 5.13. Landlock allows unprivileged processes to sandbox themselves without root access.

The Landlock sandbox is applied via `pre_exec`, so rules take effect in the child process after `fork()` but before `exec()`.

All Linux builds include Landlock support by default. No special feature flag is needed --- `cargo build --release` on Linux automatically compiles with Landlock.

With sandboxing enabled, Linux fails closed if Landlock reports anything less than a fully enforced ruleset. Use `CAKE_SANDBOX=off` as the explicit opt-out for unsandboxed Bash execution.

System paths on Linux include `/usr`, `/bin`, `/sbin`, `/lib`, `/lib64`, `/etc/alternatives`, and `/snap`.

## Layered Defense

The sandbox provides OS-level filesystem restriction as the primary enforcement mechanism. In addition, the Bash tool includes a narrow pre-execution destructive command guard that blocks known-destructive commands (e.g., `git reset --hard`, `git push --force`, `rm -rf` outside literal `/tmp` or `/var/tmp` targets) before they reach the shell. This best-effort guard complements the sandbox by catching destructive operations that are technically allowed within the sandbox's permitted zones---for example, destructive git operations inside the repository directory. It is not a shell security policy engine. See [tools.md](./tools.md) for the blocked categories; the authoritative rule set lives in `clients::tools::bash_safety` and its tests.

## Configuration

### Sandbox Policies

Use the `--sandbox` / `-s` CLI flag to select the filesystem sandbox policy applied to model-generated shell commands:

  | Value                | Behavior                                                                                                                                                                                                                                                                                                                                                                                                                            |
  | -------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
  | `read-only`          | Most restrictive. Grants read access to the workspace directory, system paths, and config paths, but denies writes to the workspace and toolchain caches. Temp directories stay read-write so commands can produce intermediate output. The Edit and Write tools are removed from the agent's tool set (they mutate files in-process, outside the OS sandbox that wraps Bash), so the no-mutation guarantee covers the whole agent. |
  | `workspace-write`    | The default. Read-write access to the project directory, temp directories, and toolchain caches; read-only access to system and config paths. Equivalent to the historical sandbox behavior.                                                                                                                                                                                                                                        |
  | `danger-full-access` | No sandbox restrictions. Bash commands run with full filesystem access.                                                                                                                                                                                                                                                                                                                                                             |

```bash
cake --sandbox read-only "Audit this repo for secrets"
cake -s read-only "Review the diff"
cake --sandbox danger-full-access "Run setup"
```

The default (no `--sandbox` flag) is `workspace-write`, so existing behavior is unchanged.

Under `read-only`, persistent read-write directories declared in `settings.toml` are also demoted to read-only: the policy denies all mutations, so it overrides the per-project write grants those settings normally provide.

### Toolbox Trust Boundary

User-defined toolbox executables are not wrapped in the Bash tool's Seatbelt or Landlock sandbox. Configuring `CAKE_TOOLBOX` or `--toolbox` therefore grants those executables the user's ambient filesystem and network authority under `workspace-write` and `danger-full-access`.

Under `read-only`, cake does not scan or describe toolbox executables and registers no `tb__*` tools. The describe action executes user code too, so merely suppressing execute calls would not preserve the policy's no-mutation guarantee. See [ADR-017](../adr/017-trusted-executable-toolbox-tools.md) and [tools.md](./tools.md#toolbox-tools).

The `CAKE_SANDBOX` environment variable is still supported for backward compatibility: when no `--sandbox` flag is passed, `CAKE_SANDBOX=off` (and its aliases) maps to `danger-full-access`. When `--sandbox` is provided, the CLI flag takes precedence over the environment variable.

### Disabling the Sandbox (legacy CAKE_SANDBOX)

Set the `CAKE_SANDBOX` environment variable to disable sandboxing:

```bash
# Any of these values disable the sandbox
export CAKE_SANDBOX=off
export CAKE_SANDBOX=0
export CAKE_SANDBOX=false
export CAKE_SANDBOX=no
```

When disabled, a warning is logged and all commands run with full filesystem access. This is equivalent to `--sandbox danger-full-access`.

The `warn` value is recognized but currently falls back to enforce mode.

### Adding Read-Only Directories (--add-dir)

Use the `--add-dir` CLI flag to grant the agent read-only access to directories outside the project directory:

```bash
# Add a single directory
cake --add-dir /path/to/reference/docs "Use the documentation in /path/to/reference/docs"

# Add multiple directories
cake --add-dir ~/Documents/specs --add-dir ~/Projects/shared-utils "Analyze the code"
```

**Key points:**

- Directories are added as **read-only** --- the agent cannot write to them
- The flag can be repeated to add multiple directories
- Invalid or non-existent directories are logged as warnings and ignored
- Both the original path and its canonical (symlink-resolved) path are added to ensure access

This is useful when you want the agent to: - Reference documentation or specifications stored elsewhere - Read shared utility code from another project - Access configuration files or templates

### Persistent Read-Write Directories (settings.toml)

Use the `directories` key in `settings.toml` to declare directories that cake can read from and write to. Unlike `--add-dir` which grants read-only access, directories listed here get full read-write access. This is useful for configuring persistent workspace directories.

**Global settings** (`~/.config/cake/settings.toml`):

```toml
directories = ["~/Projects", "~/Documents/notes"]
```

**Project settings** (`.cake/settings.toml`):

```toml
directories = ["../shared-libs", "/data/exports"]
```

**Key points:**

- Directories are added as **read-write** --- the agent can create, modify, and delete files
- Lists from global and project settings are **merged** (union with deduplication)
- Non-existent directories are logged as warnings and ignored
- Both the original path and its canonical (symlink-resolved) path are added to the sandbox

### Additional Read-Write Paths

Beyond the workspace, the sandbox automatically grants read-write access to these categories of paths:

- The current working directory (and its subtree)
- System temp directories (`$TMPDIR`, `/tmp`, `/var/tmp`)
- Rust toolchain and build caches (cargo, rustup, sccache), honoring `$CARGO_HOME`/`$RUSTUP_HOME`
- SCM CLI config/cache/state directories (`gh`, `glab`)
- AI coding assistant CLI directories (`codex`)
- Runtime manager directories (`mise`, `asdf`, `volta`)
- Linked git worktree directories: when the workspace is a linked git worktree (`.git` is a file with a `gitdir:` pointer), the per-worktree gitdir and the common git directory are automatically resolved and added as read-write so that git commands (status, add, commit, etc.) can operate under the sandbox. If the gitdir cannot be resolved, a warning is logged at session start.

The authoritative per-directory list is `SandboxConfig` in `clients::tools::sandbox`, pinned by its tests; extend it there rather than here when adding tool support. All read-write paths are canonicalized (symlinks resolved) before being added to the sandbox policy.

## Examples

```bash
# This works — reading files in the project directory
cake "List the files in this project"
# Bash tool runs: ls -la  ✓

# This is blocked — writing outside the project directory
# Bash tool runs: touch /tmp/cake_test  ✗ (Operation not permitted)

# This is blocked — reading the user's home directory
# Bash tool runs: ls ~/Desktop  ✗ (Operation not permitted)

# This works — running system tools
# Bash tool runs: git status  ✓
# Bash tool runs: cargo build  ✓
```

## Troubleshooting

### Command fails with "Operation not permitted"

The sandbox is blocking access to a path outside the allowed set. Options:

1. Ensure you're running cake from the correct project directory
2. If the command legitimately needs broader access, disable the sandbox with `--sandbox danger-full-access` (or `CAKE_SANDBOX=off`)

### "sandbox-exec not found" error (macOS)

The `sandbox-exec` binary is missing from `/usr/bin/`. This is unusual on standard macOS installations. Bash commands fail closed unless sandboxing is explicitly disabled with `--sandbox danger-full-access` (or `CAKE_SANDBOX=off`).

### "sandbox-exec cannot apply profiles" error (macOS)

The `sandbox-exec` binary exists, but macOS rejected applying a test Seatbelt profile in this process context. When the rejection is `sandbox_apply: Operation not permitted`, cake treats it as nested Seatbelt enforcement: Bash commands continue under the outer sandbox and cake logs that its own profile was not applied. This lets cake run as a subagent of tools that already use Seatbelt, but the outer tool's policy controls the effective filesystem access, including during cake `read-only` sessions.

Any other profile-probe failure still blocks Bash commands. Run cake from a normal terminal to preserve cake's own sandbox enforcement, or use `--sandbox danger-full-access` (or `CAKE_SANDBOX=off`) only when intentionally running without it.

### "Landlock not enforced" error (Linux)

Landlock requires kernel 5.13 or later. On older kernels, Landlock reports `NotEnforced` status and Bash commands fail closed unless sandboxing is explicitly disabled with `--sandbox danger-full-access` (or `CAKE_SANDBOX=off`). Cake also fails closed when Landlock reports `PartiallyEnforced`, because the filesystem sandbox is treated as unavailable unless the ruleset is fully enforced. Check your kernel version with `uname -r`.

### SSH git operations fail with host key verification

On macOS, the sandbox grants read-only access to `~/.ssh/known_hosts` and `~/.ssh/config` so the sandboxed process cannot modify them, and read-write access to SSH agent sockets under `/tmp/ssh-*` and `/private/tmp/ssh-*` so agent-based authentication works. On Linux the Landlock sandbox does not add SSH-specific path rules; use `--add-dir ~/.ssh` to grant read-only access to the SSH directory for host key verification and config lookup.

If you use SSH for git operations (e.g., `git clone git@github.com:...`), you need to populate `known_hosts` before running cake. Choose one of the following approaches:

**Option 1: Pre-populate known_hosts with ssh-keyscan**

Run once to fetch host keys for common providers:

```bash
ssh-keyscan -t ed25519,rsa github.com gitlab.com bitbucket.org >> ~/.ssh/known_hosts
```

Add any self-hosted or additional git servers the same way:

```bash
ssh-keyscan -t ed25519,rsa your-git-server.example.com >> ~/.ssh/known_hosts
```

**Option 2: Use StrictHostKeyChecking accept-new**

Add to `~/.ssh/config`:

```
Host github.com gitlab.com bitbucket.org
    StrictHostKeyChecking accept-new
```

This auto-accepts host keys on first connection, then pins them for future connections. Works for any host you add. This is the more flexible option since it handles new hosts without manual pre-population.
