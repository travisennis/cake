# Implementation Notes

## Ticket 102: Validate Stream Hook Record Contract

- Design decision: hook event records use a shared `HookEventData` struct for both `SessionRecord` and `StreamRecord` so the persisted and streamed wire shapes stay identical.
- Design decision: `HookRunner` emits through a thread-safe hook event sink supplied by the CLI. The sink writes session JSONL when a session writer exists and writes stream-json when requested, which keeps hook events visible even with `--no-session`.
- Tradeoff: the hook sink mirrors the agent observer fan-out instead of borrowing `AgentObserver` directly, because hooks run through an `Arc<HookRunner>` and may execute concurrently around tool calls.
- Open question: none yet.

## Deslop Review for Ticket 102

### How did we do?

The implementation matches the task decision: hook events now share one typed record shape across session JSONL and stream-json, remain non-conversation records for replay, and are visible live even when sessions are disabled.

### Feedback to keep

- Keep `HookEventData` as the single source of truth for hook event fields.
- Keep the hook event sink thread-safe and best-effort, matching the previous hook transcript behavior while adding stream-json emission.
- Keep `session_meta`, `prompt_context`, and `skill_activated` out of stream-json; only hook events needed to become live task events for this ticket.

### Feedback to ignore

- Do not refactor all live output through a larger observer abstraction in this task; that would widen the change beyond the stream hook contract.

### Plan of attack

No additional code changes were needed after review. Focused tests and formatting checks were rerun before the final CI pass.

### Deslop compliance

- Root AGENTS.md: read.
- Nested AGENTS.md: none found under changed paths.
- Task context: read `.agents/TASKS.md`, `.agents/.tasks/index.md`, and task `102`.
- Plans: read `.agents/PLANS.md`; no ExecPlan required because task `102` is effort M.
- Design docs: read `docs/design-docs/index.md`, `streaming-json-output.md`, `session-management.md`, `conversation-types.md`, and `hooks.md`.
- ADRs: updated relevant ADRs `004` and `005`.
- ExecPlan: not applicable because task `102` has no active ExecPlan.
- Changed files and diff: reviewed `git diff --stat` and targeted changed-file diffs.
- Validation: ran `cargo fmt`, `cargo fmt --check`, `cargo check --tests`, and focused hook/schema tests; `just ci` still required before commit.

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
