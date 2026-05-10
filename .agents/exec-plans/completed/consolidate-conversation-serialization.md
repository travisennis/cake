# Consolidate Conversation Serialization Paths

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This plan follows `.agents/PLANS.md` from the repository root. The plan is intentionally self-contained so a contributor can resume the work from this file alone.

## Purpose / Big Picture

The `cake` CLI keeps a typed conversation history in memory, persists selected records to append-only session JSONL files, emits live `stream-json` records, and sends conversation history back to model APIs. Before this change, the same `ConversationItem` shape was serialized directly for sessions, converted to Responses API input with hand-built `serde_json::json!` values, and converted to a separate streaming JSON shape with another hand-built method that production did not use. Those paths can drift silently.

After this change, API input, stream output, and session output are produced through typed serializable representations. A user can see the work by running the targeted serialization tests and observing snapshots for each supported external representation: Responses API input, stream JSON, and session JSON.

## Progress

- [x] (2026-05-10 00:00Z) Read task `061`, `.agents/TASKS.md`, and `.agents/PLANS.md`.
- [x] (2026-05-10 00:00Z) Inspected `src/clients/types.rs`, `src/clients/responses.rs`, `src/clients/agent.rs`, and `src/clients/agent_observer.rs` to locate current serialization paths.
- [x] (2026-05-10 00:00Z) Replaced hand-built Responses API input JSON with typed serializable DTOs.
- [x] (2026-05-10 00:00Z) Removed the dead hand-built streaming JSON methods and snapshot the production `StreamRecord` conversion instead.
- [x] (2026-05-10 00:00Z) Added session JSON snapshots for conversation records so the persisted schema is pinned separately from stream JSON and API input.
- [x] (2026-05-10 00:00Z) Ran targeted test `cargo test clients::types`; 44 filtered unit tests passed.
- [x] (2026-05-10 00:00Z) Updated task/index metadata when the implementation was complete.
- [x] (2026-05-10 00:00Z) Ran full CI once before deslop; `just ci` passed.
- [x] (2026-05-10 00:00Z) Ran deslop review and applied the in-scope follow-ups: production Responses requests now carry typed DTOs directly, and `docs/design-docs/conversation-types.md` no longer documents the removed streaming helper.
- [x] (2026-05-10 00:00Z) Reran final `just ci` after deslop; all checks passed.

## Surprises & Discoveries

- Observation: Production `--output-format stream-json` already serializes `StreamRecord` through `AgentObserver::stream_record`; `ConversationItem::to_streaming_json()` is dead code used only by tests.
  Evidence: `src/clients/agent_observer.rs` serializes `StreamRecord` directly, while `src/clients/types.rs` marks `ConversationItem::to_streaming_json()` with `#[allow(dead_code)]`.

- Observation: `SessionRecord::to_streaming_json()` was also dead and overlapped with task 027's stale-code list.
  Evidence: `rg to_streaming_json src/clients/types.rs` returns no matches after this plan's implementation, and task 027 now records those two items as resolved by task 061.

## Decision Log

- Decision: Keep `ConversationItem` as the in-memory conversation type and add typed adapters for external wire shapes instead of splitting the whole type hierarchy in one large refactor.
  Rationale: The task goal is to prevent serialization drift. Typed adapters give compile-time structure and snapshot coverage without rewriting the agent loop, provider parsing, or session replay in a high-risk pass.
  Date/Author: 2026-05-10 / Codex.

- Decision: Treat `StreamRecord` and `SessionRecord` as the canonical stream/session DTOs and snapshot their serde output directly.
  Rationale: Production streaming and persistence already use these enums. Testing a separate `ConversationItem::to_streaming_json()` method gives false confidence because it is not on the production path.
  Date/Author: 2026-05-10 / Codex.

## Outcomes & Retrospective

The implementation now routes Responses API input through typed serializable DTOs, removes the dead streaming JSON helper methods, and snapshots the production `StreamRecord` and `SessionRecord` serialization paths. Deslop tightened the request path so `src/clients/responses.rs` builds `Vec<ResponsesApiInputItem<'_>>` instead of `Vec<serde_json::Value>`. Targeted validation with `cargo test clients::types` and `cargo test clients::responses::tests::` passed; a broad sandboxed `cargo test clients::` run compiled but could not bind wiremock ports in this environment. Final validation with `just ci` passed after deslop.

## Context and Orientation

