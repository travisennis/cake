# Extract Inline Tests from Remaining Large Modules

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds. This document must be maintained in accordance with `ahm context plan` guidance.

Task: `171` (`.agents/.tasks/active/171.md`).

## Purpose / Big Picture

Several large Rust source files in this repository mix production code with big inline unit-test blocks written as `#[cfg(test)] mod tests { ... }`. Large mixed files are harder to scan and review. This repository already established a repeatable pattern for separating the two: move the inline test module body into a sibling file named `<module>_tests.rs` and re-attach it to the production module with `#[cfg(test)] #[path = "<module>_tests.rs"] mod tests;`. Examples that already use this pattern are `src/clients/chat_completions.rs` (tests in `src/clients/chat_completions_tests.rs`), `src/clients/tools/edit.rs` (tests in `src/clients/tools/edit_tests.rs`), `src/config/settings.rs` (tests in `src/config/settings_tests.rs`), and `src/clients/agent.rs` (tests in `src/clients/agent/agent_tests.rs`).

After this change, the remaining large modules will have their inline test blocks moved into sibling `*_tests.rs` files, making the production files shorter and easier to read. There is no user-visible behavior change: the same tests run, compile, and pass. Success is observable by running the test suite before and after and seeing the identical set of tests pass, and by seeing shorter production files (`wc -l`) with the test logic relocated to sibling files.

"Inline test block" means a module annotated with `#[cfg(test)]` whose name contains `test` (for example `mod tests` or `mod response_parsing_tests`) that lives inside a production `.rs` file. "Extract" means cut that module's body into a new sibling `*_tests.rs` file and replace the inline module with a one-line `#[path = "..."] mod tests;` declaration.

## Progress

- [x] (2026-07-01) Analyzed current state: measured prod/test line splits, identified test-module boundaries, confirmed the established extraction pattern and its handling of nested test submodules.
- [x] (2026-07-01) Authored this ExecPlan.
- [x] (2026-07-01) Batch 1: extract `src/clients/responses.rs` tests into `src/clients/responses_tests.rs` (two test modules: `tests` and nested `response_parsing_tests`).
- [x] (2026-07-01) Batch 2: extract `src/types/session.rs` tests into `src/types/session_tests.rs`.
- [x] (2026-07-01) Batch 3: extract `src/config/skills.rs` tests into `src/config/skills_tests.rs`.
- [x] (2026-07-01) Batch 4: extract `src/clients/tools/bash.rs` tests (plus `#[cfg(test)]` test helpers) into `src/clients/tools/bash_tests.rs`.
- [x] (2026-07-01) Batch 5: extract `src/hooks.rs` tests into `src/hooks_tests.rs` (production line flag remains; extraction reduces clutter).
- [x] (2026-07-01) Batch 6: extract `src/main.rs` tests into `src/main_tests.rs`, moving the test-only `#[cfg(test)] use crate::types::Role;` import into the test file (production line flag remains).
- [x] (2026-07-01) Added the seven new `*_tests.rs` files to the `cargo-crap` exclude list in `scripts/cargo-crap.sh`, matching how previously-extracted test files (`agent_tests.rs`, `chat_completions_tests.rs`, `settings_tests.rs`) are excluded from the change-risk gate.
- [x] (2026-07-01) Final validation: `cargo fmt`, `cargo check --tests`, all targeted module tests, `just fmt-check`, `just clippy-strict`, `just clippy-no-default-features`, `test-all-features` (774 unit + integration), `just lint-imports`, `just lint-module-size`, and `just task-index-check` all pass. `just ci`'s `check-coverage` gate reports one CRAP regression (`Agent::send` in `src/clients/agent/agent_loop.rs`, Δ +0.3), which reproduces identically on a clean tree with all task changes stashed — it is pre-existing coverage nondeterminism against a stale `ci/cargo-crap-baseline.json`, not caused by this task.

## Surprises & Discoveries

- Observation: The task's stated line counts and its claim that all seven files are flagged by `just lint-module-size` are stale. `src/clients/agent.rs` already had its tests extracted (now 440 prod lines). The current linter flags only `responses.rs` (test ratio), `hooks.rs` (production lines), and `main.rs` (production + test), plus the already-extracted `*_tests.rs` files (which are flagged simply for being >800 test lines, consistent with prior extractions).
  Evidence: `just lint-module-size` output and per-file `count_lines` from `scripts/lint-module-size.py`.
