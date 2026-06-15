# CLI And User Output

## Scope

Read this before changing clap flags, argument validation, exit codes,
human-readable output, progress display, help text, logging-visible behavior, or
shell-facing workflows.

## Compatibility Surfaces

- Flag names, aliases, defaults, conflicts, and exit codes.
- Text printed to stdout/stderr, especially in scripts.
- Separation of human-readable output from `json` and `stream-json` modes.
- Progress, retry, and completion summary wording when tests or users depend on
  it.

## Required Checks

- Exercise changed flag paths with focused CLI tests or manual command output.
- Update README examples and CLI docs when behavior or flags change.
- For machine-readable modes, verify stdout remains parseable JSON/NDJSON.

## Common Failure Modes

- Changing text output that scripts parse without treating it as compatibility.
- Letting progress messages leak into JSON modes.
- Updating clap validation without updating help text and README examples.
- Forgetting exit-code classification for new error paths.

## Related Docs

- [cli.md](../design-docs/cli.md)
- [logging.md](../design-docs/logging.md)
- [streaming-json-output.md](../design-docs/streaming-json-output.md)
- [CONTRIBUTING.md](../../CONTRIBUTING.md)
