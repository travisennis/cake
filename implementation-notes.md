# Implementation Notes

## Ticket 119: Per-Session Telemetry Sidecar

- Design decision: API request timing is captured in `AgentRunner`, because that is where HTTP send, response parsing, and retry decisions are already centralized. `Agent` still owns the optional writer and best-effort disable behavior so telemetry persistence stays session-scoped.
- Design decision: telemetry uses crate-local event structs for runner/tool events and converts them into `SessionTelemetryRecord` at the edge where `session_id` and `invocation_id` are known.
- Design decision: `session_id` and `invocation_id` are serialized as strings rather than enabling serde support on the `uuid` dependency. This matches existing transcript records and avoids changing dependency features for a serialization-only need.
- Design decision: added ADR 007 because the sidecar is a durable artifact and storage contract.
- Open question: none yet.

## Deslop Review

### How did we do?

The implementation matches the task and ExecPlan: telemetry is separate from transcripts, append-only, best-effort, skipped for `--no-session`, and documented.

### Feedback to keep

- Keep request timing in `AgentRunner`, where request, parse, and retry outcomes are observable.
- Keep UUIDs serialized as strings to match transcript records and avoid changing dependency features.
- Add ADR 007 because the sidecar is a durable artifact contract.
- Construct tool telemetry with the known turn index before launching tool futures.

### Feedback to ignore

- Do not add a generic telemetry trait or metrics abstraction; the current optional writer is enough for this feature.

### Plan of attack

Applied the turn-index simplification, added ADR 007, updated task/ExecPlan notes, and reran focused validation.

### Deslop compliance

- Root AGENTS.md: read from the prompt.
- Nested AGENTS.md: none found under changed paths.
- Task context: read `.agents/TASKS.md`, `.agents/.tasks/index.md`, and task `119`.
- Plans: read `.agents/PLANS.md`.
- Design docs: read `docs/design-docs/index.md`, `docs/design-docs/session-management.md`, and `docs/design-docs/logging.md`.
- ADRs: read `docs/adr/README.md` and relevant ADR 004; added ADR 007.
- ExecPlan: read and updated `.agents/exec-plans/active/per-session-telemetry-plan.md`.
- Changed files and diff: reviewed `git diff --stat` and targeted code diffs.
- Validation: ran `cargo fmt`, `cargo check --tests`, `cargo test session_telemetry`, `cargo test test_no_session_prevents_session_save -- --exact --nocapture`, and `just ci`.
