# Debugging Sandbox Denials

Use this runbook when a command works outside Cake but fails in the Bash tool with `Operation not permitted`, `Permission denied`, `os error 1`, or an explicit sandbox-initialization error. Cake uses macOS Seatbelt and Linux Landlock differently, so identify the platform before tracing or changing anything.

This procedure diagnoses filesystem enforcement. Cake's sandbox does not restrict network access, and command-policy rejections happen before the operating-system sandbox. See [Security and trust boundaries](../security.md) for the boundary and its limitations.

## Establish the Failure

1. Record the operating system, Cake invocation, working directory, selected `--sandbox` policy, `--add-dir` arguments, configured `directories`, failing command, complete stderr, and exit status.

2. Check whether an explicit CLI policy or `CAKE_SANDBOX` changed enforcement:

   ```bash
   printf 'CAKE_SANDBOX=%s\n' "${CAKE_SANDBOX-<unset>}"
   ```

   An explicit `--sandbox` value wins. Without one, `CAKE_SANDBOX=off`, `0`, `false`, or `no` selects `danger-full-access`; other values leave sandboxing enabled.

3. Re-run the same command from the same working directory with the same policy and path grants. Set `RUST_LOG=cake=debug` on the Cake process when logs are needed; Cake writes them under `~/.cache/cake/`, or the `CAKE_DATA_DIR` equivalent.

4. Classify the result:

   - A command that also fails outside Cake is not a Cake sandbox denial.
   - A Cake message saying the sandbox is unavailable is an initialization failure, not a missing path grant.
   - A command that succeeds with `--sandbox danger-full-access` but fails with the original policy is consistent with sandbox enforcement. Use this only as a diagnostic comparison, with the user's approval when the command would gain material authority; it is not the repair.

5. Continue with the branch for the host platform.

## macOS: Seatbelt

Cake generates a deny-default Seatbelt profile, writes it to a temporary `.sb` file, and invokes `/usr/bin/sandbox-exec -f <profile>`. The temporary file is removed when the command guard is dropped, but debug logs include both the generated profile and its temporary path.

### Distinguish Initialization from a Command Denial

If stderr contains `sandbox-exec: sandbox_apply`, the requested command did not run. Missing `sandbox-exec`, malformed profiles, spawn errors, and unrecognized probe failures fail closed.

The recognized [nested-Seatbelt fallback](../adr/016-nested-seatbelt-sandbox-fallback.md) is different: when Cake's startup probe reports `sandbox_apply: Operation not permitted`, Cake warns, skips its child profile, and relies on the inherited parent Seatbelt sandbox. Diagnose path denials against that parent policy; do not weaken Cake's profile to address them.

### Capture and Trace the Profile

1. Reproduce with `RUST_LOG=cake=debug` and locate the log entry beginning `Generated sandbox profile:`. Extract the complete profile text that follows the entry and write it to `/tmp/cake-debug.sb`. The separately logged temporary path normally disappears when the command finishes, so copy that file only while the command is still running.

   ```bash
   # Paste only the logged profile body into this temporary file.
   ${EDITOR:-vi} /tmp/cake-debug.sb
   ```

2. Add a Seatbelt trace destination while preserving the profile's deny-default behavior:

   ```scheme
   (deny default)
   (trace "/tmp/cake-sandbox-trace.log")
   ```

3. Replay the exact failing command:

   ```bash
   /usr/bin/sandbox-exec -f /tmp/cake-debug.sb \
     /bin/bash -c 'your-command-here'
   ```

4. Inspect `/tmp/cake-sandbox-trace.log` and identify the denied operation and canonical path. Pay attention to symlink pairs such as `/tmp` and `/private/tmp`.

Do not turn the debug profile into allow-default or treat a temporary edit as the fix. Keep tracing local to the diagnostic reproduction.

### Repair Seatbelt Enforcement

- Path grants shared by both platforms originate in `SandboxConfig` in `src/clients/tools/sandbox/mod.rs`. Add the narrowest justified readable or writable path there.
- Seatbelt-only operations and rules belong in `src/clients/tools/sandbox/macos.rs`. For example, file locking is a Seatbelt operation and is granted there with `(allow file-lock)`.
- Include original and canonical path forms where symlink resolution can change the pathname seen by Seatbelt.
- Add a focused profile-generation or enforcement test for the denied operation, then rebuild and repeat the original Cake command under the original policy.

## Linux: Landlock

