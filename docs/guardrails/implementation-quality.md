# Implementation Quality

## Scope

Read this before refactors, behavior-preserving cleanup, error-handling changes, module moves, lint posture changes, or maintainability work that is not owned by a narrower risk guardrail.

## Compatibility Surfaces

- Module boundaries and dependency direction.
- Public CLI, config, session, provider, tool, and output behavior.
- Error messages when user-facing or model-facing.
- Lint guarantees, including no production `unwrap`/`expect` and absolute `crate::` imports.

## Required Checks

- Keep refactors behavior-preserving unless the task explicitly says otherwise.
- Run focused tests covering moved code and any changed error paths.
- Update `ARCHITECTURE.md` and implementation-location references when moving responsibilities between modules.

## Common Failure Modes

- Mixing cleanup with behavior change so review cannot isolate risk.
- Adding abstractions before duplication or complexity justifies them.
- Moving code without updating codemaps, docs, or tests that encode location.
- Silencing lints instead of fixing the issue.

## Related Docs

- [ARCHITECTURE.md](../../ARCHITECTURE.md)
- [CONTRIBUTING.md](../../CONTRIBUTING.md)
