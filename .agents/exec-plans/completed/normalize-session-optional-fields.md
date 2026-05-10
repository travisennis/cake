# Normalize Optional Session Fields After Deserialization

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This plan follows `.agents/PLANS.md` in the repository root. It exists because task `.agents/.tasks/063.md` has `Effort: L`, which requires an ExecPlan before implementation.

## Purpose / Big Picture

Older cake session files can contain conversation records without per-item timestamps. New sessions write timestamps for messages, tool calls, tool outputs, and reasoning records, but the persisted `SessionRecord` type still allows those fields to be absent so older JSONL files can be read. After this change, loading a session normalizes those legacy omissions: every conversation-bearing record returned by `Session::load` has a concrete timestamp. A user can see this by loading a session JSONL file with a missing message timestamp and observing that `Session::messages()` returns a `ConversationItem` whose timestamp is present.

The change is intentionally scoped to timestamps. Message `id` and `status` remain optional because absence is meaningful for user, system, developer, and Chat Completions messages.

## Progress

- [x] (2026-05-10 19:58Z) Read `.agents/TASKS.md`, `.agents/.tasks/index.md`, `.agents/.tasks/063.md`, and `.agents/PLANS.md`.
- [x] (2026-05-10 19:58Z) Inspected `src/clients/types.rs`, `src/config/session.rs`, and prior timestamp-related ExecPlans.
- [x] (2026-05-10 20:05Z) Implemented session-load normalization for missing conversation timestamps.
- [x] (2026-05-10 20:05Z) Added regression tests proving legacy missing timestamps are filled and existing timestamps are preserved.
- [x] (2026-05-10 20:11Z) Ran targeted tests and deslop, then updated design docs and task metadata.
- [x] (2026-05-10 20:13Z) Ran full CI successfully.

## Surprises & Discoveries

- Observation: Function call IDs, call IDs, names, arguments, and reasoning IDs are already strict `String` fields in `ConversationItem` and `SessionRecord`.
  Evidence: `src/clients/types.rs` defines `FunctionCall { id: String, call_id: String, name: String, arguments: String }` and `Reasoning { id: String, ... }`.
- Observation: The remaining legacy optionality on conversation-bearing records is timestamp-related.
  Evidence: `.agents/exec-plans/completed/store-timestamps-as-datetime.md` completed the move from `Option<String>` to `Option<DateTime<Utc>>` while explicitly keeping the fields optional.

## Decision Log

- Decision: Normalize missing conversation timestamps inside `Session::load` after each `SessionRecord` is deserialized, using the session metadata timestamp as the fallback.
  Rationale: Serde can deserialize a single record but does not know the surrounding session metadata. `Session::load` already owns the whole JSONL read and has a stable session timestamp, so it is the right compatibility boundary.
  Date/Author: 2026-05-10 / Codex.
- Decision: Keep message `id` and `status` as `Option<String>`.
  Rationale: Missing IDs and statuses are not purely legacy. User, system, and developer messages do not naturally have model response IDs, and Chat Completions assistant messages may not carry the Responses API status field.
  Date/Author: 2026-05-10 / Codex.

## Outcomes & Retrospective

`Session::load` now fills missing timestamps on `message`, `function_call`, `function_call_output`, and `reasoning` records with the session metadata timestamp before storing the records. This keeps legacy v4 session files resumable without rewriting them. Existing item timestamps are preserved, and message IDs/statuses remain optional because they are not always meaningful.

## Context and Orientation

The session file format is append-only JSONL. Each line is a `SessionRecord` from `src/clients/types.rs`. The first line must be `SessionRecord::SessionMeta`, which contains required metadata including the session creation timestamp. Conversation-bearing records are `Message`, `FunctionCall`, `FunctionCallOutput`, and `Reasoning`; these can be converted into in-memory `ConversationItem` values by `SessionRecord::to_conversation_item`.

`src/config/session.rs` contains `Session::load`, the function that reconstructs a `Session` from a JSONL file. This is the deserialization boundary where compatibility with old sessions belongs. `Session::messages()` filters `Session.records` through `SessionRecord::to_conversation_item`, so normalizing records during `Session::load` also normalizes the conversation history used by resume and fork flows.

## Plan of Work

First, add a helper on `SessionRecord` in `src/clients/types.rs` that can fill a missing timestamp on only the conversation-bearing variants. The helper should take a `DateTime<Utc>` fallback and mutate `self` in place. It should leave non-conversation metadata records unchanged and preserve any timestamp that is already present.

Second, update `Session::load` in `src/config/session.rs` to remember the `SessionMeta` timestamp and call the helper for every subsequent parsed record before pushing it into `records`. This keeps old sessions readable while ensuring loaded conversation records are timestamp-complete.

Third, add tests in `src/config/session.rs`. One test should write a session with a message missing `timestamp`, load it, and assert the loaded message timestamp equals the session metadata timestamp. Another test should keep the existing RFC3339 timestamp test to prove an explicit record timestamp is preserved.

Finally, run targeted tests, run deslop, run `just ci`, update `.agents/.tasks/063.md`, `.agents/.tasks/index.md`, and this plan with final status, then commit the completed work.

## Concrete Steps

From `/Users/travisennis/Projects/cake`, edit:

- `src/clients/types.rs`: add `SessionRecord::normalize_legacy_fields(&mut self, fallback_timestamp: DateTime<Utc>)`.
- `src/config/session.rs`: call that helper in `Session::load` for conversation records after deserialization.
- `src/config/session.rs` tests: add legacy timestamp normalization coverage.
- `.agents/.tasks/063.md` and `.agents/.tasks/index.md`: mark the task complete after validation.

Run:

    cargo test config::session
    just ci

Expected result: the targeted session tests pass, and `just ci` completes successfully.

## Validation and Acceptance

Acceptance is met when loading a v4 session JSONL file with a missing conversation timestamp succeeds and the returned `SessionRecord` plus `Session::messages()` conversation item both contain a concrete timestamp. Existing session files with explicit conversation timestamps must keep those exact timestamps. The full repository CI must pass with `just ci`.

## Idempotence and Recovery

The code changes are additive and safe to re-run. Tests create temporary session files under `tempfile::TempDir`, so they clean themselves up. If `just ci` fails, inspect the failing command output, fix the relevant code or test, and rerun `just ci`. Do not delete or rewrite user session files.

## Artifacts and Notes

Targeted validation passed:

    cargo test config::session
    test result: ok. 15 passed; 0 failed

Full validation passed:

    just ci
    All checks passed!

Deslop findings kept: document load-time normalization in `docs/design-docs/session-management.md` and `docs/design-docs/conversation-types.md`.

## Interfaces and Dependencies

Use the existing `chrono::{DateTime, Utc}` type already imported in `src/clients/types.rs`. Do not add a new dependency. The new helper should be public enough for `src/config/session.rs` to call through the existing `crate::clients::SessionRecord` import.
