---
status: accepted
date: 2026-10-08
decision-makers: Travis Ennis
informed: issue 93
---

# Bootstrap Writable Sandbox Grants

## Context and Problem Statement

Cake discards configured writable directory grants whose targets do not exist. Tools cannot bootstrap state or cache directories outside the workspace until the user creates them manually. Retaining the absent path is insufficient on Linux: Landlock rules attach to existing filesystem objects, and Cake filters absent paths before preparing those rules.

## Decision Outcome

During agent-run initialization, after resolving the effective sandbox policy, create missing `directories` and `[sandbox].writable` targets, including missing parents, for writable policies. This includes `danger-full-access`. Prepare the directories before constructing the shared tool context. Settings loading, help, and diagnostic commands do not perform this preparation.

Under `read-only`, never create configured grant directories. Existing writable targets remain eligible for demotion to read-only; missing targets retain the existing warning-and-ignore behavior. Missing `[sandbox].read_only` targets also remain ignored with a file-only warning.

Existing nondirectory writable targets retain the existing warning-and-ignore behavior. Directory creation failures abort initialization before tools run, with the setting, target path, and underlying filesystem error. Successful automatic creation emits a file-only warning to retain a diagnostic for typos.

Only the configured target becomes a writable grant. Creating intermediate parents does not grant those parents or their other descendants. Resolve paths with filesystem symlink semantics, reject invalid targets, and pass the same prepared grants to the OS sandbox and in-process validation.

This refines ADR 019's decision to ignore nonexistent configured paths; its trusted-settings model and grant classes remain in effect.

## Alternatives Considered

Retaining absent grants avoids initialization side effects but cannot provide the same bootstrap behavior on Linux. Granting the closest existing ancestor would authorize unrelated siblings and is rejected. Requiring manual directory creation preserves the existing behavior but leaves the first-run trap.

## Security and Compatibility Consequences

Trusted settings now authorize directory provisioning before the tool sandbox is applied, including intermediate parents. A misspelled path can create stray directories. This is an accepted side effect, logged without changing stdout or stderr protocols. Partial creation can remain after a filesystem error; Cake does not remove directories that might now contain user data.

The review must cover read-only mutation, existing files and dangling symlinks, relative paths and symlinked ancestors, parent/sibling grant widening, initialization failure, and disagreement between OS and in-process enforcement. Filesystem replacement races remain part of the existing path-based grant model; this change does not claim to pin configuration paths permanently.

The settings schema, union merge, CLI policy precedence, and grant extent remain unchanged. Platform tests must show that provisioned targets are writable while ungranted siblings stay denied on both Seatbelt and Landlock.
