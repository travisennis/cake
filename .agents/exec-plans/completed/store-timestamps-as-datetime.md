# Store Conversation Timestamps as DateTime Values

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This plan follows `.agents/PLANS.md` from the repository root. The plan is self-contained so a contributor can resume the work from this file alone.

## Purpose / Big Picture

Cake keeps conversation history in memory, writes resumable JSONL session files, emits live `stream-json`, and converts history back into model API input. Timestamps should be real UTC time values while cake is running so code cannot accidentally treat them as arbitrary strings. After this change, conversation, session, and stream records that carry item creation times use `chrono::DateTime<Utc>` internally, while serialized JSON remains the same RFC 3339 timestamp strings that users and integrations already consume.

The behavior is observable by running the serialization and session tests. They prove old session JSON with timestamp strings still loads, new records still serialize as RFC 3339 strings, and API input ignores internal timestamps as before.

## Progress

- [x] (2026-05-10 19:33Z) Read `.agents/TASKS.md`, `.agents/.tasks/index.md`, task `062`, `.agents/PLANS.md`, root `AGENTS.md`, and the relevant timestamp code paths.
- [x] (2026-05-10 19:33Z) Created this ExecPlan because task `062` is `Effort: L` and requires a plan before implementation.
- [x] (2026-05-10 19:40Z) Updated conversation, stream, and session record timestamp fields from `Option<String>` to `Option<DateTime<Utc>>`.
- [x] (2026-05-10 19:40Z) Replaced conversation timestamp generation that eagerly called `to_rfc3339()` with `Utc::now()` values.
- [x] (2026-05-10 19:42Z) Updated tests and `docs/design-docs/conversation-types.md` to describe the internal `DateTime<Utc>` source of truth and external RFC 3339 wire format.
- [x] (2026-05-10 19:44Z) Ran deslop and applied the in-scope cleanup: removed unnecessary timestamp clones after switching to typed values.
- [x] (2026-05-10 19:47Z) Ran final `just ci`; all checks passed.

## Surprises & Discoveries

- Observation: `SessionRecord` and `StreamRecord` already use `DateTime<Utc>` for required metadata timestamps such as `session_meta`, `task_start`, `prompt_context`, `skill_activated`, and `hook_event`.
  Evidence: `src/clients/types.rs` defines those fields as `DateTime<Utc>`, while conversation-bearing variants still use `Option<String>`.

- Observation: The only remaining production timestamp string generation for conversation items is in `src/clients/agent_state.rs`, `src/clients/chat_completions.rs`, and `src/clients/responses.rs`.
  Evidence: `rg "to_rfc3339" src/clients` points at those modules for conversation item creation.

- Observation: `DateTime<Utc>` is copyable in this code path, so the typed timestamp conversion can avoid `clone()` entirely.
  Evidence: After replacing `timestamp.clone()` with `*timestamp` in `src/clients/types.rs` and direct `timestamp` reuse in response builders, `cargo test clients::types` and `cargo test config::session` passed.

## Decision Log

- Decision: Use `Option<DateTime<Utc>>` directly for optional conversation timestamps instead of adding a wrapper type.
  Rationale: `chrono` already has serde support enabled in `Cargo.toml`, required session timestamps already use this type, and keeping one timestamp type avoids unnecessary indirection.
  Date/Author: 2026-05-10 / Codex.

- Decision: Keep the persisted and streaming JSON schemas as RFC 3339 timestamp strings without bumping the session format version.
  Rationale: Serde serializes `DateTime<Utc>` to the same JSON string shape, and existing RFC 3339 session files deserialize into the typed value. This is an internal representation change, not a wire-format change.
  Date/Author: 2026-05-10 / Codex.

## Outcomes & Retrospective

Conversation-bearing `ConversationItem`, `StreamRecord`, and `SessionRecord` variants now store optional timestamps as `Option<DateTime<Utc>>`. Agent state and backend response parsing now create typed `Utc::now()` values instead of formatting strings immediately. Existing RFC 3339 JSON remains compatible through chrono serde, with an explicit session-load test covering a persisted message timestamp string. Deslop removed unnecessary timestamp clones after the type change. Final `just ci` passed.

## Context and Orientation

The key file is `src/clients/types.rs`. It defines `ConversationItem`, the in-memory enum used by the agent loop and both model backends. It also defines `SessionRecord`, the enum serialized into append-only session JSONL files, and `StreamRecord`, the enum serialized by `--output-format stream-json`.

