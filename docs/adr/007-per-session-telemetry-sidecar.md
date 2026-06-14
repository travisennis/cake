---
status: accepted
date: 2026-05-18
---
# Per-Session Telemetry Sidecar

## Context

cake already persists resumable conversation transcripts under `~/.local/share/cake/sessions/` and writes process-wide tracing logs under `~/.cache/cake/`. Those artifacts do not provide a compact, session-scoped performance timeline. Debugging slow sessions or retry-heavy runs requires stitching together transcript records, daily logs, and wall-clock observations.

The new artifact must not change resume semantics, must not leak prompt or tool-output bodies beyond the existing transcript, and must remain best-effort so observability cannot break the primary CLI task.

## Decision

Persisted sessions get an append-only newline-delimited JSON telemetry sidecar at `~/.cache/cake/session-telemetry/{session_id}.ndjson`, or `$CAKE_DATA_DIR/session-telemetry/{session_id}.ndjson` when the data directory is overridden.

Every telemetry record includes `session_id`, `invocation_id`, and `timestamp`. The sidecar records operational events such as `telemetry_init`, `api_attempt`, `retry_scheduled`, `tool_call`, and `session_summary`. It stores durations, retry classifications, request override summaries, token usage, output byte counts, and success or failure status. It does not store prompt text, assistant text, or raw tool output bodies.

Telemetry sidecars are never resumable session files. `--continue`, `--resume`, `--fork`, and latest-session discovery continue to read only transcript files from `sessions/{session_id}.jsonl`. `--no-session` skips both the transcript and telemetry sidecar.

## Rationale

- **Separate responsibilities**: Conversation transcripts remain the source of truth for resume, while telemetry is operational debugging data with different retention and query patterns.
- **Session-scoped debugging**: A single sidecar can explain API attempts, retry delays, tool durations, and final outcome without filtering the global daily log.
- **Append-only durability**: Flushing each record makes interrupted or failed runs easier to diagnose.
- **Privacy and size control**: Omitting prompts and raw tool output keeps telemetry focused on timings and classifications instead of duplicating transcript content.
- **Compatibility**: Keeping telemetry outside the sessions directory prevents non-transcript files from affecting session discovery.

## Consequences

- **Positive**: Developers can inspect a session timeline with `jq` and see slow API attempts, retry reasons, tool durations, and final usage.
- **Positive**: Continue and resume invocations append to the same session sidecar while `invocation_id` separates individual CLI runs.
- **Positive**: Telemetry write failures degrade to one warning and then disable telemetry for that invocation.
- **Negative**: The cache directory now contains another artifact family that future cleanup tooling may need to understand.
- **Negative**: The sidecar intentionally cannot answer content questions; readers must use the transcript for conversation semantics.

## Alternatives Considered

- **Add telemetry records to the session transcript**: Rejected because it would mix performance logging with resumable conversation state and change the meaning of stream-json-compatible records.
- **Only enrich the daily log**: Rejected because daily logs are process-wide, rotated, and harder to query by session.
- **Buffer telemetry until task completion**: Rejected because retries, crashes, and interruptions are the cases where partial telemetry is most useful.

## References

- `.agents/.tasks/active/119.md` - Add Per-Session Telemetry Sidecar
- `.agents/exec-plans/active/per-session-telemetry-plan.md` - Implementation plan
- `docs/design-docs/session-management.md` - Session storage and telemetry sidecar documentation
- `docs/design-docs/logging.md` - Logging and telemetry locations
- `src/session_telemetry.rs` - Telemetry record schema and writer