Cake does not generate a profile file on Linux. In the child process immediately before `exec`, it creates a Landlock ABI v5 ruleset, adds path-beneath rules, calls `restrict_self`, and requires `FullyEnforced`. There is no Cake-managed Landlock denial log comparable to a Seatbelt trace.

Cake grants:

- all filesystem access classes supported by ABI v5 beneath writable paths;
- read, directory-read, and execute beneath system and readable paths; and
- none of the handled ABI v5 filesystem rights beneath paths outside those rules.

Only paths that exist while the ruleset is built receive a rule.

### Distinguish Initialization from a Command Denial

Messages beginning `Linux sandbox unavailable` or `Failed to configure`, `Failed to create`, `Failed to add`, or `Failed to restrict` identify Landlock setup or enforcement failures. Cake fails closed when the kernel does not fully enforce the requested ruleset. Confirm kernel and Landlock support; do not convert partial or missing enforcement into a silent fallback.

A requested command that starts and then receives `Permission denied`, `Operation not permitted`, or the corresponding OS error needs pathname diagnosis instead.

### Find the Denied Path

1. Reduce the failing command to the smallest read, write, create, rename, remove, or execute operation that still fails under the same Cake working directory and sandbox policy.

2. Inspect the command's own verbose output first. Many package managers print the cache, lock, registry, or configuration path they could not access.

3. If the pathname remains unclear, run the failing command through the host's tracing facilities *as the Cake Bash command* under the same working directory, policy, and grants; for example, use `strace -f -e trace=%file -o /tmp/cake-landlock.strace your-command-here`. A trace of an outside-Cake reproduction has no Cake Landlock rules and cannot reveal their denial. Do not look for an `.sb` file or a Landlock profile trace; neither exists.

4. Canonicalize the candidate path and compare it with the lists built by `SandboxConfig`:

   - working directory, Cake temporary directories, linked-worktree Git directories, toolchain paths, and configured `directories` are writable under `workspace-write`;
   - `--add-dir`, skill directories, and platform system/config paths are readable and executable;
   - under `read-only`, Cake temporary directories remain writable while paths otherwise writable become readable and executable.

5. Check whether the candidate existed when Cake built the ruleset. A rule for a missing path is skipped, so granting only a not-yet-created leaf may not work; grant an appropriate existing ancestor when the intended authority permits it.

Landlock does not mediate file locking as a separate access class. An `flock`/`fcntl` error is not evidence for adding a Seatbelt-style `file-lock` rule on Linux; first identify the file path and the underlying filesystem operation.

### Repair Landlock Enforcement

- For a missing path grant that should apply on both platforms, change `SandboxConfig` in `src/clients/tools/sandbox/mod.rs`.
- For a Linux system path or a Landlock-specific access class, change `src/clients/tools/sandbox/linux.rs`.
- Grant the narrowest existing ancestor and the minimum read/write authority required by the documented sandbox policy. Do not use `danger-full-access` as the permanent fix.
- Add focused configuration tests and, when Linux behavior changes, a Linux enforcement test. Rebuild on Linux and repeat the original Cake command under the original policy.

## Interpret Common Failures

  | Symptom                               | Platform-specific interpretation                                                                                                                     |
  | ------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
  | Write beneath `target/` is denied     | Confirm the working directory and policy. The workspace is writable only under `workspace-write`.                                                    |
  | Cargo cache or registry access fails  | Identify the effective `CARGO_HOME` and denied path. Cake's shared configuration grants the resolved Cargo and Rustup homes under `workspace-write`. |
  | `/tmp` access is denied on macOS      | Compare `/tmp` with canonical `/private/tmp` and ensure both forms are represented.                                                                  |
  | `flock` or `fcntl` fails on macOS     | Trace for a denied `file-lock` or path operation; Seatbelt grants `file-lock` separately.                                                            |
  | `flock` or `fcntl` fails on Linux     | Trace the accessed file and ordinary filesystem operation; Landlock has no separate lock permission.                                                 |
  | Landlock is partially or not enforced | Treat it as sandbox unavailability and fail closed; verify kernel support rather than widening paths.                                                |

## Verify and Record

After a source repair:

1. Run the focused configuration, profile-generation, or enforcement tests for the changed branch.
2. Run the repository's normal code-change gate and any required platform-specific check.
3. Repeat the original command from the original working directory with the original sandbox policy and grants.
4. Add an adjacent deny check showing that an unrelated path remains blocked.
5. Record the platform, denied operation and path, source rule changed, checks, and any platform verification that could not run.
