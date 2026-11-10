---
status: accepted
date: 2026-11-10
decision-makers: Travis Ennis
informed: task 120
---

# Replace files through a staged atomic rename

## Context and Problem Statement

`Edit` and `Write` replaced existing files with `std::fs::write`, which opens the destination with truncation before writing the replacement. A write or sync failure after truncation can therefore leave the original file empty or partially replaced while the tool returns an error. The failure is rare, but the original bytes are unrecoverable when it happens.

The tools already validate canonical paths, grants, file type, size limits, and edit preconditions before writing. What they lack is a publication boundary: the replacement should become visible only after all preparation succeeds, and a failed preparation should leave the previous entry intact.

The replacement semantics also have to stay compatible with the current path contract. Existing write targets are canonicalized before either tool writes, so a symlink named on the command line is followed and its target is edited. `Write` creates missing parent directories. `Edit` preserves BOM and line endings and returns the same diff/output shape.

## Decision Drivers

- A failed replacement must leave the original bytes and effective permissions unchanged.
- A failed new-file replacement must not publish partial content.
- Path grants and credential denials remain the authority; the replacement helper may not widen them.
- Target changes between preparation and publication must be detected rather than silently overwritten.
- Existing `Edit` and `Write` output and validation behavior must remain stable.
- macOS and Linux are the supported verification targets; the code must still compile on other platforms.
- No new runtime dependency is justified.

## Considered Options

- Keep `std::fs::write` and rely on filesystem failure being rare. This preserves the current inode and metadata behavior, but leaves the truncation window unchanged.
- Keep an in-memory copy and rewrite the original after a failed `std::fs::write`. This does not preserve the original when the first failure is a process kill or when the second write also fails, and it still exposes a partially written file to concurrent readers.
- Write to a sibling temporary file and rename it over the target only after preparation succeeds. This makes publication atomic on one filesystem, leaves the original entry in place on every preparation failure, and costs a new inode and metadata fidelity.
- Use an external atomic-write crate. This adds a dependency for a small operation and still requires local decisions about hard links, symlinks, permissions, and target revalidation.

## Decision Outcome

Chosen option: write to a sibling temporary file and atomically rename it over the target only after preparation succeeds.

`Edit` and `Write` share one replacement operation. For an existing target it opens the target for write as a preflight, records its device, inode, size, link count, and effective permission bits, creates a hidden `.cake-replace-<uuid>` sibling, copies the existing effective `rwx` bits to the temporary file, writes the complete replacement, calls `sync_all`, re-reads and compares the staged bytes by SHA-256, revalidates the target identity and the temporary entry, and then renames the temporary file over the target. For a missing target it records absence, creates the temporary file with the process umask applied, and rechecks absence before publishing.

The operation rejects a final symlink, a non-regular target, a target with no write permission bits, and (on Unix) a target with more than one hard link. A failed preparation removes the temporary file; if cleanup also fails, the error names the residue. The stage file is synced, but the parent directory is not, so the guarantee covers preparation and publication errors, not power-loss durability.

### Consequences

- Good, because a failed replacement leaves the previous file entry, bytes, and effective mode unchanged.
- Good, because a concurrent target replacement or creation between preparation and publication is rejected instead of silently overwritten.
- Good, because the helper has a narrow injectable phase boundary for deterministic tests without process-wide limits.
- Bad, because every replacement creates a new inode. Hard links to the old file are not updated, so the operation rejects them instead of silently breaking them.
- Bad, because owner, group, extended attributes, ACLs, and other inode metadata are not preserved. Existing effective `rwx` bits are copied, but setuid and setgid bits are cleared.
- Bad, because replacing an existing file now requires write permission on its parent directory; a writable file in a read-only directory previously could be written in place.
- Bad, because a process kill during preparation can leave a `.cake-replace-*` file; no cleanup runs after `SIGKILL`.
- Neutral, because a symlink is still followed when it is resolved during path validation, but a symlink that occupies the final target at publication is rejected.

## More Information

Task 120 owns the implementation and fault-injection coverage. Task 043 owns the remaining symlink and same-path race window. Task 058 owns cancellation boundaries. The tool-facing grant and credential rules are unchanged; [Security](../security.md) records the replacement semantics.

The design follows the staged-write pattern used by fx's `file_mutation` implementation, with Cake-specific path validation and the stricter hard-link rejection described above.
