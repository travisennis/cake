# Strengthen Edit Tool Semantics With a Lean-Assisted Test Pass

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `.agents/PLANS.md` from the repository root. Any contributor implementing this plan must keep this file self-contained and update it whenever the implementation changes direction, discovers new behavior, or completes a milestone.

## Purpose / Big Picture

Cake's `Edit` tool rewrites user files using exact literal search-and-replace. A mistake in the multi-edit algorithm can silently corrupt files, especially when several replacements occur in one call and earlier replacements would otherwise shift the byte positions of later replacements. This plan uses Lean once as a learning and strengthening exercise: build a small formal model of the edit algorithm, use it to clarify the invariants, and convert what is learned into stronger Rust tests.

After this work, the project should have better confidence that the `Edit` tool validates and applies multiple literal edits correctly. The durable project artifact is not a permanent Lean dependency. The durable artifact is stronger Rust test coverage plus documentation explaining how Lean was used so the experiment can be repeated later if it proves useful.

What someone can do after this change that they could not do before:

- Run focused Rust tests that exercise the pure multi-edit algorithm independently of filesystem, JSON parsing, diff output, and path validation.
- Read a checked-in note describing the formal-methods workflow used for this experiment and repeat it on another small algorithm in the project.
- Inspect the Lean prototype, if it is retained, as a research artifact that explains the key equivalence between reverse-order replacement and simultaneous replacement against the original content.

How to verify it works:

- Run `cargo test edit` from `/Users/travisennis/Projects/cake` and observe the new edit semantics tests pass.
- Run `just ci` from `/Users/travisennis/Projects/cake` and observe the full project check pass.
- Read the final documentation added by this plan and confirm it explains the Lean workflow, the model scope, the properties considered, and lessons learned.

## Progress

- [x] (2026-05-01T00:39:56Z) Created this ExecPlan from the current code and the decision to use Lean once for learning and test strengthening rather than adding it as a permanent dependency.
- [ ] Extract or expose the pure edit validation/application core in `src/clients/tools/edit.rs` without changing user-visible `Edit` tool behavior.
- [ ] Add conventional unit tests and property-style tests around the pure edit core.
- [ ] Build a small Lean model of non-overlapping replacement and document the properties it checks.
- [ ] Translate useful Lean insights into Rust fixtures or property tests.
- [ ] Document the reusable workflow and lessons learned.
- [ ] Run `cargo fmt`, focused Rust tests, and `just ci`.

## Surprises & Discoveries

- Observation: The current `Edit` implementation already separates validation and application enough to make this plan feasible.
  Evidence: `preflight_edits` validates exact single matches and overlap in `src/clients/tools/edit.rs`; `apply_edits_reverse_order` performs replacements from highest byte index to lowest byte index.

## Decision Log

- Decision: Use Lean as a one-time research and test-strengthening aid for this task, not as a permanent CI dependency.
  Rationale: This keeps the experiment small and useful while avoiding a long-term formal-methods toolchain commitment before the project has evidence that it is worth maintaining.
  Date/Author: 2026-05-01, Codex

- Decision: Focus on the `Edit` tool rather than Bash safety, path validation, or session parsing.
  Rationale: The edit algorithm is small, pure, safety-sensitive, and has clear invariants. Bash safety depends on shell parsing heuristics, path validation depends on filesystem and OS canonicalization behavior, and session parsing is more schema-oriented than algorithmically subtle.
  Date/Author: 2026-05-01, Codex

- Decision: Model replacement over an abstract list of symbols in Lean before attempting to model Rust strings or byte offsets directly.
  Rationale: The important algorithmic property is about ordered, non-overlapping ranges. A list model makes that property easier to understand and avoids accidental complexity from UTF-8, filesystem IO, serde, and diff generation.
  Date/Author: 2026-05-01, Codex

## Outcomes & Retrospective

No implementation has started yet. At completion, update this section with what the Lean model clarified, which Rust tests were added, whether any bugs or ambiguities were found, and whether future Lean-assisted passes seem worthwhile for cake.

## Context and Orientation

The `Edit` tool is one of cake's model-callable tools. It accepts JSON arguments containing a file path and an array of literal replacements. It reads the target file, validates that every requested `old_text` appears exactly once, rejects overlapping replacement ranges, applies all replacements, writes the result back to disk, and returns a unified diff.

