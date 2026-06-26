# Sandboxing And Filesystem Boundaries

## Scope

Read this before changing Seatbelt, Landlock, sandbox configuration, command safety checks, path validation, allowed directories, tool filesystem access, or network policy.

## Compatibility Surfaces

- Default sandbox-on behavior and `CAKE_SANDBOX=off` opt-out.
- Allowed read-only and read-write path rules.
- Persistent directory behavior from settings.
- Bash destructive command blocking and error reporting.
- macOS and Linux behavior differences.

## Required Checks

- Test allowed and denied filesystem paths for the affected tool or sandbox.
- For Linux-sensitive code, run the narrowest feasible Linux target check, or state the platform verification gap.
- Call out security impact and any boundary relaxation in the handoff.

## Common Failure Modes

- Expanding access for convenience without documenting the security tradeoff.
- Fixing macOS Seatbelt while breaking Linux Landlock, or the reverse.
- Treating the Bash safety guard as a replacement for OS sandbox enforcement.
- Forgetting that settings directories grant persistent read-write access.

## Related Docs

- [sandbox.md](../design-docs/sandbox.md)
- [tools.md](../design-docs/tools.md)
- [ARCHITECTURE.md](../../ARCHITECTURE.md)
- [ADR 006: Linux Landlock Release Artifacts](../adr/006-linux-landlock-release-artifacts.md)
- [ADR 010: Linux Landlock Default Dependency](../adr/010-linux-landlock-default-dependency.md)
