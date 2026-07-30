# Code complexity targets

## Scope

This guardrail records the project's cyclomatic complexity (CC) and CRAP targets, and the coverage-first refactoring workflow for reducing complexity. These targets apply to every function in the production codebase.

## Targets

  | Metric                            | Target | Applies to                                              |
  | --------------------------------- | ------ | ------------------------------------------------------- |
  | McCabe cyclomatic complexity (CC) | ≤ 10   | Most functions                                          |
  |                                   | ≤ 15   | Inherently dispatch-heavy functions                     |
  |                                   | ≤ 20   | Grandfathered structurally complex loop (`Agent::send`) |
  | CRAP score                        | ≤ 30   | Every function                                          |
  |                                   | ≤ 15   | Stretch goal for CC ≤ 10 functions with ≥ 90% coverage  |

Functions introduced or modified in a change must meet the CC target before merge. Grandfathered exceptions are documented with per-function `#[allow(clippy::cognitive_complexity)]` annotations referencing their reduction task.

## Coverage-first refactoring workflow

When reducing complexity in an existing function:

1. Write focused tests achieving ≥ 80% line and branch coverage.
2. Run `cargo-crap --lcov lcov.info` to confirm the CRAP drop.
3. Refactor by extracting sub-functions, replacing match ladders with lookup tables or typed dispatch, and reducing nesting.
4. Re-run `cargo-crap` to confirm CC target is met without regressing coverage.

## Provenance

Targets and workflow were established in task #325 (2026-07-30), accepted contingent on mechanical enforcement (see #335). The full rationale and dependent refactoring tasks are listed in #325.
