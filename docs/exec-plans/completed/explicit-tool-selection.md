# Add explicit per-session tool selection

This ExecPlan is a living document, maintained per `docs/workflow/exec-plans.md`.

## Purpose / Big Picture

After this change, a user can write `tools.enabled = ["Read", "Edit"]` in `settings.toml`, or put the same key under a selected profile, and Cake will expose and execute only those registered tools. An absent key keeps the current default. An explicit empty list sends no tools to the model. The selection will be consistent across the system prompt, provider request, executor lookup, and new-session metadata.

## Progress

- [x] (2026-08-29) Inspect issue #384, relevant configuration, prompt, registry, agent, and provider code.
- [x] (2026-08-29) Create and accept ADR 024 for the configuration and security decision.
- [x] (2026-08-29) Create feature branch `feat/tool-selection` and claim issue #384.
- [x] (2026-08-29) Add settings and profile parsing, merging, and resolved state.
- [x] (2026-08-29) Filter the agent registry and default prompt, including toolbox tools.
- [x] (2026-08-29) Add focused regression tests and update the configuration reference and `cake init` template.
- [x] (2026-08-29) Run formatting, focused tests, review, and `just ci`.
- [ ] Move this plan to `docs/exec-plans/completed/` before opening a pull request.

## Surprises & Discoveries

- Observation: the prompt formatter builds an independent default registry. Evidence: `src/clients/tools/mod.rs` constructs a registry inside `format_tool_list_section`; the same selection is applied there and in the agent registry.
- Observation: provider adapters already omit tool fields when given an empty slice. Evidence: `src/clients/chat_completions.rs` and `src/clients/responses.rs` branch on `tools.is_empty()`.
- Observation: toolbox names are available only after discovery. Consequence: selection filtering occurs after `with_toolbox_tools`, and the CLI reports unavailable names after the agent is built.
- Observation: the first full gate found complexity and CRAP regressions after the new selection branches. Evidence: `just ci` first failed its CC/CRAP checks; extracting small footer, selection, and profile-overlay helpers restored the limits, and the final gate passed with zero regressions and zero CC exceedances.

## Decision Log

- Decision: use optional exact-name `tools.enabled` rather than a `disabled` list. Rationale: an allowlist makes the safe subset explicit, supports `[]`, and includes future toolbox names without adding one setting per tool. Date/Author: 2026-08-29 / Travis Ennis.
- Decision: use replacement semantics for the list at each precedence level. Rationale: a project or profile can narrow a lower-precedence global list; union semantics would make narrowing impossible. Date/Author: 2026-08-29 / Travis Ennis.
- Decision: warn from the CLI for names missing from the final registry and never register them. Rationale: configuration cannot validate dynamic toolbox names, while the CLI owns user-facing diagnostics. Date/Author: 2026-08-29 / Travis Ennis.

## Outcomes & Retrospective

Implemented optional exact-name tool selection through `[tools].enabled` and `[profiles.NAME.tools].enabled`. The default remains unchanged when the key is absent. Empty lists expose no tools. The filter applies to built-ins and discovered toolbox tools, survives builder-order changes, intersects with read-only sandbox policy, updates the built-in prompt, provider schemas, execution registry, and session metadata, and warns on unavailable names. The empty prompt footer now says that no tools are available.

Verification covered settings precedence and round trips, registry and prompt filtering, agent and sandbox interaction, Chat Completions and Responses empty-tool serialization, and two real-binary integration tests. `just ci` passed at 94.00% total coverage with no CRAP regressions and no cyclomatic-complexity exceedances. No new dependency or snapshot change was needed.

The main tradeoff is that an unavailable name is a warning rather than a startup error because toolbox names are dynamic. This matches existing skill-selection behavior and keeps the setting usable across machines with different trusted toolbox installations.

## Context and Orientation

Cake loads global settings from `<config>/cake/settings.toml` and project settings from `.cake/settings.toml`. Project values overlay global values. A selected profile overlays the merged top-level settings. `src/config/settings.rs` deserializes and merges this data into `LoadedSettings`. Existing `[tools.bash.judge]` settings must continue to work unchanged. ADR 024 records the accepted exact-name allowlist and its security interaction.

`src/main.rs` loads settings, discovers toolbox executables, builds the prompt, and creates an `Agent`. `src/clients/tools/mod.rs` owns the `ToolRegistry`, which pairs model-facing definitions with executors. `src/clients/agent.rs` initializes the default registry and attaches toolbox tools. `src/clients/agent/agent_loop.rs` sends the registry definitions to the provider and rejects unregistered calls. `src/prompts/mod.rs` inserts the available-tools section into the built-in system prompt. Session metadata is derived from `Agent::tool_names()` in `src/main.rs`.

