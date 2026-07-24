---
status: accepted
date: 2026-07-13
decision-makers: Travis Ennis
---

# Nested Seatbelt Sandbox Fallback

## Context and Problem Statement

Cake applies a macOS Seatbelt profile to every sandboxed Bash command through `/usr/bin/sandbox-exec`. Before executing commands, it probes whether the current process context can apply a profile and fails closed when the probe fails.

A cake process launched inside another Seatbelt sandbox cannot apply a second profile: macOS rejects `sandbox_apply` with `Operation not permitted`. Treating that recognized nested-sandbox condition like an unavailable or broken sandbox prevents cake from acting as a subagent even though the parent process tree is already constrained by Seatbelt.

## Decision Drivers

- Preserve cake's default sandbox-on behavior in normal process contexts.
- Allow cake's Bash tool to operate when an inherited Seatbelt sandbox prevents applying a nested profile.
- Keep missing `sandbox-exec`, malformed profiles, spawn failures, and unrecognized probe failures fail-closed.
- Make the fallback visible in logs.
- Limit the behavior change to macOS without changing Linux Landlock enforcement.

## Considered Options

- **Fall back only for the recognized nested-Seatbelt error signature.** When probe details contain `sandbox_apply: Operation not permitted`, warn and run Bash without applying cake's own child profile.
- **Continue failing closed for every profile-application failure.** Require callers to select `danger-full-access` explicitly before cake can run as a nested subagent.
- **Fall back for every profile-application failure.** Treat any failed probe as evidence that another sandbox protects the process.

## Decision Outcome

Chosen option: **fall back only for the recognized nested-Seatbelt error signature**, because it restores nested-agent interoperability while retaining fail-closed behavior for failures that do not establish the expected inherited-sandbox condition.

### Consequences

- Good, because cake can run Bash commands when invoked by Codex, Claude Code, or another Seatbelt-using parent without requiring that parent to grant `danger-full-access`.
- Good, because missing binaries and all unrecognized probe failures still block Bash execution.
- Good, because a warning records that cake did not apply its own filesystem profile and is relying on inherited Seatbelt enforcement.
- Neutral, because the error-signature check is coupled to the stable diagnostic emitted by macOS `sandbox-exec`.
- Bad, because cake cannot guarantee that the inherited sandbox grants exactly the same paths or restrictions as cake's selected policy. In particular, cake's own Bash-side `read-only` or `workspace-write` path policy is not independently enforced during the fallback; the outer sandbox is the effective security boundary.

## More Information

- Task 275: Graceful fallback when nested inside another Seatbelt sandbox.
- Related: ADR-014, Sandbox Policy CLI Flag. This decision narrows its normal macOS enforcement behavior only when nested Seatbelt application is rejected with the recognized EPERM signature.
- `docs/security.md`
- `src/clients/tools/sandbox/mod.rs`
- `src/clients/tools/sandbox/macos.rs`
