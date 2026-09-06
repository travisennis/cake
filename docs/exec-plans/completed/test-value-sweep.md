## Sweep low-value and implementation-coupled tests

This ExecPlan is a living document, maintained per `docs/workflow/exec-plans.md`. It tracks issue #502, "Sweep low-value and implementation-coupled tests."

## Purpose / Big Picture

Cake's tests should protect behavior that users, providers, extensions, and maintainers rely on: CLI and exit contracts, wire formats, persistence, security boundaries, concurrency, and meaningful error handling. Tests that only restate private source structure, prove a trivial derived value, duplicate a stronger test, or assert a tautology consume maintenance time while providing little regression detection. This sweep will review the Rust tests in `src/` and `tests/`, remove only candidates with a concrete value rationale, and simplify production seams that exist solely for deleted tests. The resulting test suite will be smaller without weakening the compatibility and security contracts above.

## Progress

- [x] (2026-09-05) Confirmed a clean `master` synchronized with `origin/master` at `3bebb6e`.
- [x] (2026-09-05) Created and claimed GitHub issue #502; added this plan as the implementation record.
- [x] (2026-09-05) Inventoried the Rust test modules and classified candidates against CLI, wire, persistence, security, hook/toolbox, concurrency, and configuration contracts.
- [x] (2026-09-05) Removed 56 confirmed low-value tests across 19 files after independent review restored four non-subsumed cases; the initial candidate set was 58 and exact final category/file accounting appears below.
- [x] (2026-09-05) Reviewed production seams after deletion; no production simplification was justified because every remaining test-only seam still has retained behavioral consumers or production call sites. Removed only the deleted tests' private helpers (`test_hook_runner` and `REGISTRY_MAPPING`).
- [x] (2026-09-05) Ran focused/full Rust checks, `just check`, coverage/change-risk/CC gates, dependency advisories, rustdoc, Markdown checks, fixtures, and a release build. Independent review resolved the initial coverage failure with supported workspace artifact cleanup.
- [x] (2026-09-05) Updated issue #502 acceptance notes and linked this completed plan; pushed the branch and opened the pull request.

## Surprises & Discoveries

- The repository contains 65 Rust files with test attributes, 1,516 test attributes/locations reported by the initial search, 4,081 assertion lines, and 125 snapshot-related calls; the initial counts include production and test support code and were not used as final removal counts.
- The project board currently has more than 200 items. The checked-in `scripts/claim-issue.sh` only reads 200 items, so issue #502 had to be claimed with the same documented GraphQL status mutation after verifying the item at a larger limit. This is workflow tooling context, not part of the code change.
- The initial coverage report contained both `/Users/travisennis/Projects/cake-1/` and `/Users/travisennis/Projects/cake/cake-1/` source roots and reported 33.66%. With cargo-llvm-cov 0.9.0, `cargo llvm-cov clean --workspace` followed by `just check-coverage` removed the duplicate roots and passed at 94.53%, including CRAP and complexity gates, before any test restoration. The recipe's existing `--profraw-only` cleanup had not removed the stale workspace artifacts. No coverage-related environment overrides were present in the review shell; the exact origin of the stale artifacts was not established.

## Decision Log

- Decision: Treat this as an L-sized cross-cutting maintenance task requiring an ExecPlan. Rationale: the audit spans all Rust test surfaces and can affect production seams, while the requested outcome requires exact category/file accounting and compatibility-focused verification. Date/Author: 2026-09-05, coding agent.
- Decision: Remove a test only when its assertions do not add meaningful behavioral protection or are fully subsumed by a stronger test. Preserve unit tests when they exercise parsing, validation, boundary conditions, error semantics, security, concurrency, serialization, or other observable behavior. Rationale: test location is not itself evidence of low value. Date/Author: 2026-09-05, coding agent.
- Decision: Do not replace deleted low-value tests with new tests solely to preserve coverage. Rationale: coverage is a signal, not the purpose of a test; the requested sweep prioritizes regression value. Date/Author: 2026-09-05, coding agent.

