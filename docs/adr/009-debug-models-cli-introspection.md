# ADR 009: Debug Models CLI Introspection

**Status:** Accepted
**Date:** 2026-06-02

## Context

Cake's CLI is currently a flat prompt-oriented command. Operators need a way to
inspect effective model configuration without starting an agent turn or resolving
API keys. Task 194 introduces the first introspection command, so the command
shape should be stable enough for future debug/config inspection commands.

## Decision

Add a `debug` top-level subcommand family with a `models` subcommand:

```text
cake [OPTIONS] [PROMPT]
cake debug models
```

`cake debug models` loads merged settings from the current directory using the
same settings loader as normal runs, prints configured model metadata to stdout,
and exits before agent/session setup. It displays the configured API key
environment variable name but never reads or displays the API key value.

## Rationale

- Keeps introspection commands separate from prompt execution while preserving
  the existing prompt-oriented default path.
- Reuses `SettingsLoader`, so global and project settings precedence remains
  consistent with normal runs.
- Avoids introducing a machine-readable output contract before the project has a
  broader debug command strategy.

## Consequences

- **Positive**: Users can verify model settings without making a provider call.
- **Positive**: Future debug subcommands have an established clap structure.
- **Negative**: `debug` is reserved as a top-level subcommand name.

## Alternatives Considered

- **Flat `--debug-models` flag**: Rejected because it does not scale to future
  introspection commands.
- **Resolve and validate API keys**: Rejected because the command is for
  configuration inspection and must not expose secret values.

## References

- Task 194: Add `cake debug models` Command to Show Configured Models
- `docs/design-docs/cli.md`
- `src/cli/debug.rs`
- `src/config/settings.rs`
