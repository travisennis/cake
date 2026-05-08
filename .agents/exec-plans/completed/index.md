# Completed ExecPlans

This index follows `.agents/PLANS.md`. Completed ExecPlans are plans whose work is complete, or historical plans that have been explicitly closed as superseded with a retrospective.

- `agent-skills-implementation-plan.md` - Implement agent skills discovery, prompt disclosure, configuration, activation, and documentation.
- `append-only-session-management.md` - Refactor session management to append-only task events.
- `hooks-implementation-plan.md` - Add command hooks to cake.
- `reasoning-plan.md` - Add reasoning effort and budget configuration.
- `retry-strategy-plan.md` - Retry transient API failures intelligently.
- `schema-unification-plan.md` - Historical v3 stream/session unification plan, closed as superseded by append-only v4 sessions.
- `snapshot-testing-plan.md` - Add snapshot testing for prompts and provider request shapes.
- `split-agent-responsibilities.md` - Split the Agent facade into smaller state, observer, and backend runner responsibilities.

## Revision Notes

- 2026-05-07 / Codex: Created the completed ExecPlan index while migrating plans from `.agents/.plans/` to `.agents/exec-plans/completed/`.
- 2026-05-08 / Codex: Completed task 049 and moved `split-agent-responsibilities.md` into this index.
