# Refactor Chat Message Construction State

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This plan follows `.agents/PLANS.md` from the repository root. The plan is self-contained so a contributor can resume the work from this file alone.

## Purpose / Big Picture

Cake converts its internal conversation history into Chat Completions API messages before calling `/chat/completions`. The current conversion works, but the function carries several pending buffers directly in one loop, so it is easy to miss when tool calls, reasoning text, and developer context are flushed or preserved.

After this change, `src/clients/chat_completions.rs::build_messages` will delegate those transitions to a small named builder. Users should observe no request-shape changes: existing Chat Completions snapshots should remain stable, and new tests should make edge transitions explicit.

## Progress

- [x] (2026-05-10 23:57Z) Read `.agents/TASKS.md`, `.agents/.tasks/index.md`, task `064`, `.agents/PLANS.md`, and the relevant `src/clients/chat_completions.rs` code and tests.
- [x] (2026-05-10 23:57Z) Created this ExecPlan because task `064` is `Effort: L` and requires a plan before implementation.
- [x] (2026-05-11 00:05Z) Refactored `build_messages` state handling into a private `ChatMessageBuilder` with named transition methods.
- [x] (2026-05-11 00:05Z) Added edge-case tests for flushing pending tool calls before a user message and preserving developer context until the next user message.
- [x] (2026-05-11 00:05Z) Ran targeted Chat Completions build-message and snapshot tests; all passed and existing snapshots stayed stable.
- [x] (2026-05-11 00:05Z) Ran deslop and applied the in-scope follow-ups: removed an `unreachable!` role-helper path and corrected stale Chat Completions notes in `docs/design-docs/conversation-types.md`.
- [x] (2026-05-11 00:08Z) Ran final `just ci`; all checks passed.
- [x] (2026-05-11 00:08Z) Prepared the completed task for commit.

## Surprises & Discoveries

- Observation: Chat message snapshots already cover the primary request shapes for simple messages, grouped function calls, reasoning with assistant text, reasoning with tool calls, assistant text combined with tool calls, empty history, and Kimi reasoning placeholder injection.
  Evidence: `src/clients/chat_completions.rs` contains `insta::assert_json_snapshot!` tests for these cases.

- Observation: `docs/design-docs/conversation-types.md` had stale Chat Completions notes saying system messages map to developer and reasoning is skipped.
  Evidence: Current `src/clients/chat_completions.rs` maps `Role::System` to `"system"`, folds `Role::Developer` into the next user message, and preserves reasoning text as `reasoning_content` on assistant messages.

- Observation: Strict clippy requires `Role` to be passed by value in the new builder helper methods and permits the builder constructor and role mapper to be `const fn`.
  Evidence: The first `just ci` run failed on `clippy::trivially-copy-pass-by-ref` and `clippy::missing-const-for-fn`; after applying those fixes, the final `just ci` passed.

## Decision Log

- Decision: Keep the refactor local to `src/clients/chat_completions.rs` and preserve the existing `build_messages(history: &[ConversationItem]) -> Vec<ChatMessage<'_>>` interface.
  Rationale: This task is about auditability of Chat Completions message construction, not changing provider behavior or moving ownership boundaries across modules.
  Date/Author: 2026-05-10 / Codex.

- Decision: Use a small builder struct with named transition methods rather than a broader cross-module abstraction.
  Rationale: The existing state is local to one conversion function. A builder makes flush points and pending state ownership explicit without introducing unnecessary public API.
  Date/Author: 2026-05-10 / Codex.

- Decision: Have `chat_role_name` return `Option<&'static str>` for developer messages instead of relying on `unreachable!`.
  Rationale: Developer messages are intentionally folded into user content. Representing that as `None` keeps the helper total and avoids a runtime panic path if the call order changes later.
  Date/Author: 2026-05-11 / Codex.

## Outcomes & Retrospective

`build_messages` now delegates conversion state to `ChatMessageBuilder`, preserving the existing request shape while making pending tool calls, developer context, and reasoning transitions explicit. Two edge-case tests pin behavior that was previously implicit. Deslop also corrected stale design documentation for Chat Completions translation, and final `just ci` passed.

## Context and Orientation

The key file is `src/clients/chat_completions.rs`. It implements the Chat Completions backend for cake. The public request path calls `send_request`, which builds a vector of `ChatMessage` values from the internal `ConversationItem` history, applies provider-specific message transforms, converts tools, and sends a JSON request to the configured Chat Completions-compatible endpoint.