The central implementation is in `src/clients/tools/edit.rs`. `execute_edit` is the user-facing tool implementation. It parses JSON, checks edit count, rejects no-op edits, validates the path through `validate_path_for_write`, rejects binary or invalid UTF-8 files, preserves UTF-8 BOM and line endings, calls `preflight_edits`, calls `apply_edits_reverse_order`, writes the file, and generates a diff.

`preflight_edits` in `src/clients/tools/edit.rs` searches the normalized file content for each edit's `old_text`. It rejects an edit if the text is missing or appears more than once. It records each match as a byte index and length, sorts matches by index, and rejects overlapping ranges.

`apply_edits_reverse_order` in `src/clients/tools/edit.rs` receives validated matches and applies them from highest starting index to lowest starting index. This reverse order is meant to avoid position shifts: replacing text near the end of the file first cannot change the byte indices of text earlier in the file.

`src/clients/tools/mod.rs` contains shared path validation. This plan does not change path validation except as needed to keep `execute_edit` using the same behavior after the pure edit logic is extracted.

Key terms:

- Literal replacement: an edit that searches for exact text and replaces that exact text with new text. It is not a regular expression.
- Pure function: a function whose output depends only on its input and that does not read files, write files, inspect environment variables, or mutate global state.
- Invariant: a rule that must remain true for the algorithm to be correct. For this work, one invariant is that accepted edit ranges do not overlap.
- Formal model: a smaller mathematical version of the algorithm written in Lean. It intentionally leaves out production details that are not needed to reason about the core behavior.
- Oracle: a trusted reference used by tests to decide whether the production implementation behaved correctly.
- Simultaneous replacement: the conceptual result of applying all edits to the original content at once, so no replacement can shift another replacement's original position.

Key files:

- `src/clients/tools/edit.rs`: The production `Edit` tool implementation and current unit tests.
- `src/clients/tools/mod.rs`: Shared tool definitions and path validation used by `Edit`.
- `Cargo.toml`: Rust dependency manifest. Add a Rust test dependency here only if property testing requires one.
- `.agents/.research/topics/provers.md`: Existing research summary that motivates using a small formal model as a practical bug-finding and test-strengthening tool.
- `.agents/exec-plans/active/edit-tool-lean.md`: This plan.

## What We're NOT Doing

This plan does not add Lean as a required project dependency in normal development or CI unless the experiment produces a clear reason to revisit that decision.

This plan does not attempt to prove the compiled Rust implementation correct. The Lean work models the critical edit algorithm and informs Rust tests.

This plan does not redesign the `Edit` tool's user-facing JSON schema, diff output, path validation, binary-file detection, or line-ending policy.

This plan does not formalize Bash command safety, sandbox behavior, session persistence, API clients, or the full agent loop.

This plan does not require committing generated Lean build artifacts or downloaded dependencies.

## Implementation Approach

Proceed in small, verifiable steps. First isolate the pure edit algorithm in Rust so it can be tested without files. Then add conventional and property-style tests that encode the behavior expected today. Next, build a small Lean model over lists and ranges to reason about non-overlapping replacements. Finally, convert any useful insight from the Lean model into Rust tests and write down how the experiment worked.

The implementation should preserve existing `Edit` tool behavior. Existing tests in `src/clients/tools/edit.rs` should continue to pass. New tests should focus on the algorithmic core: exact-match validation, duplicate-match rejection, overlap rejection, reverse-order application, and preservation of untouched content.

The Lean artifact can live outside the production build path. A suitable location is `.agents/.research/edit-tool-lean/` because the current decision is to treat Lean as research support rather than product code. If the experiment later becomes a maintained workflow, a future plan can move the model to a more formal location and add documented toolchain setup.

## Milestones

### Milestone 1: Extract the Pure Edit Core

Overview: This milestone makes the edit algorithm directly testable without involving filesystem IO. At the end, `execute_edit` should still behave the same way, but its core validation and replacement logic should be callable from unit tests with a string and a list of edits.

Repository Context: Work in `src/clients/tools/edit.rs`. The existing `Edit` and `MatchedEdit` types are private to the module. That is acceptable because the initial tests can live in the same module. If the function signatures become clearer with a small private error enum, add it in this file only.

