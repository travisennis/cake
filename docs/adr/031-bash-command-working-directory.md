---
status: accepted
date: 2026-09-17
decision-makers: Travis Ennis
informed: issue 575
---

# Give Bash Calls An Explicit Working Directory

## Context and Problem Statement

Cake's model-facing Bash tool starts every command in the invocation working directory. An agent that needs to inspect or build a project subdirectory must put `cd` into each command, which makes the effective directory implicit and needlessly couples directory selection to shell syntax. The tool already passes its working directory to the child process and to the command-safety judge, so the missing capability is an explicit, validated argument rather than a new execution mechanism.

The argument must not let an untrusted model use directory selection to escape the filesystem authority granted to the run. Existing sandbox rules and command diagnostics must continue to describe the same effective directory.

## Decision Drivers

- Make a command's working directory explicit in the provider-facing tool call.
- Preserve the current behavior when the argument is omitted.
- Resolve relative paths predictably from the invocation working directory.
- Reject missing, non-directory, and outside-workspace paths before judging or spawning.
- Keep the command-safety judge, repository digest, child process, and denial diagnostics aligned on one effective path.
- Keep macOS Seatbelt and Linux Landlock authority unchanged.

## Considered Options

- **Require agents to embed `cd` in `command` (status quo).** Rejected because the effective directory is hidden in shell text and must be repeated in every call.
- **Accept any host directory supplied by the model.** Rejected because a convenience argument must not widen the model's filesystem authority under the default sandbox.
- **Add an optional `cwd` string resolved from and constrained to the invocation workspace (chosen).** This gives agents explicit subdirectory execution while preserving the existing sandbox root and keeping the omitted form fully compatible.
- **Make `cwd` persistent across later calls.** Rejected because tool calls are independently scheduled and persisted; hidden mutable shell state would make later commands' behavior depend on an earlier call.

## Decision Outcome

Chosen option: the Bash schema gains an optional `cwd` string. When present, a relative value is joined to the invocation working directory and an absolute value is checked directly. The resolved path is canonicalized, must exist as a directory, and must remain within the canonical invocation working directory. The canonical path is used for the child process, command-safety judge request, repository digest, and sandbox-denial diagnostics. The raw requested argument remains in the tool call transcript. When absent, Cake uses the existing invocation working directory exactly as before.

The selected directory applies only to that Bash call. It does not mutate `ToolContext` or persist to subsequent calls. The existing OS sandbox configuration remains rooted at the invocation working directory; because the selected directory is a descendant, no new filesystem grant is required.

### Consequences

- Good, because agents can run `cwd: "subproject"` without shell-level `cd` indirection.
- Good, because the judge and executor receive the same effective directory, preventing safety context from describing one path while the command runs in another.
- Good, because the default schema call and existing commands remain unchanged.
- Good, because canonicalization prevents a symlinked subdirectory from escaping the workspace.
- Bad, because a command cannot select an independently granted directory outside the invocation workspace; agents must use an invocation or worktree rooted at the desired project. (Partially superseded by [ADR 032](032-bash-cwd-sandbox-grants.md).)
- Bad, because invalid cwd arguments now produce a tool error before the command-safety judge is called.

## More Information

- [ADR 032](032-bash-cwd-sandbox-grants.md) partially supersedes this record: a `cwd` may also name a directory the sandbox grants, and any existing directory under `DangerFullAccess`. The rest of this decision, including the optional argument, canonicalization, per-call scope, and the single effective path used by the child, judge, repository digest, and denial diagnostics, stands.
- Issue #575 owns the implementation.
- The provider-facing schema and model-visible wording live in `src/clients/tools/bash.rs` and `src/clients/tools/bash-description.txt`.
- Current sandbox guarantees remain in [Security](../security.md); this decision does not widen them.