## Outcomes & Retrospective

The sweep removed 56 tests from 19 files. The categories are intentionally non-overlapping:

- **24 duplicate behavioral checks**, retained behavior in stronger snapshots or neighboring cases: `src/cli/debug.rs` (2), `src/cli/replay.rs` (1), `src/clients/chat_completions_tests.rs` (1), `src/clients/responses_tests.rs` (2), `src/clients/tools/read.rs` (1), `src/config/session.rs` (2), `src/main_tests.rs` (9), and `src/prompts/mod.rs` (6).
- **19 trivial, derived, no-op, or pass-through checks**: `src/cli/output.rs` (1), `src/cli/sessions.rs` (2), `src/clients/agent/agent_tests.rs` (3), `src/clients/agent_state.rs` (2), `src/clients/tools/bash_tests.rs` (1), `src/clients/tools/mod.rs` (4), `src/clients/tools/toolbox_tests.rs` (2), `src/config/data_dir.rs` (2), `src/config/session.rs` (1), and `src/types/usage.rs` (1).
- **13 implementation-coupled or reimplemented checks**: `src/clients/chat_completions_tests.rs` (3), `src/clients/judge_rubric_tests.rs` (1), `src/clients/responses_tests.rs` (1), `src/clients/tools/mod.rs` (4), and `src/types/conversation.rs` (4).

### Removed-test ledger

D = duplicate behavioral check; T = trivial/derived/no-op/pass-through check; I = implementation-coupled or locally reimplemented check. Each removed test appears once.

- `src/cli/debug.rs` (2 D): `format_models_handles_optional_fields_unset`, `render_models_json_false_returns_table`.
- `src/cli/output.rs` (1 T): `stream_json_swallows_success`.
- `src/cli/replay.rs` (1 D): `load_records_returns_all_lines_in_order`.
- `src/cli/sessions.rs` (2 T): `truncate_prompt_short`, `truncate_prompt_empty`.
- `src/clients/agent/agent_tests.rs` (3 T): `accumulate_usage_none_is_noop`, `builder_with_session_id`, `builder_with_history`.
- `src/clients/agent_state.rs` (2 T): `resolve_assistant_message_no_output_items`, `resolve_assistant_message_items_but_no_message_or_reasoning`.
- `src/clients/chat_completions_tests.rs` (1 D): `build_messages_empty_history`; (3 I): `parse_choices_with_usage`, `parse_response_extracts_cached_tokens`, `parse_response_defaults_cached_tokens_when_missing`.
- `src/clients/judge_rubric_tests.rs` (1 I): `vocabulary_covers_every_registry_check`.
- `src/clients/responses_tests.rs` (2 D): `extract_instructions_empty_history`, `build_input_empty_history`; (1 I): `provider_config_with_all_returns_none`.
- `src/clients/tools/bash_tests.rs` (1 T): `require_sandbox_tests_defaults_to_false_when_unset`.
- `src/clients/tools/mod.rs` (4 T): `hint_no_line_column`, `escape_context_passes_normal_chars`, `empty_registry_returns_empty_slice`, `read_tool_registry_definitions_match`; (4 I): `tool_context_with_temp_dirs_preserves_inputs`, `tool_context_construction_is_repeatable_with_explicit_temp_dirs`, `definitions_returns_same_slice_on_repeated_calls`, `definitions_are_stable_across_clone`.
- `src/clients/tools/read.rs` (1 D): `start_line_one_without_end_line_matches_default`.
- `src/clients/tools/toolbox_tests.rs` (2 T): `truncate_output_passes_small_output_through`, `tool_that_ignores_stdin_still_succeeds`.
- `src/config/data_dir.rs` (2 T): `sessions_dir_structure`, `session_telemetry_path_lives_under_cache_directory`.
- `src/config/session.rs` (1 T): `test_session_new_defaults`; (2 D): `test_session_loads_trailing_task_start`, `test_session_create_writes_v4`.
- `src/main_tests.rs` (9 D): `test_cli_parsing_no_session_defaults_false`, `test_cli_parsing_add_dir_none`, `test_cli_parsing_skills_defaults`, `test_build_content_prompt_with_empty_stdin`, `test_build_content_multiline_prompt`, `test_build_content_multiline_stdin`, `test_build_content_multiline_both`, `handle_agent_turn_success_with_hooks`, `handle_agent_turn_error_with_hooks`.
- `src/prompts/mod.rs` (6 D): `resolve_uses_builtin_when_no_override`, `resolve_builtin_is_trimmed`, `empty_agents_files`, `with_agents_files`, `with_skill_catalog`, `with_agents_and_skills`.
- `src/types/conversation.rs` (4 I): `role_deserialization_case_insensitive`, `role_equality`, `role_clone`, `role_debug_format`.
- `src/types/usage.rs` (1 T): `usage_default_values`.

