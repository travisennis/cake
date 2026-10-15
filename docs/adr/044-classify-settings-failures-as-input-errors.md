---
status: accepted
date: 2026-10-15
decision-makers: Travis Ennis
informed: task 081
---

# Classify Settings Failures As Input Errors

## Context and Problem Statement

A settings file that cake cannot read or parse --- an unreadable, unparsable, or self-contradictory `.cake/settings.toml` or global `settings.toml` --- exits `1` on every path, but [Integrations](../integrations.md) and `src/exit_code.rs` assign `3` to `invalid input, flags, configuration, or missing credentials`. The classification named a defect in the configuration the user supplied an agent or tool execution error.

[ADR-029](./029-diagnostic-json-document.md) made the mismatch machine-visible. `cake debug models --json` emitted a document with `status: "error"` and a `settings.load_failed` finding beside exit `1`, and `cake bash check --json` exited `1` with no document, so a consumer scripting `case $? in 3)` for configuration problems never saw the settings failure. ADR-029 promised that "the exit code keeps its documented meaning", and `docs/integrations.md` said only that an `error` document "exits nonzero", which recorded the question instead of answering it. PR #557 preserved the behavior deliberately.

The decision needed is which class a settings load or parse failure belongs to, and how the code, the exit-code table, and the diagnostic document agree.

## Decision Drivers

- The exit-code table is a compatibility surface, and one class must mean one thing.
- Precedent: `classify_typed_error` already maps the configuration-class `OutputSchemaError::{Unreadable, InvalidJson, InvalidSchema}` and `DataDirError` to `3`.
- One decided class, applied to the run path and to every introspection command that can surface the failure.
- Classification stays structural: a typed downcast, not a message pattern.

## Considered Options

- **Keep `1` for settings failures.** Rejected: it reports a configuration defect as an execution error and leaves the document's `error` status disagreeing with the exit code.
- **Keep `1` and document the mismatch as intentional.** Rejected: a consumer that branches on `$?` still cannot tell a broken settings file from a failed tool, which is the distinction the structured codes exist to carry.
- **Map each `SettingsError` variant to its own class.** Rejected for now: every variant is a defect in the input file cake was handed, so a per-variant split would invent distinctions the documented failure classes do not make.
- **Classify settings failures as input errors, exit `3` (chosen).**

## Decision Outcome

Chosen option: a settings load or parse failure is an input/configuration error and exits `3`.

Concretely:

- `src/exit_code.rs` gains a typed `SettingsError` mapping in `classify_typed_error`, beside the output-schema mapping. Every variant maps to `code::INPUT_ERROR`, and the match is exhaustive with no wildcard arm, so a new variant states its class instead of inheriting one.
- The mapping is a typed downcast, not a string pattern. `"Failed to parse settings file"` stays out of the string patterns, and a test keeps it out.
- The typed error must reach `classify_to_u8` unwrapped: `anyhow::Error::downcast_ref` inspects only the outermost error object, so nothing between `SettingsLoader::load*` and the classifier may add `.context(..)` to it.
- The class applies everywhere the failure surfaces: the prompt-run path, `cake debug models` and `cake debug models --json` (which still emit the `settings.load_failed` error document), and `cake bash check --json` (which cannot render a document and reports on stderr with empty stdout). `cake sessions list` does not read settings, so it is unchanged, and `cake init` never loads settings, so it is unaffected.

### Security and Compatibility Impact

The change adds no filesystem, network, or command-execution surface and no new dependency. It is an exit-code compatibility change: a settings load or parse failure exited `1` and now exits `3`, so anything branching on `$?` (1 → 3) must migrate. The stderr diagnostic, the machine-readable document, and the finding `id` are unchanged, so a consumer that reads `status` and `checks` is unaffected. The migration note is in the `## Compatibility changes` section of [Integrations](../integrations.md).

### Consequences

- Good, because a configuration defect exits the code the table already assigns to configuration, so `case $? in 3)` sees it.
- Good, because a diagnostic document's `error` status now agrees with the process exit code.
- Bad, because a script that tested `$? -eq 1` for a broken settings file must change to `3`. Accepted: the old code contradicted the documented meaning, and the change is recorded as a compatibility change.
- Bad, because a consumer distinguishing "my configuration is broken" from "cake could not run" must now read the stderr text or the finding, since both are nonzero. Accepted: the exit code carries the distinction the table promises, and the document carries the detail.

## More Information

- Task 081, migrated from [#559](https://github.com/travisennis/cake/issues/559); found while reviewing PR #557.
- Amends the exit behavior of [ADR-029](./029-diagnostic-json-document.md) without changing its document contract: its "the exit code keeps its documented meaning" now holds for settings failures too.
- Related: #433 (the `--fail-on-error` half of the same review) and #458 (provider failures normalized to the documented class).
- Current durable authority for the contract: [Integrations](../integrations.md); the mapping is in `src/exit_code.rs`, and the tests are in `tests/exit_codes.rs` and `tests/diagnostic_json.rs`.