The key file is `src/clients/types.rs`. It defines `ConversationItem`, the in-memory enum used by the agent loop and provider backends. It also defines `SessionRecord`, the enum written as JSONL session records, and `StreamRecord`, the enum emitted for live stream JSON output. A DTO, or data transfer object, is a small serializable type whose shape is chosen for one external boundary such as an HTTP request or JSONL output.

Responses API requests are assembled in `src/clients/responses.rs`. That module calls `build_input`, which currently maps history items with `ConversationItem::to_api_input()`. Chat Completions requests are assembled in `src/clients/chat_completions.rs` and use a separate `ChatMessage` DTO, so this plan does not change Chat Completions request construction.

Live streaming is coordinated by `src/clients/agent_observer.rs`. Its `stream_record` method serializes a `StreamRecord` and converts the same record into a `SessionRecord` for persistence. `Agent::stream_item` in `src/clients/agent.rs` converts a `ConversationItem` to a `StreamRecord` using `StreamRecord::from_conversation_item`.

## Plan of Work

First, add typed serializable Responses API input DTOs in `src/clients/types.rs`. These DTOs should borrow from `ConversationItem` and express the API wire shape with serde attributes. `ConversationItem::to_api_input()` can remain as a test and compatibility helper, but it should serialize the DTO instead of constructing JSON by hand. `src/clients/responses.rs::build_input` can continue returning `Vec<serde_json::Value>` for now, because the request type already uses that shape and the task can be completed without changing the HTTP request type.

Second, remove `ConversationItem::to_streaming_json()`. Update tests that snapshot stream JSON to call `StreamRecord::from_conversation_item(&item)` and serialize that record. This ties stream snapshots to the same path production uses.

Third, add session JSON snapshots for conversation-bearing `SessionRecord` variants. These snapshots should prove that session persistence is a separate external representation and that it stays aligned with conversion from `ConversationItem`.

Finally, update `.agents/.tasks/061.md` and `.agents/.tasks/index.md` to mark the task complete, run validation, run the deslop review pass, and commit the work with a Conventional Commit message.

## Concrete Steps

Work from the repository root:

    cd /Users/travisennis/Projects/cake

Run the targeted tests while editing:

    cargo test clients::types

Run full project validation after code and task metadata are updated:

    just ci

If snapshots change, review the generated `.snap.new` files and accept only intentional changes by moving them over the existing snapshot files.

## Validation and Acceptance

Run `cargo test clients::types` and expect the serialization tests in `src/clients/types.rs` to pass. The tests should include snapshots for Responses API input, stream JSON through `StreamRecord`, and session JSON through `SessionRecord`.

Run `just ci` and expect formatting, linting, and tests to pass. This proves the codebase remains valid after the serialization consolidation.

Acceptance is met when `ConversationItem` no longer contains a dead hand-built streaming JSON method, Responses API input JSON is produced by typed serializable DTOs, and snapshots cover all three supported external representations.

## Idempotence and Recovery

The edits are source and Markdown changes only. Re-running tests is safe. If snapshot output differs unexpectedly, inspect the `.snap.new` files before replacing existing snapshots. Do not delete or rewrite unrelated task files, session files, or user changes.

## Artifacts and Notes

The important existing evidence is:

    src/clients/agent_observer.rs: stream_record serializes StreamRecord directly.
    src/clients/types.rs: ConversationItem::to_streaming_json is marked dead code.
    src/clients/responses.rs: build_input maps ConversationItem::to_api_input.

## Interfaces and Dependencies

The implementation should stay within existing dependencies: `serde` and `serde_json` are already used for serialization. No new crate is required.

The desired helper interface in `src/clients/types.rs` is:

    impl ConversationItem {
        pub fn to_api_input(&self) -> serde_json::Value
    }

The method may remain public because tests and Responses API code already use it, but its body must delegate to typed serializable DTOs instead of using `serde_json::json!` for each variant. Stream conversion should use the existing:

    impl StreamRecord {
        pub fn from_conversation_item(item: &ConversationItem) -> Self
    }

Revision note, 2026-05-10 / Codex: Created this ExecPlan because task `061` is marked `XL` and requires a plan before implementation.

Revision note, 2026-05-10 / Codex: Updated the plan after implementation to record the typed DTO decision, removed dead streaming helpers, and captured targeted test evidence.

Revision note, 2026-05-10 / Codex: Updated after deslop to record the direct typed Responses request path and design-doc cleanup.

Revision note, 2026-05-10 / Codex: Recorded final `just ci` success after deslop.
