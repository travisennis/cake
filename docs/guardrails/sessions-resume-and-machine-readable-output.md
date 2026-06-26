# Sessions, Resume, And Machine-Readable Output

## Scope

Read this before changing JSONL session records, session lifecycle behavior, continue/resume/fork, telemetry sidecars, transcript persistence, `json` output, or `stream-json` output.

## Compatibility Surfaces

- `{uuid}.jsonl` session file shape and append-only expectations.
- Session selection by working-directory header and modification time.
- `task_start` to conversation records to `task_complete` sequencing.
- `StreamRecord` and machine-readable stdout schemas.
- Telemetry record shape and timing fields.

## Required Checks

- Add or update tests around session load/save and output serialization.
- Run snapshot tests when JSON records or serialized conversation data change.
- Preserve partial recovery and atomic write behavior unless explicitly changed.

## Common Failure Modes

- Breaking old session files by tightening deserialization too much.
- Polluting machine-readable stdout with progress or human-readable text.
- Changing resume/fork behavior while only testing new sessions.
- Updating session records without updating stream-json docs or snapshots.

## Related Docs

- [session-management.md](../design-docs/session-management.md)
- [streaming-json-output.md](../design-docs/streaming-json-output.md)
- [conversation-types.md](../design-docs/conversation-types.md)
- [ADR 004: Append-Only Session Task Events](../adr/004-append-only-session-task-events.md)
- [ADR 007: Per-Session Telemetry Sidecar](../adr/007-per-session-telemetry-sidecar.md)
