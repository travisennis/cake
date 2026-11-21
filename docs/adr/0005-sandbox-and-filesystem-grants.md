# 0005 --- Sandbox policy, path grants, and platform confinement

- **Status:** Accepted
- **Date:** 2026-11-20
- **Governs:** `src/clients/tools/sandbox/`, `src/config/trust.rs`, and the grant projections used by `src/clients/tools/mod.rs`

## Context

Sandboxing is default-on and fails closed. The same resolved grant set has two projections: one for the operating system (Seatbelt on macOS, Landlock on Linux) and one for in-process file tools. Hooks, toolbox executables, and MCP servers are trusted control-plane extensions outside the sandbox. A change here can silently turn a denial into an allowance, so the platform-specific reasoning is recorded rather than inferred.

## Decisions

### Policy and grants

- `PathGrants` is the single owner of resolved filesystem grants. It projects one view for the OS sandbox and one for the file tools. Read and ReadImage accept read-write and read-only grants; Edit and Write accept only read-write grants. A credential denylist wins over both.
- The OS projection lets Bash write built-in toolchain caches; the in-process projection keeps those paths read-only and excludes OS-only paths.
- On unsupported platforms no strategy reads the configuration fields. The constructor still reads them explicitly to keep the fields live rather than relying on `#[expect(dead_code)]`, which is reserved for serde fields.
- Linux detection is infallible. macOS and unsupported platforms can fail closed, and every failure is reported rather than silently running unsandboxed.
- `DangerFullAccess` applies no sandbox. Keep its generated-profile representation writable so the helper retains explicit policy semantics when inspected independently of the Bash path.

### Shared writable and read-and-execute state

The sandbox grants write access to the caches a command needs to run rather than to the whole home directory. The shared list is the only owner; each platform strategy translates it into its own rule form. It covers:

- Rust: cargo and rustup, honoring `CARGO_HOME` and `RUSTUP_HOME`.
- Pre-commit hook caches.
- SCM CLIs (`gh`, `glab`) and their configuration.
- Cross-language runtime managers: mise, volta, asdf, proto, pkgx.
- Node.js: version managers, npm, configstore, node-gyp, pnpm, yarn classic and modern, corepack, turborepo, browser automation and test runners.
- Bun and Deno.
- Go, honoring its environment overrides.
- Java and JVM toolchains: Maven, Gradle, SBT, Coursier, jenv, sdkman.
- Python: uv, pip, pipx, poetry, pdm, conda, hatch, ruff, mypy, jupyter, pyenv, and related tools.
- Ruby: rbenv, rvm, gem, bundler.
- Perl: perlbrew, plenv, cpan, cpanm, local::lib.
- PHP and Composer.
- macOS `~/Library` application-support and cache locations, including the sccache disk cache.

A tool whose caches are missing under the sandbox is a grant-list gap; adding the cache to this list is the intended fix.

### macOS Seatbelt

- Profile applicability is a property of the process context. An already-sandboxed cake process cannot become unsandboxed later in the same run. Cache the probe so every Bash command does not pay for a `sandbox-exec` subprocess before doing real work.
- Grant the `.ssh` directory itself so tools can list it; this does not grant key read access.
- Conventional agent socket layouts are end-anchored and require the agent socket file name. A sibling file in the same directory, including one that shares the directory prefix, stays denied. Launchd-managed `ssh-agent` uses `/private/tmp`.
- A command reaches its agent only through the socket named by `SSH_AUTH_SOCK`, such as `~/.ssh/agent/s.XXXX` or `~/.1password/agent.sock`. Grant the socket in both the spelling the variable uses and its resolved form, plus traversal for its parent.
- The profile grants process execution, same-sandbox process introspection, Mach services required by dyld, DNS, and system frameworks, sysctl reads, the system socket needed by the network stack, network access (Seatbelt confines the filesystem, not the network), the root literal for dyld traversal, and ancestor directory literals for every writable and read-and-execute path so `readdir` and `stat` can traverse them.
- Read-write access covers the working directory and temp directories. Read-and-execute access covers system paths, configured read-only paths, skill paths, and writable paths demoted by read-only policy; the global process-exec rule supplies execution authority.
- File ioctl is allowed only for terminal devices. File locking is allowed because cargo and other build tools require it.
- `Command` has no API to transfer a configured environment to a replacement command, so rebuilding the command for `sandbox-exec` must reproduce every explicit set and removal, or values such as `AI_AGENT=cake` are dropped. Stdio configuration is re-applied for the same reason.
- Write the profile to a temp file and keep it alive through spawn via `SandboxGuard`, then clean it up deterministically when the guard drops after execution.

### Linux Landlock

- Resolve path existence and open the rule file descriptors before `fork`. The resulting `RulesetCreated` owns everything the child needs.
- Add read-write rules for the working directory and temp directories.
- Add read-and-execute rules for system paths, configured read-only paths, skill paths, and writable paths demoted by read-only policy. Execute is included so scripts and binaries run while Landlock still denies mutation, matching Seatbelt's `file-read*` plus global `process-exec` rules.
- The post-fork closure only moves the inherited ruleset, performs `no_new_privs` and `restrict_self`, maps failures to errno-backed errors, and closes the ruleset file descriptor. It neither allocates nor acquires process-global locks, which is what makes the unsafe closure sound.

### Linked worktrees

- A `.git` file marks a linked worktree or submodule and contains `gitdir: <path>`. Parse the marker, trim whitespace, and resolve a relative path against the `.git` file's directory.
- Canonicalize the gitdir to resolve symlinks and `..` components.
- Determine the common git directory from `commondir` when present, otherwise from the per-worktree path structure. `commondir` is relative to the per-worktree gitdir.
- A worktree's own directory is a generated checkout, never project configuration. Trust hashing skips it because hashing would be slow and would make trust depend on unrelated working trees.
- A missing project directory has no configuration to trust; a caller may probe a path before it exists.

## Consequences

Adding a runtime, a cache, or an agent socket means extending the shared list or the platform profile with a test of the actual command under the sandbox. The platform files cannot be validated by review alone; `docs/security.md` defines the guarantees and the verification expected for a change here.