`ConversationItem` is the internal enum for conversation history. Relevant variants are `Message`, `FunctionCall`, `FunctionCallOutput`, and `Reasoning`. A `Message` carries a role such as system, developer, user, assistant, or tool. A `FunctionCall` is a model-requested tool invocation. A `FunctionCallOutput` is the result returned to the model. `Reasoning` is provider-specific hidden reasoning text that some Chat Completions-compatible providers accept as `reasoning_content`.

`ChatMessage` is the outbound Chat Completions request DTO. It can carry normal message content, tool calls, a tool result id, and provider-specific reasoning content. The conversion must preserve current behavior:

- developer messages are buffered and prepended to the next user message as context;
- consecutive function calls are grouped into one assistant message with `tool_calls`;
- function call output first flushes any pending tool calls, then emits a tool message;
- pending tool calls followed by assistant text are merged into that assistant message;
- reasoning content is attached to the next assistant message or assistant tool-call message.

## Plan of Work

First, replace the inline mutable state in `build_messages` with a private `ChatMessageBuilder<'a>` struct in `src/clients/chat_completions.rs`. It should own `messages`, `pending_tool_calls`, `pending_reasoning_content`, and `pending_developer_context`. `build_messages` should create this builder, pass each `ConversationItem` through it, and call a final method that flushes trailing tool calls and returns messages.

Second, move each transition into a method with a behavior name. For example, `push_message`, `push_function_call`, `push_function_call_output`, `remember_reasoning`, `flush_pending_tool_calls`, `content_with_developer_context`, and `push_chat_message`. The method names are more important than the exact names here: the final code should make it obvious when pending tool calls are flushed, when developer context is consumed, and when reasoning is consumed.

Third, keep the existing snapshots and unit tests. Add focused unit tests for edge transitions that were previously implicit: pending tool calls before a non-assistant message must flush into an assistant tool-call message before the next message, and developer context must wait until a user message before being consumed. These tests should not require snapshot updates unless they intentionally add new snapshots.

Finally, update `.agents/.tasks/064.md`, `.agents/.tasks/index.md`, and this ExecPlan with final status and evidence. Run the deslop skill, apply any in-scope cleanup it finds, run final `just ci`, and commit with a Conventional Commit message.

## Concrete Steps

Work from the repository root:

    cd /Users/travisennis/Projects/cake

Run targeted tests while editing:

    cargo test clients::chat_completions::tests::build_messages
    cargo test clients::chat_completions::tests::snapshot_

Run full project validation after code, docs, and task metadata are updated:

    just ci

## Validation and Acceptance

Acceptance is met when `build_messages` no longer carries the pending buffers directly in its loop, but instead delegates to named transition methods on a small private builder. Existing Chat Completions snapshots must remain stable, and the added edge-case tests must pass.

Run the targeted Chat Completions tests and expect them to pass. Run `just ci` and expect formatting, linting, and all tests to pass.

## Idempotence and Recovery

These edits are source and Markdown changes only. Re-running tests is safe. If snapshot output changes unexpectedly, inspect generated `.snap.new` files before accepting them; this plan expects no existing snapshot changes.

## Artifacts and Notes

Important current locations:

    src/clients/chat_completions.rs::build_messages
    src/clients/chat_completions.rs::ChatMessageBuilder::flush_pending_tool_calls
    src/clients/chat_completions.rs tests beginning around build_messages_simple_conversation
    src/clients/snapshots/cake__clients__chat_completions__tests__*.snap

## Interfaces and Dependencies

No new dependencies are needed. The public signature should stay:

    fn build_messages(history: &[ConversationItem]) -> Vec<ChatMessage<'_>>

The new helper should stay private to `src/clients/chat_completions.rs`, with a shape similar to:

    struct ChatMessageBuilder<'a> {
        messages: Vec<ChatMessage<'a>>,
        pending_tool_calls: Vec<ChatToolCallRef<'a>>,
        pending_reasoning_content: Option<Cow<'a, str>>,
        pending_developer_context: Vec<&'a str>,
    }

Revision note, 2026-05-10 / Codex: Created this ExecPlan after initial source inspection because task `064` is `Effort: L`.

Revision note, 2026-05-11 / Codex: Updated after implementation and deslop to record the builder refactor, edge tests, design-doc correction, and targeted test evidence.

Revision note, 2026-05-11 / Codex: Recorded final `just ci` success after clippy-driven cleanup.
