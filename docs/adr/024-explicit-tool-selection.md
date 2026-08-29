---
status: accepted
date: 2026-08-29
decision-makers: Travis Ennis
consulted: issue #384
informed: issue #384
---

# Explicit Tool Selection

## Context and Problem Statement

Cake currently registers Bash, Read, Edit, and Write for ordinary runs, with read-only sandbox mode as the only built-in selection. Users need a session-level way to expose only the tools required by a workflow. The selection must affect the model-facing prompt, provider tool schemas, execution registry, and persisted session metadata together. Toolbox tools are discovered dynamically and must remain subject to the same selection without weakening their trust rules.

## Decision Drivers

- Preserve current behavior when users do not configure a selection.
- Make an explicit empty list meaningful so a workflow can run without model tools.
- Keep the model from seeing tools that execution cannot accept.
- Support global, project, and profile settings using the existing precedence model.
- Preserve read-only sandbox filtering and toolbox trust boundaries.
- Surface misspelled or unavailable names instead of silently accepting them.

## Considered Options

- Add a general exact-name allowlist under `[tools]`, with the same allowlist available under a profile. This supports built-in and discovered toolbox tools and composes with the existing registry.
- Use only `PreToolUse` hooks. This can reject calls but still advertises the tools and does not change provider request schemas.
- Add separate flags for each built-in tool. This would not cover toolbox tools and would create a larger, less composable CLI surface.

## Decision Outcome

Chosen option: an optional exact-name `tools.enabled` allowlist in settings and profiles. An absent key preserves the existing registry. A present list, including `[]`, filters the registry after sandbox and toolbox rules are applied. Names are case-sensitive and use the registered names (`Bash`, `Read`, `Edit`, `Write`, and `tb__...` for toolbox tools). Names that are not in the final registry are reported as warnings and are not executable.

The setting is a selection surface, not a replacement for sandbox policy. Read-only mode can still remove a name selected by `tools.enabled`, and toolbox executables remain trusted extensions governed by their existing discovery rules.

### Consequences

- Good, because one setting controls all model-visible and executable tool definitions.
- Good, because profiles can provide narrow workflows without changing global defaults.
- Good, because `None` and `Some([])` preserve the distinction between default behavior and no tools.
- Bad, because the selected toolbox names are resolved only after toolbox discovery, so unavailable names can be warned about only during run setup.
- Bad, because changing the selection while continuing a persisted session can append to a session whose original metadata lists a different tool set; the current invocation still uses the resolved selection and the append-only transcript remains unchanged.

## More Information

- Issue #384 defines the user-facing acceptance criteria.
- `docs/configuration.md` owns the settings contract.
- `src/clients/tools/` owns the registry and model-facing tool definitions.
- `docs/security.md` owns the sandbox and trusted-tool boundaries.