Current required metadata timestamps are already typed as `chrono::DateTime<Utc>`. The remaining drift is on conversation-bearing variants: `Message`, `FunctionCall`, `FunctionCallOutput`, and `Reasoning` store optional item timestamps as `Option<String>`. These optional timestamps are copied through `ConversationItem`, `StreamRecord`, and `SessionRecord`.

`src/clients/agent_state.rs` creates user, developer, and tool-output conversation items during the agent loop. `src/clients/chat_completions.rs` and `src/clients/responses.rs` parse provider responses into assistant conversation items. `src/config/session.rs` loads and saves session records through serde, so compatibility with existing JSONL files depends on serde accepting the current RFC 3339 timestamp strings.

## Plan of Work

First, change the optional timestamp fields on `ConversationItem`, `SessionRecord`, and `StreamRecord` conversation variants in `src/clients/types.rs` from `Option<String>` to `Option<DateTime<Utc>>`. Keep `#[serde(skip_serializing_if = "Option::is_none")]` so absent timestamps stay omitted.

Second, update all construction sites to pass typed values. Replace `chrono::Utc::now().to_rfc3339()` with `chrono::Utc::now()` when building conversation timestamps. Where a single response timestamp is shared by multiple items, keep one `DateTime<Utc>` value and copy or clone it as needed. `DateTime<Utc>` is cheap to copy in this codebase's existing usage, but use whatever the compiler accepts without string conversion.

Third, update tests in `src/clients/types.rs`, `src/clients/agent_state.rs`, backend tests, and `src/config/session.rs` to use typed fixed timestamps. Add or adjust a compatibility test that writes a JSONL record with an RFC 3339 timestamp string and verifies `Session::load` returns a typed timestamp equal to the expected `DateTime<Utc>`.

Fourth, update `docs/design-docs/conversation-types.md` to say timestamps are `Option<DateTime<Utc>>` internally and serialize as RFC 3339 strings externally. The session-management and streaming-json design docs can continue to say external timestamps are RFC 3339 strings unless a nearby sentence needs clarification.

Finally, update `.agents/.tasks/062.md`, `.agents/.tasks/index.md`, and this ExecPlan with final status and evidence. Then run the deslop skill, apply any in-scope cleanup it finds, run `just ci`, and commit with a Conventional Commit message.

## Concrete Steps

Work from the repository root:

    cd /Users/travisennis/Projects/cake

Search the relevant timestamp surface:

    rg -n "timestamp: Option<String>|to_rfc3339|timestamp: Some\\(\" src/clients src/config tests

Run targeted tests while editing:

    cargo test clients::types
    cargo test config::session
    cargo test clients::agent_state
    cargo test clients::responses::tests::
    cargo test clients::chat_completions::tests::

Run the required final validation after code, docs, and task metadata are updated:

    just ci

## Validation and Acceptance

Acceptance is met when no `ConversationItem`, conversation-bearing `SessionRecord`, or conversation-bearing `StreamRecord` variant stores a timestamp as `Option<String>`. Timestamp generation for conversation items must use `chrono::Utc::now()` values instead of immediately formatting to strings.

Existing session JSON with timestamp strings must still load, and new session/stream JSON must still serialize timestamps as RFC 3339 strings. The targeted tests should pass before final validation, and `just ci` must pass because this task changes code and docs.

## Idempotence and Recovery

These edits are source, docs, task metadata, and snapshot changes only. Re-running tests is safe. If snapshot output changes unexpectedly, inspect the `.snap.new` files before accepting them. Do not change `CURRENT_FORMAT_VERSION` unless a test proves the external session schema changed; this plan expects no format-version bump.

## Artifacts and Notes

Relevant current code:

    src/clients/types.rs: ConversationItem, SessionRecord, StreamRecord
    src/clients/agent_state.rs: creates user, developer, and tool-output conversation timestamps
    src/clients/chat_completions.rs: parses assistant response items
    src/clients/responses.rs: parses Responses API output items
    src/config/session.rs: loads and saves JSONL sessions

## Interfaces and Dependencies

Use the existing dependency:

    chrono = { version = "0.4.44", features = ["serde"] }

The intended internal field shape is:

    timestamp: Option<DateTime<Utc>>

The intended external JSON shape remains:

    "timestamp": "2026-05-10T12:34:56Z"

Revision note, 2026-05-10 / Codex: Created this ExecPlan after initial source inspection because task `062` is `Effort: L`.

Revision note, 2026-05-10 / Codex: Moved this ExecPlan to completed after implementation and deslop.

Revision note, 2026-05-10 / Codex: Recorded final `just ci` success before commit.