Independent review restored `emit_task_complete_record_no_callback` and `test_is_binary_data_allows_empty` because they cover distinct no-observer and empty-output branches. The two restored tests are intentionally absent from this ledger.

Independent review corrected the original duplicate row (which summed to 26 and counted two agent tests again) and restored the two distinct boundary tests `seconds_tenths_handles_max_milliseconds_without_overflowing` and `resolve_assistant_message_from_past_end_is_none`. The former detects overflow at `u128::MAX`, unlike ordinary rounding cases; the latter detects out-of-range slicing, unlike the retained exact-end case. None of these restorations is for coverage inflation.

### Verification

After the four restorations, `just check-full` passed end to end: formatting, strict Clippy in both feature modes, all-feature tests (1,387 unit tests passed, 2 ignored, plus 93 integration tests), Linux compatibility, fixtures, coverage at 94.53%, CRAP regression, cyclomatic complexity, dependency checks, rustdoc, Markdown checks, and release build. The restored tests covered `seconds_tenths_handles_max_milliseconds_without_overflowing`, `resolve_assistant_message_from_past_end_is_none`, `emit_task_complete_record_no_callback`, and `test_is_binary_data_allows_empty`; `git diff --check` passed. All nine hosted checks on the review-fix PR head also passed.

The initial coverage failure was resolved by supported artifact cleanup rather than by adding low-value tests. Independent review verified the removed names against the Git diff and retained the private fixture cleanup; no production seam cleanup was justified.

## Context and Orientation

Cake is a Rust 2024 binary-only CLI. Test code lives in inline `#[cfg(test)]` modules, dedicated `*_tests.rs` modules under `src/`, and integration tests under `tests/`. The compatibility authorities are `src/main.rs` and `src/cli/` for CLI shape and output, `src/clients/` and snapshots for provider wire formats, `src/clients/tools/` plus security documentation for tool and sandbox behavior, `src/config/` and `src/prompts/` for configuration and prompt resolution, and `src/types/session.rs` plus snapshots for persisted records. `ARCHITECTURE.md` describes the boundaries that must remain covered.

The audit will prioritize clear source-string checks, tests that merely assert a private implementation name or structure, trivial getters/derived accessors, tautologies, and duplicate scenarios. It will not delete a test merely because it is a unit test, has a short body, checks a string, or contributes to coverage. Protocol and user-facing strings remain valuable when they are part of a documented contract or distinguish an externally observable failure.

## Plan of Work

First, inspect every test module and build a candidate ledger from test names, assertions, snapshots, and production seams. For each candidate, identify the behavior it protects and whether another retained test or an integration contract already covers it. Exclude security, wire, persistence, CLI/exit, hook/toolbox, scheduling, concurrency, boundary, and meaningful error-path tests from deletion unless the evidence shows exact duplication.

