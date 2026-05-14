# Completed ExecPlans

This index follows `.agents/PLANS.md`. Completed ExecPlans are plans whose work is complete, or historical plans that have been explicitly closed as superseded with a retrospective.

- `agent-skills-implementation-plan.md` - Implement agent skills discovery, prompt disclosure, configuration, activation, and documentation.
- `append-only-session-management.md` - Refactor session management to append-only task events.
- `consolidate-conversation-serialization.md` - Consolidate conversation API, stream, and session serialization paths.
- `hooks-implementation-plan.md` - Add command hooks to cake.
- `lazy-skill-body-loading.md` - Implement lazy skill body loading at activation time.
- `normalize-session-optional-fields.md` - Normalize legacy missing conversation timestamps when sessions are loaded.
- `provider-strategy.md` - Move provider-specific request quirks behind a shared strategy layer.
- `refactor-coding-assistant-run.md` - Refactor `CodingAssistant::run` into named orchestration steps.
- `refactor-chat-build-messages.md` - Refactor Chat Completions message construction state handling.
- `reasoning-plan.md` - Add reasoning effort and budget configuration.
- `retry-strategy-plan.md` - Retry transient API failures intelligently.
- `schema-unification-plan.md` - Historical v3 stream/session unification plan, closed as superseded by append-only v4 sessions.
- `snapshot-testing-plan.md` - Add snapshot testing for prompts and provider request shapes.
- `split-agent-responsibilities.md` - Split the Agent facade into smaller state, observer, and backend runner responsibilities.
- `store-timestamps-as-datetime.md` - Store conversation timestamps as typed UTC DateTime values internally.

## Revision Notes

- 2026-05-07 / Codex: Created the completed ExecPlan index while migrating plans from `.agents/.plans/` to `.agents/exec-plans/completed/`.
- 2026-05-08 / Codex: Completed task 049 and moved `split-agent-responsibilities.md` into this index.
- 2026-05-09 / Codex: Completed task 052 and moved `provider-strategy.md` into this index.
- 2026-05-09 / Codex: Completed task 054 and moved `refactor-coding-assistant-run.md` into this index.
- 2026-05-10 / Codex: Completed task 061 and moved `consolidate-conversation-serialization.md` into this index.
- 2026-05-10 / Codex: Completed task 062 and moved `store-timestamps-as-datetime.md` into this index.
- 2026-05-10 / Codex: Completed task 063 and moved `normalize-session-optional-fields.md` into this index.
- 2026-05-11 / Codex: Completed task 064 and moved `refactor-chat-build-messages.md` into this index.
- 2026-05-14 / Codex: Completed task 125 and moved `lazy-skill-body-loading.md` into this index.
