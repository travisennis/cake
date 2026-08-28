# Separate limits overlays from resolved runtime limits

This ExecPlan is a living document, maintained per `docs/workflow/exec-plans.md`.

## Purpose / Big Picture

Cake reads `[limits]` values from TOML, merges them across global, project, and selected profile settings, then passes the result to the agent loop, built-in tools, and hooks. Today the loaded result still uses the TOML representation, where `Option<Limit>` means either an absent key or a present key whose value may be a number or `"unlimited"`. Each consumer resolves that representation independently.

After this change, TOML parsing and precedence merging will use a dedicated overlay type. `LoadedSettings` will contain one resolved runtime limits value with concrete agent-loop limits and resolved tool budgets. The agent, tools, and hooks will consume that value without reinterpreting configuration sentinels. Selected profiles will also be able to override limits using the existing profile precedence rules.

## Progress

- [x] (2026-08-28) Confirm issue #285 is open, valid, and claimed as In Progress.
- [x] (2026-08-28) Inspect current settings, agent, tool, hook, profile, and test consumers.
- [x] (2026-08-28) Decide the overlay and resolved runtime representations.
- [x] (2026-08-28) Add the overlay and resolved types, including profile limit overlays.
- [x] (2026-08-28) Resolve limits once while producing `LoadedSettings`.
- [x] (2026-08-28) Update the agent, tool context, and hook consumers.
- [x] (2026-08-28) Add focused top-level, profile, sentinel, and default-preservation tests.
- [x] (2026-08-28) Update configuration and generated settings documentation.
- [x] (2026-08-28) Run focused checks, `just ci`, and review the final diff.
- [x] (2026-08-28) Fill Outcomes & Retrospective and move this plan to `docs/exec-plans/completed/` before opening the pull request.

## Surprises & Discoveries

- Before implementation, `src/config/settings.rs` stored `LimitsSettings` in `LoadedSettings`, even though it was also the serde-facing TOML type. Its fields used `Option<Limit>`, where `None` meant absent and `Some(Limit::UNLIMITED)` meant an explicit unlimited value.
- Before implementation, `SettingsAccumulator` already preserved explicit unlimited values for tool budgets, but it converted agent-loop limits to `Option<u32>` before constructing `LoadedSettings`, so the raw sentinel representation was not consistently preserved.
- Before implementation, `ProfileSettings` had no `limits` field. A profile `[profiles.NAME.limits]` table was therefore ignored and reported as an unknown key. The issue acceptance notes required profile limit merging, so the implementation added that supported overlay.
- Before implementation, the two main tool-limit consumers at `src/main.rs:528` and `src/main.rs:1079` independently called `tool_limits()`.

## Decision Log

- Decision: Keep `LimitsSettingsOverlay` as the serde overlay type and add a separate `ResolvedLimits` runtime type containing `Option<u32>` agent-loop caps plus one `ToolLimits` value. Rationale: the existing TOML syntax and absent-versus-unlimited merge semantics remain explicit, while runtime consumers receive values in their native form. Date/Author: 2026-08-28, cake agent.
- Decision: Add `limits: Option<LimitsSettingsOverlay>` to `ProfileSettings` and merge it after top-level settings, following the existing global-profile then project-profile precedence order. Rationale: this is required by issue #285 acceptance notes and matches the documented profile precedence without changing existing top-level behavior. Date/Author: 2026-08-28, cake agent.
- Decision: Do not add an ADR. Rationale: this is an implementation refactor plus completion of the existing profile-overlay pattern; it does not change persisted session state, defaults, sandbox authority, or provider behavior. The user-facing `[limits]` documentation will be updated because the supported profile syntax becomes complete. Date/Author: 2026-08-28, cake agent.

## Outcomes & Retrospective

The implementation now keeps TOML parsing and precedence state in `LimitsSettingsOverlay`, resolves it once into `ResolvedLimits`, and passes concrete values to the agent, tools, and hooks. Profile limit overlays are supported with global-profile then project-profile precedence. Existing numeric, absent, unlimited, and compiled-default behavior remains covered by tests.

Focused settings and agent tests passed. The full `cargo test` suite passed. `just cc-check`, `just docs-check`, and `just ci` passed. The first CI coverage attempt used stale LLVM coverage artifacts and reported a false 85.87% result; `cargo llvm-cov clean` followed by a clean `just ci` run reported 93.91% coverage, no CRAP regressions, and passed all gates.

The change meets the purpose: runtime consumers no longer reinterpret the TOML overlay, profile limits work without unknown-key warnings, and the existing compatibility behavior remains verified. No follow-up work remains for this plan.

## Context and Orientation

Cake is a Rust 2024 binary-only CLI. `src/config/settings.rs` deserializes `settings.toml`, merges global settings, project settings, and selected profiles, and returns `LoadedSettings`. The `LimitsSettingsOverlay` type is the TOML-facing structure. Its `Limit` wrapper represents either a positive `u32` cap or the explicit `"unlimited"` sentinel. An outer `Option` represents whether a key was absent, so the overlay type is intentionally nested. `ResolvedLimits` is the runtime structure with concrete agent and tool values.

`src/main.rs` builds a `ToolContext` from loaded settings, applies the resolved tool budgets at `src/main.rs:528`, later passes the resolved agent-loop limits at `src/main.rs:1046`, and applies the hook output limit at `src/main.rs:1079`. `src/clients/agent.rs` copies `ResolvedLimits` into its two internal `Option<u32>` fields. `src/clients/tools/mod.rs` stores a `ToolLimits` value, and hooks accept the resolved output cap. `src/config/settings_tests.rs` owns settings merge tests; `src/clients/agent/agent_tests.rs` owns agent limit behavior tests; `src/cli/init.rs` and `docs/configuration.md` describe supported configuration.