Plan of Work: Introduce a private pure function that accepts normalized content and edits, validates them with the existing preflight logic, applies them with the existing reverse-order application, and returns the modified normalized content. Keep `execute_edit` responsible for JSON parsing, path validation, file reading, BOM handling, line-ending normalization/restoration, writing, and diff generation. Refactor by moving existing calls rather than rewriting the algorithm.

The intended function shape is:

    fn apply_literal_edits_to_normalized_content(
        content: &str,
        edits: &[Edit],
        path: &Path,
    ) -> Result<(String, usize), String>

The function may return the modified content and the number of applied edits. If implementation reveals a cleaner name or return type, update this plan's Decision Log before changing it.

Interfaces and Dependencies: No new external dependency is required for this milestone. Keep all new functions private unless tests outside the module require otherwise.

Concrete Steps:

Working directory: `/Users/travisennis/Projects/cake`

Run the focused existing tests before editing:

    cargo test clients::tools::edit

Expected outcome: existing edit tests pass. If the exact module path does not match Rust's test filter behavior, use:

    cargo test edit

Refactor `src/clients/tools/edit.rs` so `execute_edit` calls the new pure function after normalizing content and before restoring line endings. Run:

    cargo fmt
    cargo test edit

Validation and Acceptance: Existing behavior is preserved if all current edit tests pass and no expected output changes except internal function organization. The new pure function should be covered by at least one direct unit test that does not create a temporary file.

Idempotence and Recovery: This refactor is safe to repeat. If tests fail, compare the old inline sequence in `execute_edit` with the new helper call and ensure BOM and line-ending restoration still happen outside the pure function.

Artifacts and Evidence: Record a short transcript in this plan after completion, for example:

    running N tests
    test clients::tools::edit::tests::multiple_edits_in_single_call ... ok
    test result: ok. N passed; 0 failed

Success Criteria:

Automated verification:

- [ ] `cargo fmt` completes without changing unrelated files.
- [ ] `cargo test edit` passes.
- [ ] At least one direct pure-function unit test exists in `src/clients/tools/edit.rs`.

Manual verification:

- [ ] Review `execute_edit` and confirm path validation, binary rejection, UTF-8 validation, BOM preservation, line-ending preservation, file write, and diff generation remain in the user-facing flow.

Implementation Note: After completing this milestone and automated verification, pause for manual confirmation before proceeding if a human is supervising the implementation.

### Milestone 2: Add Rust Tests That State the Edit Contract

Overview: This milestone turns the intended semantics into executable Rust tests. At the end, the project should have tests that clearly state what accepted and rejected multi-edit inputs mean.

Repository Context: Work primarily in `src/clients/tools/edit.rs`. If adding property-based tests, update `Cargo.toml` with a dev-dependency such as `proptest`. A property-based test generates many inputs automatically and checks that a stated rule always holds.

Plan of Work: Add direct tests for the pure edit core. Keep existing filesystem-level tests because they verify integration behavior. Add tests for missing text, duplicate matches, no-op edits, overlapping edits, adjacent non-overlapping edits, replacements that grow content, replacements that shrink content, and ordering. If the intended contract is that the order of independent edits does not matter, add a test for that. If the implementation intentionally preserves original edit order for error reporting only, document that in the test names and Decision Log.

For a reference implementation, use a simple helper in tests that takes already-known non-overlapping ranges and constructs the simultaneous replacement result from left to right. This helper should be deliberately straightforward, even if less efficient, because it serves as an oracle for tests.

Interfaces and Dependencies: Prefer ordinary unit tests first. Add `proptest` only if hand-written tests leave too many edge cases uncovered. If `proptest` is added, keep strategies small and readable.

Concrete Steps:

Working directory: `/Users/travisennis/Projects/cake`

Run:

    cargo test edit

Add tests in the `#[cfg(test)]` module in `src/clients/tools/edit.rs`. If adding `proptest`, update `Cargo.toml` under `[dev-dependencies]` and run:

    cargo test edit

Validation and Acceptance: The tests should fail if `apply_edits_reverse_order` is changed to apply edits from low index to high index without adjusting indices. The tests should also fail if overlap detection is removed or duplicate matches are accepted.

