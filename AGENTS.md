# Agent Instructions

## Project

Cake is a Rust 2024 binary-only AI coding assistant CLI with sandboxed tool execution, persisted sessions, and OpenAI-compatible Chat Completions and Responses API backends.

The code, tests, snapshots, CLI help, and `justfile` are the implementation authorities. Durable prose exists only for user guidance, external contracts, security boundaries, architectural intent, contributor workflow, and decision history.

## Operating loop

1. Run `ahm prime` before any work.
2. If the request names a task, ExecPlan, ADR, or research record, inspect it through `ahm` before choosing implementation work.
3. Read the smallest relevant code and tests. Load durable documentation only when the change affects its audience or contract.
4. Preserve compatibility unless the task explicitly changes it.
5. If work is managed, start and complete it through `ahm`.
6. Make surgical edits and run risk-proportionate checks.
7. After implementation edits, run a review in a subagent until no actionable findings remain, then perform preflight.
8. Hand off changes, exact checks, skipped checks, and remaining risk.

Large or cross-cutting work requires an ExecPlan as directed by `ahm context plan`.

## Compatibility and risk

Treat these as compatibility surfaces:

- CLI flags, defaults, exit codes, stdout, and stderr;
- tool schemas, execution semantics, scheduling, and model-visible errors;
- sandbox policies, allowed paths, and trusted-extension boundaries;
- session JSONL, JSON and stream-json output, hooks, and toolbox protocols;
- settings keys, precedence, providers, request shaping, and retry behavior;
- prompt construction, AGENTS.md discovery, skills, and hook effects.

Protect compatibility with focused tests and snapshots. Security-boundary changes require explicit impact analysis and platform-specific verification. Serialized-format changes require compatibility analysis. Dependency changes require explicit scope and `Cargo.toml`/`Cargo.lock` consistency.

## Repository rules

- Do not commit or push unless explicitly asked.
- Work on the current branch unless asked to create another.
- Preserve unrelated user changes; never clean or revert them.
- Use Conventional Commits when writing commit messages; verified by the commit-msg hook.
- Never hand-edit generated `.ahm` indexes.
- Future work and unresolved questions belong in `.ahm`, not durable docs.
- Update architecture documentation only when a durable boundary or invariant changes, not when symbols or files move.
- Before broad edits and before handoff, inspect `git status --short`.

## Verification

Use focused checks first. `just ci` is the normal code-change gate; documentation-only work uses targeted Panache format/lint checks for changed living documents, link validation, `ahm doctor`, and `git diff --check`. Follow [CONTRIBUTING.md](CONTRIBUTING.md) for exceptions and specialized checks.
