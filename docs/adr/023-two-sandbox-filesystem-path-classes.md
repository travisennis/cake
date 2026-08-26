---
status: accepted
date: 2026-08-26
decision-makers: Travis Ennis
informed: issue 273
---

# Represent Sandbox Filesystem Grants With Two Path Classes

## Context and Problem Statement

Cake builds filesystem grants for model-generated Bash commands, then translates them to macOS Seatbelt rules or Linux Landlock rules. The internal `SandboxConfig` currently has three path collections: `writable`, `system_paths`, and `readable`. Both platform translators give `system_paths` and `readable` the same effective read-and-execute authority. The macOS translator also emits GitHub CLI and GitLab CLI path grants separately even though the shared writable path builder already owns the same paths.

The duplicate representations can drift without changing the intended policy. The implementation needs one collection for each effective filesystem authority and one owner for each shared path list.

## Decision Drivers

- Preserve the effective filesystem authority of all existing sandbox policies.
- Make distinct internal classes correspond to distinct effective permissions.
- Keep shared SCM CLI paths identical on macOS and Linux and owned in one place.
- Preserve read-only file grants, canonical-path handling, and read-only policy demotion.
- Keep macOS-only device, SSH agent, Keychain, Mach, process, and network capabilities unchanged.
- Keep enforcement fail-closed when Seatbelt or Landlock is unavailable or incomplete.

## Considered Options

- **Keep three path collections and add comments or equivalence tests.** Rejected because duplicate classes and duplicate SCM owners remain available to drift.
- **Collapse `system_paths` and `readable` into one read-and-execute collection and remove the macOS SCM duplicate (chosen).** This matches the two effective filesystem authorities without changing policy.
- **Give system paths a distinct execute-only or broader class.** Rejected because it changes effective authority and has no current user need.

## Decision Outcome

Chosen option: represent ordinary filesystem grants with two path collections: writable paths receive read, write, and execute authority; read-and-execute paths receive read and execute authority but no write authority. System paths, configuration and device paths, user read-only grants, skill paths, and paths demoted by the read-only policy share the read-and-execute collection. The shared sandbox configuration remains the only owner of GitHub CLI and GitLab CLI filesystem paths; platform translators only translate the resulting collections.

Platform-only non-filesystem capabilities and specialized macOS rules remain separate because they represent authority that the two ordinary filesystem path classes cannot express.

### Consequences

- Good, because every ordinary filesystem path class maps to a distinct effective authority.
- Good, because the two platform translators consume the same two classes and SCM filesystem paths have one owner.
- Good, because read-only demotion and user-configured file grants use the same read-and-execute semantics as built-in system paths.
- Neutral, because this is an internal refactor: CLI shape, settings shape, selected policies, allowed paths, and effective authority do not change.
- Risk, because an omitted path or changed file-versus-directory rule could narrow or widen authority. Focused real-platform allow and deny tests protect those cases.

## More Information

- Issue 273: Collapse only equivalent sandbox path classes.
- ExecPlan: `docs/exec-plans/active/collapse-equivalent-sandbox-path-classes.md` while implementation is active.
- ADR 019 remains the authority for project-customizable `read_only` and `writable` settings grants.
- Current security guarantees are in `docs/security.md`; implementation authority is in `src/clients/tools/sandbox/`.