Idempotence and Recovery: Adding tests is additive. If a generated property test fails with a hard-to-understand case, save the minimal failing input as a normal unit test before adjusting the property.

Artifacts and Evidence: Record any failing case found during development in `Surprises & Discoveries`, including the input content, edits, expected result, and observed result.

Success Criteria:

Automated verification:

- [ ] `cargo test edit` passes.
- [ ] New tests cover accepted adjacent edits, rejected overlapping edits, rejected duplicate matches, growing replacements, shrinking replacements, and edit ordering.
- [ ] If `proptest` is added, generated cases are deterministic enough for CI and failures print useful minimal examples.

Manual verification:

- [ ] Test names and assertions read like a clear contract for future maintainers.

Implementation Note: After completing this milestone and automated verification, pause for manual confirmation before proceeding if a human is supervising the implementation.

### Milestone 3: Build a Small Lean Model

Overview: This milestone uses Lean to model the algorithm at the right abstraction level. At the end, there should be a small Lean research artifact that models non-overlapping replacements and records what was proved, attempted, or learned.

Repository Context: Create a research directory such as `.agents/.research/edit-tool-lean/`. Do not add Lean files to production source directories. Do not add Lean execution to `just ci` in this milestone.

Plan of Work: Model content as a list of abstract symbols rather than Rust UTF-8 strings. Model an edit as a start index, a length, and replacement content. Define what it means for edits to be sorted and non-overlapping. Define replacement in two ways: reverse-order application and simultaneous left-to-right reconstruction. Prove, or at minimum machine-check through examples while documenting the gap, that reverse-order application on sorted non-overlapping edits produces the same result as simultaneous replacement.

The model does not need to represent search uniqueness. Search uniqueness is a Rust-side validation concern. The Lean model should focus on the application theorem after validation has produced concrete non-overlapping ranges.

Interfaces and Dependencies: Use Lean 4 if available locally. If Lean is not installed, document the blocker and optionally create pseudocode or comments describing the intended model. Do not install global toolchains without explicit approval.

Concrete Steps:

Working directory: `/Users/travisennis/Projects/cake`

Check whether Lean is available:

    lean --version

If Lean is available, create `.agents/.research/edit-tool-lean/` and add a small Lean file, for example `.agents/.research/edit-tool-lean/EditModel.lean`. Keep it independent of lake packages unless a package becomes clearly necessary.

If Lean is not available, stop before installing anything and record the blocker in `Surprises & Discoveries`. Ask the human whether to install Lean or continue with a paper model and Rust tests only.

Validation and Acceptance: The Lean file should be understandable to a Rust contributor who is new to Lean. It should contain comments explaining how each definition maps back to `src/clients/tools/edit.rs`.

Idempotence and Recovery: The research directory is additive. If the Lean proof becomes too time-consuming, keep the examples and partial definitions, then explicitly document what was learned and what remains unproved.

Artifacts and Evidence: Record the `lean --version` output and any successful Lean check command. If the proof is partial, include the exact theorem statement that remains unfinished.

Success Criteria:

Automated verification:

- [ ] If Lean is available, the Lean model file checks with the local Lean command chosen for the artifact.
- [ ] If Lean is unavailable, the plan records that no Lean check was run and why.

Manual verification:

- [ ] The Lean artifact or fallback note explains the mapping from abstract ranges to Rust's `MatchedEdit` fields: `index`, `match_length`, and `new_text`.
- [ ] The artifact states clearly whether a proof was completed, partially completed, or deferred.

Implementation Note: After completing this milestone and automated verification, pause for manual confirmation before proceeding if a human is supervising the implementation.

### Milestone 4: Convert Lean Insights Into Rust Tests and Documentation

Overview: This milestone turns the learning into durable project value. At the end, cake should have stronger Rust tests and a short reusable guide explaining how Lean was used.

Repository Context: Update `src/clients/tools/edit.rs` tests with any edge cases discovered from the model. Add documentation in a durable location, preferably `docs/design-docs/edit-tool-formal-model.md` or `.agents/.research/edit-tool-lean/README.md`. If the guidance is meant for future contributors rather than only this experiment, prefer `docs/design-docs/edit-tool-formal-model.md`.