- Observation: Extracting tests does not silence the linter for `hooks.rs` and `main.rs` because those are flagged on *production* line count (863 and 1017, both >800), which extraction does not change. Extraction still reduces total file size and clutter, which is the task's stated goal. The linter always exits 0 (informational), so "pass" means it runs cleanly, not that it emits zero warnings.
  Evidence: `scripts/lint-module-size.py` header comment: "Exit code is always 0; this is an informational lint."
- Observation: Nesting `responses.rs`'s second test module (`response_parsing_tests`) inside the extracted `mod tests` changed its module path from `clients::responses::response_parsing_tests` to `clients::responses::tests::response_parsing_tests`, which broke `insta` named-snapshot resolution (insta derives snapshot filenames from the module path). The fix was to keep `response_parsing_tests` as a separate top-level module via a second `#[path]` attachment (`src/clients/responses_response_parsing_tests.rs`), preserving the original module path and all existing `*.snap` filenames with zero snapshot churn.
  Evidence: 9 `snapshot assertion ... failed` errors referencing `cake__clients__responses__tests__response_parsing_tests__*.snap.new` files; resolved (59 responses tests pass) after de-nesting.
- Observation: `scripts/cargo-crap.sh` maintains an explicit `--exclude` list of extracted `*_tests.rs` files so the change-risk gate ignores test code. New extracted test files must be added there or their test functions surface as high-CRAP "new" entries. After excluding the seven new files, the only remaining `check-coverage` regression is `Agent::send` (Δ +0.3) in the untouched `src/clients/agent/agent_loop.rs`, which reproduces on a clean stashed tree — the committed baseline is stale/nondeterministic independent of this task.
  Evidence: `git stash` of all task changes then `scripts/cargo-crap.sh --lcov ... --format markdown` still reports the identical `Agent::send | +0.3` regression.
- Observation: The linter counts `#[cfg(test)]` items that are NOT `mod <test>` blocks (e.g. `#[cfg(test)] impl BashExecutionArgs`, `#[cfg(test)] async fn execute_bash_unsandboxed` in `bash.rs`, and `#[cfg(test)] use crate::types::Role;` in `main.rs`) as *production* lines, because its brace-counter only recognizes test *modules*. Moving those helpers into the extracted test file therefore also reduces the counted production lines.
  Evidence: `_extract_mod_name`/`_looks_like_test_mod` logic in `scripts/lint-module-size.py`.

## Decision Log

- Decision: Follow the exact established pattern — sibling `<module>_tests.rs` file whose top level is the body of `mod tests` (starting with `use super::*;`), attached via `#[cfg(test)] #[path = "<module>_tests.rs"] mod tests;`.
  Rationale: Consistency with existing extractions (`chat_completions`, `edit`, `settings`, `agent`) and zero behavior change.
  Date/Author: 2026-07-01 / Trav (via agent)
- Decision: For `responses.rs`, which has two test modules (`mod tests` and `mod response_parsing_tests`), extract each into its own top-level sibling file attached via separate `#[path]` declarations (`responses_tests.rs` and `responses_response_parsing_tests.rs`), rather than nesting the second inside the first.
  Rationale: `response_parsing_tests` uses `insta` named snapshots whose filenames are derived from the module path. Nesting would rename the module path and require renaming 9 `*.snap` files; keeping separate top-level modules preserves the exact module paths and snapshot filenames with zero churn. (This differs from `chat_completions_tests.rs`, which nests its own `response_parsing_tests` — but that one does not use named snapshots, so nesting was safe there.)
  Date/Author: 2026-07-01 / Trav (via agent)
- Decision: Leave the two `#[cfg(test)]` helpers in `bash.rs` (`impl BashExecutionArgs::with_sandbox` and `execute_bash_unsandboxed`) in place rather than moving them into `bash_tests.rs`.
  Rationale: Those helpers reference `super::ToolResult`/`super::ToolContext` where `super` is the `tools` module; moving them inside `mod tests` would change `super` to the `bash` module and require rewriting those paths. The tests still reach the helpers via `use super::*`, so leaving them is zero-risk and behavior-preserving. This is a minor deviation from the plan's original step 4.
  Date/Author: 2026-07-01 / Trav (via agent)
