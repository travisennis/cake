---
status: accepted
date: 2026-09-15
decision-makers: Travis Ennis
informed: issue 441
---

# Diagnostic JSON Document For Machine-Mode Introspection

## Context and Problem Statement

Cake's introspection commands spoke three dialects to machine consumers. `cake debug models --json` and `cake sessions list --json` emitted a bare JSON array, while `cake bash check` had no machine-readable mode at all. A consumer that scripted against several commands wrote a parser per command, and could not tell "the inspection found nothing" from "the inspection never ran" without also parsing stderr.

The findings were worse than the payloads. Unrecognized settings keys surfaced as `warning:` text on stderr, so a consumer validating configuration in CI had to scrape stderr for a finding that belongs in the machine-readable result. Ordering was not promised and not always reproducible: `sessions list` sorted by timestamp with a stable sort over arbitrary directory-iteration order, so two sessions written in the same second could list in either order, and `debug models` sorted a `HashMap`'s values at the point of rendering rather than at the boundary.

#308 (`cake doctor`) and #432 (`cake debug config`) will make more of the resolved configuration and trust surface machine-facing, so the shape must be settled before those commands ship.

## Decision Drivers

- One document shape for every diagnostic command, so a consumer writes one parser and switches on a `command` field.
- A versioned document: consumers must be able to detect a shape change instead of silently misparsing output.
- Findings are data, not log text. Configuration and environment findings belong in the document, and machine stdout stays quiet otherwise.
- Determinism. Two runs over the same state emit the same bytes, including array order and key order.
- Exit behavior stays meaningful and unchanged for existing commands: the document carries the detail, the exit code keeps its documented meaning, and a `bash check` verdict remains successful inspection output.
- No default behavior changes. The document appears only under an explicit `--json`.

## Considered Options

- **Keep the per-command dialects (status quo).** Rejected: three parsers, findings only on stderr, and array order that a consumer cannot rely on.
- **Adopt the envelope for new commands only, leaving the bare arrays alone.** Rejected: the bare arrays are the two commands that exist, so "one convention" would describe a shape no command used, and the determinism and stderr problems would stay unsolved.
- **Stream newline-delimited records per item, as `stream-json` does.** Rejected: these commands answer one question per invocation, and one document matches both the completion-JSON precedent and the single-document contract already stated for machine-readable stdout.
- **A versioned envelope with `data` as the payload object (chosen).** One object with `schema_version`, `command`, `status`, `summary`, `checks`, and `data`. `data` is an object so a command can add a field without changing the top-level shape, and `summary` carries the scalars a consumer scripts against.
- **Make `status` reflect the inspected subject, so a `block` verdict or an unknown settings key is `error`.** Rejected: `status` then means different things per command and would imply exit-code changes for `bash check`, whose verdict/override/bypass contract already exits `0`. `status` describes the invocation; the finding's own `status` and the exit code describe the problem.
- **Emit an error document for a failing judge in `bash check --json` too.** Rejected for now: that path's failure classification is a documented exit-code contract that this decision does not change, and the fail-closed detail is provider-supplied text that only the sensitive `--diagnostic` path redacts. `debug models` and `sessions list` failures (settings, directory) carry no such detail and do report an `error` document.

## Decision Outcome

Chosen option: diagnostic commands emit one versioned document, and a command's findings travel inside it.

The document is one pretty-printed object followed by a newline:

```json
{
  "schema_version": 1,
  "command": "debug models",
  "status": "ok",
  "summary": { "count": 1 },
  "checks": [],
  "data": { "models": [] }
}
```

- `schema_version` is `1` today. A change to the top-level shape or to the meaning of an existing field increments it.
- `status` is `ok`, `warning`, or `error`, derived from the findings so a command cannot report a healthy status beside an error finding. It describes the invocation, not the inspected subject.
- `summary` holds command-specific scalars (`count`, judge `verdict`), and `data` holds the payload.
- `checks` holds findings, each with a stable machine-readable `id`, a `status` of `warning` or `error`, and a human-readable `message`. The defined ids are `settings.unknown_key`, `settings.load_failed`, and `sessions.directory_unreadable`.

Command behavior under the document:

- `debug models --json` reports the configured models in name order and moves unrecognized-settings-key findings from stderr into `checks`.
- `sessions list --json` reports the sessions for the working directory newest first, with the session id breaking equal-timestamp ties, which removes the dependency on directory-iteration order.
- `bash check --json` reports the judge decision with `verdict`, `code`, `confidence`, `message`, `overridden`, `bypassed`, and `latency_ms`, and carries unrecognized-settings-key findings in `checks` the same way `debug models --json` does, so a typo in `[tools.bash.judge]` reaches a machine consumer. It conflicts with `--diagnostic`, whose raw report stays text-only because it is deliberately sensitive. A failing judge keeps its documented exit classification and writes no document, so that path reports the findings on stderr instead.
- The payload is serialized at each field's own width rather than through a generic value, so an `f32` judge confidence prints as the same number the text verdict shows.
- A reportable failure renders an `error` document and still exits nonzero; a failure the command cannot render as a document writes only to stderr and leaves stdout empty.
- Human-readable output is unchanged, including the settings warnings that still print to stderr without `--json`.

### Consequences

- Good, because a machine consumer reads one shape, learns the failure from `status` and `checks`, and no longer needs stderr to see a configuration finding.
- Good, because array order is now a property of the document rather than of the filesystem or a hash map, which makes golden output stable and diffs reviewable.
- Good, because #308 and #432 can add `--json` without inventing a second convention, and existing commands keep their exit codes.
- Bad, because `debug models --json` and `sessions list --json` change their machine-readable stdout from a bare array to the document. This is a compatibility change for consumers that parsed the array, documented in [Integrations](../integrations.md).
- Bad, because the finding ids are now a contract: removing or renaming one, or changing a finding's severity, is a schema change rather than an implementation detail.
- Bad, because machine mode suppresses stderr warnings for these commands, so a human piping output to a file no longer sees them; the document carries them instead, except on a `bash check --json` judge error, which writes no document and keeps them on stderr.

## More Information

- Implements issue #441, `Normalize machine-mode output across subcommands`, under parent tracker #507.
- Related: #433 took the `--fail-on-error` half of the same problem statement; #308 (`cake doctor`) and #432 (`cake debug config`) consume this contract.
- Extends the introspection pattern of [ADR-009](./009-debug-models-cli-introspection.md) (load merged settings, print to stdout, exit before agent/session setup) to a documented output contract.
- Current durable authority for the contract: [Integrations](../integrations.md); the Rust types in `src/cli/diagnostic.rs` and the CLI-level tests in `tests/diagnostic_json.rs` are the field-level authority.