The setting will use registered, case-sensitive names: `Bash`, `Read`, `Edit`, `Write`, and discovered toolbox names such as `tb__run_tests`. `None` means no selection was configured. `Some(vec![])` means no tools. Read-only sandbox filtering remains a separate intersection: it can remove Edit, Write, and toolbox tools even when selected.

## Plan of Work

First extend `src/config/settings.rs` with an optional `enabled` list under top-level `[tools]`, a profile overlay under `[profiles.NAME.tools]`, and a resolved value on `LoadedSettings`. Merge the list by replacement only when the key is present, preserving the distinction between absent and empty. Keep Bash judge extraction independent from the new field. Update test fixtures that construct `LoadedSettings` directly.

Next add a registry filter in `src/clients/tools/mod.rs` that rebuilds cached definitions after filtering. Apply it in the agent builder after sandbox and toolbox registration. Apply the same filter to the prompt formatter, threading the optional selection through `src/prompts/mod.rs` and `src/cli/session_factory.rs`. After the final agent is built, the CLI will compare configured names with `Agent::tool_names()` and print one `warning:` line for every unavailable name. This keeps diagnostics out of the clients layer and makes unknown names non-executable.

Then add settings merge tests, registry and prompt tests, agent-builder tests, and provider coverage for an empty selection. Update `docs/configuration.md`, `src/cli/init.rs`, and related tests with the exact syntax, precedence, name rules, empty-list behavior, and sandbox interaction. Review session metadata and hook behavior for consistency without changing their existing contracts.

## Concrete Steps

All commands run from `/Users/travisennis/Projects/cake`.

1. Edit the settings, registry, agent, prompt, session-factory, main, tests, and documentation files described above. Keep the default path unchanged when `tools.enabled` is absent.

2. Run focused tests while iterating:

   ```
   cargo test settings
   cargo test tool
   cargo test prompt
   cargo test agent
   ```

3. Format changed Rust and Markdown through the repository's normal checks:

   ```
   cargo fmt --all -- --check
   just docs-check
   ```

4. Run the complete Rust/configuration gate:

   ```
   just ci
   ```

5. Review the final diff and issue acceptance, then update this plan's progress and outcomes before opening a pull request.

## Validation and Acceptance

A temporary project settings file containing `tools.enabled = ["Read", "Edit"]` must produce a session whose `session_meta.tools` contains only `Read` and `Edit`, whose initial prompt lists only those tools, and whose provider request contains only those definitions. A model request for Bash in that run must receive the existing unknown-tool error and must not execute Bash.

A file containing `tools.enabled = []` must produce an empty tool list, omit provider `tools` and `tool_choice` fields, and still complete a no-tool response. Removing the key must restore the four default built-ins. A profile list must replace a top-level list. A selected `tb__name` must work only when the toolbox tool was discovered and trusted. `--sandbox read-only` must still remove Edit, Write, and toolbox tools. An unavailable configured name must appear as a `warning:` diagnostic and never enter `Agent::tool_names()`.

The settings tests must prove global/project/profile precedence and the `None` versus empty distinction. Registry tests must prove cached definitions and execution lookup use the same filtered set. Prompt and provider tests must prove no unavailable definition leaks to the model.

## Idempotence and Recovery

The settings and code changes are safe to repeat through exact edits. Focused tests and formatting are repeatable. If a test reveals a stale snapshot or fixture, update only the fixture required by this feature and rerun its focused command. Do not rewrite session history or change the generated CRAP baseline until the final gate requires it. If the full gate fails from stale coverage artifacts, use the repository's documented clean coverage procedure before interpreting the result.

The ADR and ExecPlan are repository records created before implementation. If the product decision changes, update ADR 024 and this plan before changing code. If implementation is abandoned, leave the issue open or close it as not planned with a reason; do not silently remove the records.

## Artifacts and Notes

Expected configuration example:

```
[tools]
enabled = ["Read", "Edit"]
```

Expected profile example:

```
[profiles.review.tools]
enabled = ["Read"]
```

The final issue comment should record the focused tests, `just ci` result, documentation assessment, and the resulting tool-selection behavior.

## Interfaces and Dependencies

- `crate::config::settings::ToolsSettings` will carry the optional top-level `enabled` list alongside the existing Bash settings.
- `crate::config::settings::ToolsSettingsOverlay` will carry profile `enabled` state.
- `crate::config::settings::LoadedSettings` will expose the resolved optional list to CLI setup.
- `crate::clients::tools::ToolRegistry` will filter entries and rebuild definitions.
- `crate::clients::Agent::with_enabled_tools` will apply the selection after sandbox and toolbox registration.
- `crate::prompts::resolve_system_prompt` and `crate::prompts::build_initial_prompt_messages` will receive the optional selection so built-in prompt text matches the registry.
- Existing `serde`, TOML parsing, provider adapters, sandbox policy, hook protocol, and toolbox protocol remain dependencies; no new dependency is needed.