- Decision: Add the seven new `*_tests.rs` files to the `--exclude` list in `scripts/cargo-crap.sh` and do NOT regenerate `ci/cargo-crap-baseline.json`.
  Rationale: Excluding extracted test files from the change-risk gate matches the established maintenance pattern (the previously-extracted `agent_tests.rs`, `chat_completions_tests.rs`, `settings_tests.rs` are already excluded). The remaining `Agent::send` +0.3 regression is pre-existing coverage nondeterminism proven to reproduce on a clean tree, so regenerating the shared baseline would only re-snapshot noise and mask future real regressions; that is a maintainer decision outside this task's scope.
  Date/Author: 2026-07-01 / Trav (via agent)
- Decision: Include `hooks.rs` and `main.rs` even though their production-line flag will remain.
  Rationale: The task's goal is reducing production-file clutter without behavior change; extraction meaningfully shrinks both files. Splitting production code to get under 800 lines is out of scope for this refactor task.
  Date/Author: 2026-07-01 / Trav (via agent)
- Decision: Do not extract `src/clients/agent.rs` (already extracted) and do not touch already-extracted `*_tests.rs` files.
  Rationale: They already follow the pattern; re-splitting a large `*_tests.rs` is out of scope.
  Date/Author: 2026-07-01 / Trav (via agent)

## Outcomes & Retrospective

All six candidate modules had their inline test blocks extracted into sibling `*_tests.rs` files following the established pattern. Zero production behavior changes. The full test suite compiles and passes identically before and after. Production files are substantially shorter:

- `responses.rs`: 1854 -> 474 lines (tests -> `responses_tests.rs` 831 + `responses_response_parsing_tests.rs` 550)
- `session.rs`: 1165 -> 487 lines (tests -> `session_tests.rs` 678)
- `skills.rs`: 1065 -> 636 lines (tests -> `skills_tests.rs` 424)
- `bash.rs`: 1208 -> 524 lines (tests -> `bash_tests.rs` 682; two tiny `#[cfg(test)]` helpers left in place)
- `hooks.rs`: 1497 -> 865 lines (tests -> `hooks_tests.rs` 632)
- `main.rs`: 1999 -> 1017 lines (tests -> `main_tests.rs` 974)

`hooks.rs` and `main.rs` remain flagged for production line count (>800), as expected and documented; that is a separate concern from inline-test clutter and out of scope for this refactor.

## Context and Orientation

The repository is a Rust 2024 binary crate rooted at `src/main.rs`. Unit tests live inline in most modules under a `#[cfg(test)] mod tests { ... }` block. A Python linter, `scripts/lint-module-size.py` (run via `just lint-module-size`), reports files whose production lines exceed 800, or whose test lines exceed 800 with a test ratio over 40%. It classifies a line as "test" only if it lives inside a module whose name contains `test`; `*_tests.rs` files are counted entirely as test lines. It always exits 0.

The already-extracted modules demonstrate the target shape. In `src/clients/chat_completions.rs` the file ends with:

    #[cfg(test)]
    #[path = "chat_completions_tests.rs"]
    mod tests;

and `src/clients/chat_completions_tests.rs` begins with `use super::*;` followed by the test functions, and contains a nested `mod response_parsing_tests { use super::*; ... }` at its end. `src/clients/agent.rs` uses a subdirectory variant: `#[path = "agent/agent_tests.rs"] mod tests;`. This plan uses the flat sibling variant (`<module>_tests.rs` next to `<module>.rs`) for all files except where a subdirectory already exists.

Candidate files and their inline test structure (line numbers approximate, verify before editing):

- `src/clients/responses.rs`: `#[cfg(test)] mod tests` at ~468 and a second `#[cfg(test)] mod response_parsing_tests` at ~1304, running to EOF.
- `src/types/session.rs`: single `#[cfg(test)] mod tests` at ~485 to EOF.
- `src/config/skills.rs`: single `#[cfg(test)] mod tests` at ~638 to EOF.
- `src/clients/tools/bash.rs`: test helpers `#[cfg(test)] impl BashExecutionArgs` (~58) and `#[cfg(test)] async fn execute_bash_unsandboxed` (~441) in the production area, plus `#[cfg(test)] mod tests` at ~522 to EOF.
- `src/hooks.rs`: single `#[cfg(test)] mod tests` at ~863 to EOF.
- `src/main.rs`: a test-only `#[cfg(test)] use crate::types::Role;` at ~34, plus `#[cfg(test)] mod tests` at ~1017 to EOF.