Next, edit only the affected test files to remove confirmed candidates and their now-unused helpers/imports. If a candidate is a duplicate, retain the test with the stronger observable assertion and explain the choice in the issue/PR accounting. If a deleted test was the sole consumer of a production-only helper or seam, simplify that production code in the same focused diff; do not change public behavior or use the sweep to refactor unrelated code.

Finally, format and run focused tests for touched modules, then run `just check`. Because this is a Rust and test change, use `just check-full` or the narrower coverage/change-risk commands available in the repository if the full local run is feasible; report exact skipped checks and reasons. Review the final diff for accidental behavior changes, compatibility regressions, and unnecessary deletion. Complete the issue notes and archive this plan before opening the PR.

## Concrete Steps

All commands run from `/Users/travisennis/Projects/cake/cake-1`.

Use `rg`, `find`, `wc`, and targeted reads to enumerate test functions, source-string assertions, snapshots, duplicate helper setup, and test-only production seams. Maintain a local candidate ledger during the audit; it need not be committed unless a reviewer needs it. For each edit, run a focused command such as `cargo test <module-or-test-filter>` and inspect the diff with `git diff --check` and `git diff --stat`.

After implementation, run:

```
cargo fmt -- --check
cargo test --all-features --quiet
just check
```

Run `just check-full` when time and local tool availability permit. If the full coverage/change-risk suite is run separately, use the repository recipes from `justfile` and record the exact result. Before handoff, inspect `git status --short`, `git diff --check`, and `git diff --stat`, push `test/test-value-sweep`, and open the PR with `just pr labels="type:test,area:quality" body=<body-file> issue=502` after acceptance notes and this plan are complete.

## Validation and Acceptance

Acceptance is behavioral and accounting-focused. The retained suite must still exercise the documented CLI and exit behavior, provider request/response translations and snapshots, session JSONL records, settings and prompt precedence, tool argument/path validation, sandbox fail-closed behavior, hook and toolbox protocols, scheduling/concurrency limits, and meaningful errors. `cargo test --all-features --quiet` and `just check` must pass. The final report and issue comment must enumerate every removed test with category and repository-relative file, state why it was low-value or duplicated, identify every production simplification, and list any checks that were not run.

No behavior or security contract is intentionally changed. Any test failure caused by a production behavior difference blocks handoff rather than being fixed by weakening the assertion.

## Idempotence and Recovery

Inventory and test commands are safe to repeat. Test deletions are recoverable with Git while the branch retains history; before broad edits, commit a coherent checkpoint so a candidate can be restored without reverting unrelated work. If formatting or checks expose an accidental behavior change, restore only the affected paths from the last branch commit, re-read the candidate's rationale, and narrow the deletion. Do not reset or clean unrelated user changes. Do not hand-edit generated `ci/cargo-crap-baseline.json`; if a production simplification changes its metrics, regenerate it with `just change-risk-baseline`.

## Artifacts and Notes

The final PR body and issue #502 acceptance comment will include an exact removal ledger, simplification summary, compatibility assessment, verification commands/results, and skipped checks. This plan's completed Outcomes & Retrospective will preserve the same conclusions before the plan is moved to `docs/exec-plans/completed/`.

## Interfaces and Dependencies

The sweep relies on Rust's existing test harness, `cargo test`, `cargo insta` snapshots where applicable, and the repository's `just` recipes. It must preserve the public CLI binary, OpenAI-compatible backend request/response shapes, session JSONL and stream records, hook/toolbox protocols, sandbox policy, and all documented exit behavior. No dependency or wire-format change is in scope.

## Revision note

- (2026-09-05) Filled the initial outcomes ledger and archived the plan before opening PR #503.
- (2026-09-05) Independent review corrected double-counted agent tests, restored four distinct cases, reconciled the final 56-test ledger, and resolved stale coverage artifacts with `cargo llvm-cov clean --workspace`.