The compatibility requirements are strict: absent agent-loop keys remain uncapped; absent tool-budget keys retain their compiled defaults; explicit `"unlimited"` disables the selected cap or overrides a lower-precedence cap; numeric caps remain unchanged; project and profile precedence remains per the configuration document; and tool and hook behavior remains unchanged unless the user configures a limit.

## Plan of Work

First, introduce the separate runtime representation in `src/config/settings.rs`. Keep `LimitsSettingsOverlay` as the serde overlay, add `ResolvedLimits` with the two resolved agent-loop caps and a `ToolLimits` field, and move the default application and sentinel conversion into one resolution method. Change `LoadedSettings.limits` to `ResolvedLimits`. Store overlay values in the merge accumulator until all global, project, and profile layers have been applied, then resolve exactly once in `into_loaded`.

Next, add `limits` to `ProfileSettings` and apply it through the same merge helper used by top-level settings. This preserves per-key inheritance and explicit unlimited overrides for selected profiles, including global profile followed by project profile precedence.

Then, update `src/clients/agent.rs` and `src/main.rs` to consume `ResolvedLimits`. The agent builder will copy the already-resolved agent caps. `ToolContext` will receive the single `ToolLimits` value, and hook setup will read that same value. Remove runtime calls that resolve the overlay a second time.

Finally, update fixtures and focused tests. Settings tests will assert resolved values for top-level, global/project, profile, explicit unlimited, and compiled-default cases. Agent tests will build `ResolvedLimits` directly. Update `docs/configuration.md` and the inert `cake init` settings reference to show that `[profiles.NAME.limits]` is supported. Run formatting, focused tests, complexity checks as needed, and the full Rust gate.

## Concrete Steps

All commands run from `/Users/travisennis/Projects/cake`.

1. Edit `src/config/settings.rs`, `src/clients/agent.rs`, `src/main.rs`, affected fixtures, and focused tests. Keep the public TOML keys unchanged and use the existing `Limit` validation.
2. Run `cargo fmt --all -- --check` while iterating. Expected result: no formatting differences.
3. Run `cargo test settings`. Expected result: all settings-filtered unit and matching integration tests pass.
4. Run targeted agent tests with `cargo test max_turns_limit` and `cargo test max_tool_calls_limit`. Expected result: existing limit-exceeded behavior remains green.
5. Run `just cc-check` if modified functions are reported by the complexity gate. Expected result: no new CC violation.
6. Run `just ci`. Expected result: the complete Rust gate passes, including formatting, Clippy, tests, coverage/change risk, imports, and module-size checks.
7. Review `git diff --check`, `git diff --stat`, and the complete diff. Confirm no unrelated files changed and that `LoadedSettings` has only the resolved limits representation.

## Validation and Acceptance

A settings file with numeric top-level limits produces the same agent and tool caps as before. An absent agent-loop limit leaves the loop uncapped, while an absent tool-budget limit uses its compiled default. A project value overrides the corresponding global value, and a project or selected profile value of `"unlimited"` removes a lower-precedence cap. An absent key in a higher-precedence layer does not erase a lower-precedence value.

A selected profile can contain, for example:

```
[profiles.review.limits]
max_turns = 10
read_max_output_bytes = "unlimited"
```

and the resolved result uses those values without warning. Global profile values apply before project profile values. The settings tests must prove these cases and must prove no unknown-key warning is emitted for the new profile table.

Agent tests must continue to prove that max-turn and max-tool-call limits stop the loop at the same point and produce the same user-facing limit details. Tool and hook code must receive the same resolved `ToolLimits` value from the CLI without any call to the overlay resolver at runtime.

## Idempotence and Recovery

The code and documentation edits are safe to repeat when based on the current file contents. Focused tests and `just ci` do not modify source files, except for normal generated coverage artifacts handled by repository recipes. If a test or gate fails, keep the implementation branch intact, inspect the failing output, and rerun the narrowest failed command after correction. Do not regenerate `ci/cargo-crap-baseline.json` unless the gate shows a real complexity or coverage baseline change is required.

If the profile syntax or resolved type needs a material change, update this plan's Decision Log and every affected milestone before continuing. If the change cannot preserve existing defaults or sentinel semantics, stop and record the compatibility issue rather than silently changing behavior.

## Artifacts and Notes

The primary proof will be the focused settings and agent test output, plus the final `just ci` result. The issue's acceptance checklist must be updated with the actual commands and results before opening the pull request. The pull request body must contain `Closes #285`.

## Interfaces and Dependencies

`src/config/settings.rs::LimitsSettingsOverlay` remains the serde overlay for `[limits]` tables. `src/config/settings.rs::ResolvedLimits` is the runtime value stored in `LoadedSettings.limits`; it owns resolved `max_turns`, `max_tool_calls`, and `ToolLimits`. `src/config/settings.rs::ToolLimits` remains the concrete budget structure used by `ToolContext` and hooks. `src/config/settings.rs::SettingsLoader::load_with_profile` remains responsible for precedence and resolution.

`src/clients/agent.rs::Agent::with_limits` will accept `&ResolvedLimits`. `src/clients/tools/mod.rs::ToolContext::with_limits` continues to accept `ToolLimits`. `src/main.rs` will pass `LoadedSettings.limits.tool_limits` to both tool and hook consumers and `&LoadedSettings.limits` to the agent. No provider, session, sandbox, or external runtime dependency changes.