## Plan of Work

For each candidate file, perform a mechanical, behavior-preserving extraction:

1. Identify the exact byte/line boundaries of the inline `#[cfg(test)] mod <name> { ... }` block(s) (from the `#[cfg(test)]` attribute line through the module's closing brace at EOF).
2. Create the sibling `<module>_tests.rs` file. Its content is the *body* of `mod tests` (everything between the outer `{` and its matching `}`), preserving the leading `use super::*;` and all inner code verbatim.
3. For a file with a second sibling test module (only `responses.rs`), append that second module into the new file as a nested `mod <name> { ... }` (keeping its own inner `use` statements), matching the `chat_completions_tests.rs` precedent.
4. For files with `#[cfg(test)]` test *helpers* outside the test module (only `bash.rs`: `impl BashExecutionArgs` extension and `execute_bash_unsandboxed`), move those helper items into the extracted test file as well, since they exist solely to support the tests. Keep the `#[cfg(test)]` attributes off inside the test file (the whole file is already test-only via the `#[cfg(test)] mod tests;` attachment) — i.e. drop the now-redundant `#[cfg(test)]` on moved helpers.
5. In the production file, replace the removed inline module(s)/helpers with the single attachment:

       #[cfg(test)]
       #[path = "<module>_tests.rs"]
       mod tests;

6. For `main.rs`, also remove the test-only `#[cfg(test)] use crate::types::Role;` from the production section and add `use crate::types::Role;` inside `src/main_tests.rs` (the moved tests need it).
7. Run `cargo fmt` and `cargo check --tests`, then the targeted tests for the touched module.

Do the work in the batch order listed in `Progress`, verifying after each batch. Prefer the smallest possible diff to the production file (ideally only the removed block replaced by the 3-line attachment).

## Concrete Steps

Working directory for all commands: `/Users/travisennis/Projects/cake`.

Baseline (capture the passing test count before any change):

    cargo test 2>&1 | tail -20

For each batch, after editing:

    cargo fmt
    cargo check --tests
    cargo test <module-path> 2>&1 | tail -20   # e.g. cargo test responses

After all batches:

    cargo fmt
    just lint-module-size
    just ci

Expected: `cargo check --tests` succeeds after each batch; the total passing test count from `cargo test` is unchanged from baseline; `just ci` passes.

## Validation and Acceptance

Acceptance is behavior-preserving relocation:

- Before and after the full change, `cargo test` reports the same number of passing tests (no tests added or removed).
- Each production file shrinks by roughly the size of its extracted test block (verify with `wc -l`).
- `cargo check --tests` passes after every batch.
- `cargo fmt`, `just lint-module-size`, and `just ci` pass before handoff.
- Each new `*_tests.rs` file begins with `use super::*;` and contains the relocated tests; each production file ends with the `#[cfg(test)] #[path = "..."] mod tests;` attachment.

## Idempotence and Recovery

Each batch is independent and additive-then-subtractive: create the new file, then remove the inline block. If `cargo check --tests` fails after a batch, the failure is localized to that file — inspect the diff for a mis-copied brace or a missing `use`. To roll back a single batch, restore the production file and delete the new `*_tests.rs` file (both are tracked by git). Re-running the extraction on an already-extracted file is a no-op guard: if the production file already ends with the `mod tests;` attachment, skip it.

## Artifacts and Notes

Target attachment shape appended to each production file:

    #[cfg(test)]
    #[path = "responses_tests.rs"]
    mod tests;

New sibling file shape (`src/clients/responses_tests.rs`):

    use super::*;
    // ... relocated #[test] fns from the original `mod tests` ...

    mod response_parsing_tests {
        use super::*;
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        // ... relocated #[test] fns ...
    }

## Interfaces and Dependencies

No public interfaces change. No dependencies change. The only structural additions are new `#[cfg(test)]`-gated sibling modules attached via `#[path]`. Module test paths shift only where a second test module becomes nested (e.g. `crate::clients::responses::response_parsing_tests` becomes `crate::clients::responses::tests::response_parsing_tests`); this affects nothing observable because these are `#[cfg(test)]` unit tests referenced by name only within the crate.
