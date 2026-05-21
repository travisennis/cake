---
name: debugging-sandbox
description: Diagnose sandbox denials in cake. Use when a command fails inside the cake sandbox with `Operation not permitted (os error 1)`, when `cargo`/`flock`/`fcntl` operations mysteriously fail under cake but succeed outside, or when the user mentions sandbox profiles, Seatbelt, `sandbox-exec`, or `CAKE_SANDBOX`.
---

# Debugging Sandbox Denials

cake runs tools inside a macOS Seatbelt (`sandbox-exec`) or Linux Landlock
sandbox. When a command fails with `Operation not permitted`, use this skill
to identify which operation was denied and update the profile.

## Quick Diagnosis

```bash
# Check whether the sandbox is active
echo $CAKE_SANDBOX  # Sandboxing is on unless off/0/false/no

# Reproduce the failure with the same profile
sandbox-exec -f "$TMPDIR"/cake/sandbox_profiles/cake_sandbox_*.sb \
  bash -c "your-command-here"
```

## Trace Mode: Find What Was Denied

Create a debug profile that **logs** denials instead of (or in addition to)
blocking them.

```bash
# 1. Find the generated profile
ls -la "$TMPDIR"/cake/sandbox_profiles/

# 2. Copy it
cp "$TMPDIR"/cake/sandbox_profiles/cake_sandbox_XXXX.sb /tmp/debug_sandbox.sb

# 3. Edit /tmp/debug_sandbox.sb — replace `(deny default)` with one of:
#
#    # Still blocks, but logs every denial:
#    (deny default (with send-signal SIGKILL))
#    (trace "/tmp/sandbox_trace.log")
#
#    # Logs without blocking (use to see everything the command needs):
#    (deny default (with no-log))
#    (trace "/tmp/sandbox_trace.log")

# 4. Re-run the failing command with the debug profile
sandbox-exec -f /tmp/debug_sandbox.sb bash -c "cargo check"

# 5. Inspect denied operations
cat /tmp/sandbox_trace.log
```

## Common Missing Permissions

| Error pattern                                 | Likely cause                               | Fix                                |
| --------------------------------------------- | ------------------------------------------ | ---------------------------------- |
| `Operation not permitted` on `target/` writes | Missing `file-lock`                        | Add `(allow file-lock)` to profile |
| `/tmp` access denied despite being allowed    | Symlink mismatch (`/tmp` → `/private/tmp`) | Include both forms in profile      |
| Cargo registry download fails                 | `~/.cargo/registry` is read-only           | Add to `read_write` paths          |
| `flock` / `fcntl` failures                    | Missing `file-lock` permission             | Add `(allow file-lock)` to profile |

## Inspecting the Generated Profile

```bash
# The active profile path is logged at startup
grep "Generated sandbox profile" ~/.cache/cake/cake.*.log

# Or find the latest profile file
ls -lt "$TMPDIR"/cake/sandbox_profiles/ | head -5
cat "$TMPDIR"/cake/sandbox_profiles/cake_sandbox_*.sb
```

## Worked Example: `cargo build` Denied Inside the Sandbox

User reports: cake's `bash` tool can't run `cargo build` — fails with
`Operation not permitted (os error 1)`.

```bash
$ echo $CAKE_SANDBOX
# (empty → sandbox is on)

$ ls "$TMPDIR"/cake/sandbox_profiles/
cake_sandbox_a3f1.sb

$ cp "$TMPDIR"/cake/sandbox_profiles/cake_sandbox_a3f1.sb /tmp/debug_sandbox.sb
# Edit /tmp/debug_sandbox.sb:
#   keep `(deny default ...)` as-is, add: (trace "/tmp/sandbox_trace.log")

$ sandbox-exec -f /tmp/debug_sandbox.sb bash -c "cargo build" 2>&1 | tail -3
error: failed to acquire package cache lock
Caused by: Operation not permitted (os error 1)

$ tail -5 /tmp/sandbox_trace.log
(deny file-write-data (path "/Users/me/.cargo/registry/index/.cargo-lock"))
(deny file-write-data (path "/Users/me/.cargo/.package-cache"))
(deny file-lock (path "/Users/me/.cargo/registry/index/.cargo-lock"))
```

**Diagnosis**: `cargo` needs `file-lock` on `~/.cargo`, plus write access
to the registry path. Matches the "Cargo registry download fails" row in
the table above.

**Fix**: Update the profile template under `src/clients/tools/sandbox/`
to add `(allow file-lock)` and include `~/.cargo/registry` (and the
package-cache path) in the read-write set. Rebuild cake.

## After Fixing

Profile changes belong in the cake source (`src/clients/tools/sandbox/`),
not in the generated `$TMPDIR` copy. Once trace mode identifies the missing permission,
update the source profile template and rebuild cake.