Plan of Work: Compare the Lean model to the Rust test suite. For each property modeled in Lean, ensure there is either a Rust test or a documented reason it is not applicable to production code. Write the workflow documentation in plain language: why this target was chosen, how the pure function was extracted, how the abstract model was scoped, what commands were run, what was learned, and what should be done differently next time.

Interfaces and Dependencies: No new runtime dependency is needed. If a test dependency was added, keep it under `[dev-dependencies]`.

Concrete Steps:

Working directory: `/Users/travisennis/Projects/cake`

Run focused tests:

    cargo test edit

Run full validation:

    just ci

Expected outcome: `just ci` completes successfully. The project AGENTS instructions require running the full CI check before finishing code changes.

Validation and Acceptance: A future contributor should be able to read the documentation and understand how to repeat a one-time Lean-assisted pass on another small algorithm without needing this conversation.

Idempotence and Recovery: Documentation and tests are additive. If `just ci` fails for an unrelated existing issue, record the exact failure and the evidence that focused edit tests passed.

Artifacts and Evidence: Add concise command output to this plan after running validation:

    cargo test edit
    test result: ok. N passed; 0 failed

    just ci
    ...
    finished successfully

Success Criteria:

Automated verification:

- [ ] `cargo fmt` passes.
- [ ] `cargo test edit` passes.
- [ ] `just ci` passes, or the plan records a precise unrelated blocker.
- [ ] Documentation exists describing the Lean-assisted workflow and lessons learned.

Manual verification:

- [ ] Read the documentation and confirm it includes enough detail to repeat the workflow.
- [ ] Confirm Lean remains outside required CI unless a new explicit decision is recorded.

Implementation Note: After completing this milestone, update `Outcomes & Retrospective` with what was achieved and whether Lean seems worth using again in cake.

## Testing Strategy

Unit tests should cover the pure edit core directly. These tests should not create temporary files and should not depend on path validation. They should use small input strings whose expected outputs are obvious.

Integration-style tests should keep using the existing `execute_edit` path with temporary files. These tests prove that JSON parsing, file reading, line-ending preservation, BOM preservation, writing, and diff generation still work around the pure core.

Property-style tests, if added, should generate simple ASCII content and non-overlapping ranges. Keep the generator constrained so failures are easy to understand. When a generated failure reveals a bug or ambiguity, preserve the minimized case as a normal unit test.

Manual testing consists of reviewing the test names, documentation, and the final structure of `execute_edit`. Because this is a CLI internals change, there is no need to start a server or run an interactive UI.

## Performance Considerations

The refactor should not change the asymptotic behavior of the `Edit` tool. The current algorithm searches each `old_text` in the content, sorts matches, checks adjacent overlaps, and applies replacements by rebuilding strings. This plan is about correctness and testability, not performance.

If property tests are added, keep generated case sizes small enough that `cargo test edit` remains fast. If a property test noticeably slows the suite, reduce case count or input size and record the tradeoff in the Decision Log.

## Migration Notes

There is no data migration. Existing users should see the same `Edit` tool behavior and the same JSON schema. Any change in error text should be avoided unless it clarifies behavior and existing tests are updated intentionally.

## Rollback Plan

If the refactor causes unexpected instability, revert only the extraction while keeping any valuable new tests that can still target existing functions. If the Lean model becomes too expensive, stop after Milestone 2 and document the lesson. The plan's value does not depend on forcing a complete formal proof.

Do not use destructive git commands to roll back work. Prefer small manual patches or a normal non-destructive revert commit if the human explicitly asks for one.

## References

- Existing prover research: `.agents/.research/topics/provers.md`
- ExecPlan rules: `.agents/PLANS.md`
- Production edit implementation: `src/clients/tools/edit.rs`
- Shared tool path validation: `src/clients/tools/mod.rs`
- Full project validation command: `just ci`

## Revision History

- 2026-05-01T00:39:56Z: Initial ExecPlan created. Reason: capture the agreed approach of using Lean once to strengthen `Edit` tool tests and document the reusable workflow without making Lean a permanent project dependency.
- 2026-05-07: Moved this active ExecPlan from `.agents/.plans/` to `.agents/exec-plans/active/` during the ExecPlan directory migration.
