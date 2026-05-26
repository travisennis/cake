## Reorganize Type Modules

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

Reference: `.agents/PLANS.md` in the repository root.

## Purpose / Big Picture

Core types are spread across four locations (`src/models/`, `src/clients/types.rs`, `src/config/model.rs`, `src/clients/chat_types.rs`) without a clear boundary. The largest file, `src/clients/types.rs`, is 1,842 lines mixing domain types, API wire-format DTOs, session persistence records, and usage statistics. After this change, every type lives in a module that reflects its layer and purpose. Contributors can find any type by asking "is it a domain concept, a persistence record, or an API DTO?" and navigating directly to the right file. The `src/models/` directory is eliminated entirely, and `Message` is replaced by `(Role, String)` tuples on input and `ConversationItem` on output.

## Progress

- [x] Milestone 1: Create `src/types/` module skeleton
- [x] Milestone 2: Move `Usage`, `InputTokensDetails`, `OutputTokensDetails` to `src/types/usage.rs`
- [x] Milestone 3: Move `Role`, `ReasoningContent`, `ReasoningContentKind`, `ConversationItem` to `src/types/conversation.rs`
- [x] Milestone 4: Move `GitState`, `TaskOutcome`, `TaskCompleteSubtype`, `SessionRecord`, `StreamRecord`, all `*Data` structs to `src/types/session.rs`
- [x] Milestone 5: Extract Responses API DTOs into `src/clients/responses_types.rs` and move conversions to `src/clients/responses.rs`
- [x] Milestone 6: Delete `src/clients/types.rs` and update all imports to `crate::types`
- [x] Milestone 7: Delete `src/models/` and update all imports to `crate::types`
- [x] Milestone 8: Remove `Message` struct, replace with `String` input and `Option<String>` output on `Agent::send()`
- [x] Final validation: `just ci` passes, no `src/models/` directory, no `src/clients/types.rs` file

## Surprises & Discoveries

- `MessageData`, `FunctionCallData`, `FunctionCallOutputData`, `ReasoningData` are only constructed by name inside tests; in non-test code only `TaskStartData`, `TaskCompleteData`, and `HookEventData` are constructed by name. Re-exporting all `*Data` types at the top of `crate::types` triggered "unused import" warnings on non-test builds. Resolved by declaring `pub mod session` so the test-only structs are reachable at `crate::types::session::*` without polluting the top-level namespace.
- Two `#[cfg(test)]` modules already exist in `clients/responses.rs` (`tests` and `response_parsing_tests`). The migrated `to_api_input_*` tests were appended into the second module, which required a duplicate `to_api_input_json` helper.
- Snapshot files retained their original byte content so wire-format compatibility was preserved automatically; only the file paths needed to change to match the new module locations.

## Decision Log

- Decision: Organize by layer first, then by backend within the API layer.
  Rationale: Domain types are backend-agnostic and should live at the crate root. API DTOs are backend-specific and belong in their respective client modules. This matches the "deletion test" — removing Responses API support means deleting `responses.rs` and `responses_types.rs` together.
  Date/Author: 2026-05-19 / Trav + agent

- Decision: Merge `src/models/` into a new `src/types/` module.
  Rationale: The name `models` is ambiguous. `types` clearly communicates "shared domain vocabulary." Having one canonical home avoids "which module does this belong to?" confusion.
  Date/Author: 2026-05-19 / Trav + agent

- Decision: Keep session/persistence types (`SessionRecord`, `StreamRecord`, `*Data` structs, `GitState`) in `src/types/session.rs` alongside domain types, not in `config/session.rs`.
  Rationale: They are domain vocabulary the agent loop and observers work with directly. `GitState` is an outlier (only constructed in config) but splitting it out creates worse indirection than keeping it with `SessionRecord`.
  Date/Author: 2026-05-19 / Trav + agent

- Decision: Extract Responses API DTOs into `src/clients/responses_types.rs`, symmetric with `src/clients/chat_types.rs`.
  Rationale: Matches the existing pattern for Chat Completions DTOs. The deletion test confirms: removing Responses API means deleting `responses.rs` + `responses_types.rs` together.
  Date/Author: 2026-05-19 / Trav + agent

- Decision: Move `ConversationItem::to_api_input_item()` and the `From<&ConversationItem> for ResponsesApiInputItem` impl into `src/clients/responses.rs`.
  Rationale: These are Responses-specific conversion logic. If you remove Responses API support, you'd expect to find and delete them alongside the Responses code. The domain type `ConversationItem` stays in `types/conversation.rs` without any API knowledge.
  Date/Author: 2026-05-19 / Trav + agent

- Decision: Move `Usage`, `InputTokensDetails`, `OutputTokensDetails` to `src/types/usage.rs`.
  Rationale: `Usage` is a domain type that both backends normalize into. It is not an API DTO. The API-specific `ApiUsage` and `ChatUsage` stay in their respective `*_types.rs` files with their `From` impls.
  Date/Author: 2026-05-19 / Trav + agent

- Decision: Leave `ReasoningEffort`, `ApiType`, `ModelConfig`, `ResolvedModelConfig`, `ModelProvider`, `ProviderHeaders` in `config/model.rs`.
  Rationale: They are model configuration concerns, not domain types. Both backends already import them from config. Moving them to `types/` would blur the layer boundary.
  Date/Author: 2026-05-19 / Trav + agent

- Decision: Delete `src/clients/types.rs` entirely rather than keeping it as a re-export module.
  Rationale: This is a full migration, not a gradual aliasing step. Every import gets updated in one pass. A re-export module just delays cleanup and creates confusion about which path is canonical.
  Date/Author: 2026-05-19 / Trav + agent

- Decision: Use selective re-exports in `src/types/mod.rs` (Option A).
  Rationale: Makes `crate::types::ConversationItem` work directly, matching the current `crate::clients::types::ConversationItem` pattern. Makes the module's public surface explicit and auditable. Glob re-exports hide new types; qualified paths add noise.
  Date/Author: 2026-05-19 / Trav + agent

- Decision: Remove `Message` struct entirely. Input path uses `(Role, String)` tuples (which `build_initial_prompt_messages` already returns). Output path uses `ConversationItem` or extracted content.
  Rationale: `Message` is always immediately converted to `ConversationItem` on input and destructured to read `.content` on output. It adds ceremony without information. If a thin response type is needed later, it can be introduced then based on actual need.
  Date/Author: 2026-05-19 / Trav + agent

- Decision: Use `pub` visibility for module interfaces, `pub(super)` for internal DTOs. No `pub(crate)` additions.
  Rationale: This is a binary-only crate — `pub` and `pub(crate)` are functionally identical. The existing codebase uses `pub` for interfaces and `pub(super)` for internal types. The real API surface control is what `mod.rs` re-exports, not the visibility modifier. Stay consistent with existing convention.
  Date/Author: 2026-05-19 / Trav + agent

- Decision: Tests travel with their types. Each sub-module gets its own `#[cfg(test)] mod tests`.
  Rationale: Standard Rust pattern. Makes it trivial to find which tests cover a type. Responses API input snapshot tests move to `responses.rs` or `responses_types.rs` since they test Responses-specific conversion logic.
  Date/Author: 2026-05-19 / Trav + agent

- Decision: Cross-module conversion impls live on the destination type's file. `StreamRecord::from_conversation_item()` lives in `session.rs`. `From<StreamRecord> for SessionRecord` lives in `session.rs`. `SessionRecord::to_conversation_item()` lives in `session.rs`.
  Rationale: Rust idiom — impls go on the type they're defined for. `session.rs` imports `ConversationItem` from its sibling module. No circular dependency because `conversation.rs` doesn't import from `session.rs`.
  Date/Author: 2026-05-19 / Trav + agent

## Outcomes & Retrospective

- All eight milestones completed in a single change. `src/models/` and `src/clients/types.rs` are gone. The 1,842-line monolith is split into `src/types/conversation.rs` (~280 lines), `src/types/usage.rs` (~60 lines), `src/types/session.rs` (~700 lines including tests), and `src/clients/responses_types.rs` (~190 lines).
- `Agent::send()` signature is now `String -> anyhow::Result<Option<String>>`, removing the redundant `Message` wrapper. Internal history still uses `ConversationItem::Message { role, content, ... }`.
- `just ci` (fmt, clippy --all-targets, full tests, import lint) is green; 624 unit tests + 24 integration tests pass.
- Acceptance criteria satisfied:
  1. `src/models/` removed ✓
  2. `src/clients/types.rs` removed ✓
  3. `src/types/` contains `conversation.rs`, `session.rs`, `usage.rs` with selective re-exports in `mod.rs` ✓
  4. `src/clients/responses_types.rs` contains all Responses API DTOs; `src/clients/chat_types.rs` unchanged ✓
  5. `Message` struct removed; `Agent::send()` updated ✓
  6. `just ci` passes ✓
  7. No `pub(crate)` visibility additions; `pub` for module interfaces, `pub(super)` for internal DTOs ✓
  8. `clients/mod.rs` re-exports `ConversationItem`, `GitState`, `SessionRecord`, `TaskOutcome` from `crate::types` ✓

## Context and Orientation

The cake CLI is a binary-only Rust crate (Rust 2024 edition, Tokio async runtime). It has no library target. The type modules being reorganized are:

- `src/models/` — 3 files, 232 lines. Contains `Role` (enum) and `Message` (struct with `role` and `content` fields). `Message` is a thin wrapper used only for `Agent::send()` input/output.
- `src/clients/types.rs` — 1,842 lines. The monolith containing domain types (`ConversationItem`, `Usage`, `TaskOutcome`, `SessionRecord`, `StreamRecord`, all `*Data` structs, `GitState`, `ReasoningContent`), Responses API DTOs (`Request`, `ApiResponse`, `OutputMessage`, `OutputContent`, `ProviderConfig`, `ReasoningConfig`, `ApiUsage`, etc.), and conversion impls.
- `src/clients/chat_types.rs` — 125 lines. Chat Completions request/response DTOs, all `pub(super)`. Properly scoped.
- `src/config/model.rs` — 303 lines. `ApiType`, `ReasoningEffort`, `ModelConfig`, `ResolvedModelConfig`, `ModelProvider`, `ProviderHeaders`. Stays in place.

Key import paths today:
- `crate::clients::types::{ConversationItem, Usage, SessionRecord, StreamRecord, ...}` — used across ~15 files
- `crate::models::{Message, Role}` — used across ~9 files
- `crate::clients::{Agent, ConversationItem, TaskOutcome, ToolContext}` — re-exported through `clients/mod.rs`
- `crate::config::model::{ApiType, ModelConfig, ReasoningEffort, ResolvedModelConfig}` — used across ~10 files

The `clients/mod.rs` re-exports `ConversationItem`, `GitState`, `SessionRecord`, and `TaskOutcome` from `types`. After this plan, those re-exports will point to `crate::types` instead.

`Message` is used in two ways:
1. Input: `main.rs` constructs `Message { role: Role::User, content: ... }` and passes it to `Agent::send()`. Internally, `Agent::send()` immediately extracts `.content` and creates a `ConversationItem::Message`.
2. Output: `Agent::send()` returns `Option<Message>`. The caller in `main.rs` reads `.content` from it. `resolve_assistant_message()` in `agent_state.rs` constructs a `Message` from the last `ConversationItem::Message` in history.

After removing `Message`:
- Input: `Agent::send()` takes `(Role, String)` or just a `String` (since it's always `Role::User`).
- Output: `Agent::send()` returns `Option<String>` (the content) or `Option<ConversationItem>` if the caller needs more detail. `main.rs` only reads `.content`, so `Option<String>` suffices.

## Plan of Work

### Milestone 1: Create `src/types/` module skeleton

Create `src/types/mod.rs`, `src/types/conversation.rs`, `src/types/session.rs`, and `src/types/usage.rs` as empty files with module declarations. Add `mod types;` to `src/main.rs`. Verify the project compiles with empty type modules.

### Milestone 2: Move `Usage` types to `src/types/usage.rs`

Move `Usage`, `InputTokensDetails`, and `OutputTokensDetails` from `src/clients/types.rs` to `src/types/usage.rs`. Add re-exports in `src/types/mod.rs`. Update all imports from `crate::clients::types::{Usage, InputTokensDetails, OutputTokensDetails}` to `crate::types::{Usage, InputTokensDetails, OutputTokensDetails}`. Update `clients/mod.rs` re-exports. Move the `Usage` tests from `clients/types.rs` to `types/usage.rs`. Run `cargo check --tests` and `cargo test usage`.

### Milestone 3: Move conversation types to `src/types/conversation.rs`

Move `Role` from `src/models/roles.rs`, `ReasoningContent` and `ReasoningContentKind` from `src/clients/types.rs`, and `ConversationItem` from `src/clients/types.rs` to `src/types/conversation.rs`. Add re-exports in `src/types/mod.rs`. Update all imports from `crate::models::Role` and `crate::clients::types::ConversationItem` to `crate::types::{Role, ConversationItem, ...}`. Move the `Role` tests from `models/roles.rs` and the `ConversationItem`/`ReasoningContent` tests from `clients/types.rs` to `conversation.rs`. Run `cargo check --tests` and `cargo test conversation`.

Note: `ConversationItem::to_api_input_item()` and the `From<&ConversationItem> for ResponsesApiInputItem` impl stay in `clients/types.rs` for now. They move to `clients/responses.rs` in Milestone 5.

### Milestone 4: Move session/persistence types to `src/types/session.rs`

Move `GitState`, `TaskCompleteSubtype`, `TaskOutcome` (and its `Serialize`/`Deserialize` impls), `TaskStartData`, `MessageData`, `FunctionCallData`, `FunctionCallOutputData`, `ReasoningData`, `TaskCompleteData`, `HookEventData`, `SessionRecord`, `StreamRecord`, and the `From<StreamRecord> for SessionRecord`, `StreamRecord::from_conversation_item()`, `SessionRecord::normalize_legacy_fields()`, and `SessionRecord::to_conversation_item()` impls from `src/clients/types.rs` to `src/types/session.rs`. Add re-exports in `src/types/mod.rs`. Update all imports. Move the corresponding tests. Run `cargo check --tests` and `cargo test session`.

### Milestone 5: Extract Responses API DTOs into `src/clients/responses_types.rs`

Create `src/clients/responses_types.rs` containing all `pub(super)` Responses API types currently in `src/clients/types.rs`:
- `ResponsesApiInputItem`, `ResponsesMessageContent`, `ResponsesReasoningSummary`
- `ProviderConfig`, `ReasoningConfig`
- `Request`
- `ApiResponse`, `OutputMessage`, `OutputContent`
- `ApiUsage`, `ApiInputTokensDetails`, `ApiOutputTokensDetails`

Add `mod responses_types;` to `src/clients/mod.rs`. Update `src/clients/responses.rs` to import these from `super::responses_types` instead of `super::types`. Move the `From<&ConversationItem> for ResponsesApiInputItem` impl and `ConversationItem::to_api_input_item()` from `types.rs` to `responses.rs`. Move the `ProviderConfig` test and the Responses API input snapshot tests from `clients/types.rs` to `responses.rs` (or `responses_types.rs` for the DTO-only tests). Run `cargo check --tests` and `cargo test responses`.

### Milestone 6: Delete `src/clients/types.rs`

After Milestones 2–5, `src/clients/types.rs` should be empty (or contain only the `use crate::models::Role` import and the `mod tests` that was already moved). Delete the file. Remove `mod types;` and `pub use types::{...}` from `src/clients/mod.rs`. Update `clients/mod.rs` to re-export `ConversationItem`, `GitState`, `SessionRecord`, and `TaskOutcome` from `crate::types` instead. Update all remaining `crate::clients::types::` imports throughout the crate to `crate::types::`. Run `cargo check --tests` and `cargo test`.

### Milestone 7: Delete `src/models/`

After Milestone 3, `src/models/roles.rs` and `src/models/messages.rs` should be empty (their types moved to `types/conversation.rs`). Delete `src/models/mod.rs`, `src/models/roles.rs`, and `src/models/messages.rs`. Remove `mod models;` from `src/main.rs`. Update any remaining `crate::models::` imports to `crate::types::`. Run `cargo check --tests` and `cargo test`.

### Milestone 8: Remove `Message` struct

Change `Agent::send()` signature from `fn send(&mut self, message: Message) -> anyhow::Result<Option<Message>>` to `fn send(&mut self, content: String) -> anyhow::Result<Option<String>>`. Update callers:
- In `main.rs`: replace `Message { role: Role::User, content: ... }` with just the content string. Replace `result.map(|m| m.content)` with `result` directly.
- In `agent_state.rs`: replace `resolve_assistant_message()` return type from `Message` to `String`. Remove the `Message` construction.
- In `agent.rs` tests: replace `Message { role: Role::User, content: ... }` with just the content string. Replace `Some(Message { content, .. })` assertions with `Some(content)` assertions.
- Remove `src/types/conversation.rs`'s `Message` struct and its tests (if any were moved there from `models/messages.rs`).
- Update `main.rs`'s `TurnResult` struct to use `Option<String>` instead of `Option<Message>`.
- Update `CliOutputSink::render_text_result` and related methods to work with `Option<String>`.
- Remove `use crate::models::{Message, Role}` from `main.rs` and other files.
- Run `cargo check --tests`, `cargo test`, `cargo fmt`, `just clippy-strict`.

## Concrete Steps

All commands run from the repository root directory.

### Milestone 1

    cargo check --tests

Create the empty module files and add `mod types;` to `main.rs`. Verify compilation.

### Milestone 2

    cargo check --tests
    cargo test usage

Move `Usage`, `InputTokensDetails`, `OutputTokensDetails` and their tests. Update imports. Verify compilation and tests pass.

### Milestone 3

    cargo check --tests
    cargo test conversation
    cargo test role

Move `Role`, `ReasoningContent`, `ReasoningContentKind`, `ConversationItem` and their tests. Update imports. Verify compilation and tests pass.

### Milestone 4

    cargo check --tests
    cargo test session

Move all session/persistence types and their tests. Update imports. Verify compilation and tests pass.

### Milestone 5

    cargo check --tests
    cargo test responses

Extract Responses API DTOs into `responses_types.rs`. Move conversion impls to `responses.rs`. Update imports. Verify compilation and tests pass.

### Milestone 6

    cargo check --tests
    cargo test

Delete `clients/types.rs`. Update all `crate::clients::types::` imports. Verify full compilation and test suite.

### Milestone 7

    cargo check --tests
    cargo test

Delete `src/models/`. Update all `crate::models::` imports. Verify full compilation and test suite.

### Milestone 8

    cargo check --tests
    cargo test
    cargo fmt
    just clippy-strict

Remove `Message` struct. Change `Agent::send()` signature. Update all callers and tests. Verify full compilation, test suite, formatting, and linting.

### Final validation

    just ci

Verify that `just ci` passes with no warnings or errors. Confirm that `src/models/` directory no longer exists and `src/clients/types.rs` no longer exists.

## Validation and Acceptance

The change is complete when all of the following are true:

1. `src/models/` directory does not exist. All former `crate::models::` imports now reference `crate::types::`.
2. `src/clients/types.rs` does not exist. All former `crate::clients::types::` imports now reference `crate::types::` or `crate::clients::responses_types::` (for internal DTOs).
3. `src/types/` contains three sub-modules: `conversation.rs`, `session.rs`, `usage.rs`, with selective re-exports in `mod.rs`.
4. `src/clients/responses_types.rs` contains all Responses API DTOs. `src/clients/chat_types.rs` is unchanged.
5. `Message` struct no longer exists. `Agent::send()` takes a `String` and returns `anyhow::Result<Option<String>>`.
6. `just ci` passes: `cargo check --tests`, `cargo test`, `cargo fmt --check`, `just clippy-strict`, and `ahm --dry-run index` reports no stale indexes.
7. No `pub(crate)` visibility additions — `pub` for module interfaces, `pub(super)` for internal DTOs, matching existing convention.
8. `clients/mod.rs` re-exports `ConversationItem`, `GitState`, `SessionRecord`, and `TaskOutcome` from `crate::types`.

## Idempotence and Recovery

Each milestone is a self-contained commit that compiles and passes tests. If a milestone fails partway:
- Use `git diff` to identify incomplete changes.
- Revert the last commit with `git reset --hard HEAD` and re-attempt.
- Milestones 1–7 are pure reorganization (move types, update imports). No behavior changes. If tests pass after a milestone, it is safe.
- Milestone 8 changes behavior (removes `Message`). If this milestone causes issues, it can be deferred while milestones 1–7 are still valuable on their own.

## Artifacts and Notes

### Target module structure after completion

    src/
      types/
        mod.rs              — Selective re-exports from sub-modules
        conversation.rs     — Role, ConversationItem, ReasoningContent, ReasoningContentKind
        session.rs          — SessionRecord, StreamRecord, GitState, TaskOutcome, TaskCompleteSubtype, *Data structs
        usage.rs            — Usage, InputTokensDetails, OutputTokensDetails
      clients/
        mod.rs              — Re-exports from crate::types, Agent, ToolContext, summarize_tool_args
        agent.rs            — Agent orchestrator
        agent_runner.rs     — Agent run loop
        agent_observer.rs   — Observer callbacks
        agent_state.rs      — Agent state, resolve_assistant_message
        backend.rs          — Backend abstraction
        chat_completions.rs — Chat Completions backend
        chat_types.rs       — Chat Completions DTOs (pub(super), unchanged)
        responses.rs        — Responses backend + ConversationItem→ResponsesApiInputItem conversion
        responses_types.rs  — Responses API DTOs (pub(super))
        provider_strategy.rs — Provider routing
        retry.rs            — Retry logic
        tools/              — Tool implementations
      config/
        model.rs            — ModelConfig, ResolvedModelConfig, ApiType, ReasoningEffort, ModelProvider, ProviderHeaders (unchanged)
        session.rs          — Session persistence (imports from crate::types)
        ...

### Import path changes

| Before | After |
|--------|-------|
| `crate::models::Role` | `crate::types::Role` |
| `crate::models::Message` | Removed; use `(Role, String)` or `String` |
| `crate::clients::types::ConversationItem` | `crate::types::ConversationItem` |
| `crate::clients::types::Usage` | `crate::types::Usage` |
| `crate::clients::types::SessionRecord` | `crate::types::SessionRecord` |
| `crate::clients::types::StreamRecord` | `crate::types::StreamRecord` |
| `crate::clients::types::TaskOutcome` | `crate::types::TaskOutcome` |
| `crate::clients::types::GitState` | `crate::types::GitState` |
| `crate::clients::types::ReasoningContent` | `crate::types::ReasoningContent` |
| `crate::clients::types::ReasoningContentKind` | `crate::types::ReasoningContentKind` |
| `crate::clients::types::HookEventData` | `crate::types::HookEventData` |
| `crate::clients::types::FunctionCallOutputData` | `crate::types::FunctionCallOutputData` |
| `crate::clients::types::InputTokensDetails` | `crate::types::InputTokensDetails` |
| `crate::clients::types::OutputTokensDetails` | `crate::types::OutputTokensDetails` |
| `crate::clients::types::ProviderConfig` | `crate::clients::responses_types::ProviderConfig` (internal only) |
| `crate::clients::types::OutputMessage` | `crate::clients::responses_types::OutputMessage` (internal only) |
| `crate::clients::types::OutputContent` | `crate::clients::responses_types::OutputContent` (internal only) |
| `crate::clients` re-exports (`ConversationItem`, `GitState`, `SessionRecord`, `TaskOutcome`) | Re-exported from `crate::types` in `clients/mod.rs` |
| `crate::config::model::*` | Unchanged |

## Interfaces and Dependencies

### New `src/types/mod.rs` interface

    pub use conversation::{ConversationItem, ReasoningContent, ReasoningContentKind, Role};
    pub use session::{
        FunctionCallData, FunctionCallOutputData, GitState, HookEventData, MessageData,
        ReasoningData, SessionRecord, StreamRecord, TaskCompleteData, TaskCompleteSubtype,
        TaskOutcome, TaskStartData,
    };
    pub use usage::{InputTokensDetails, OutputTokensDetails, Usage};

### New `src/clients/responses_types.rs` interface

All types remain `pub(super)` within `clients`. No public interface changes.

### Changed `src/clients/mod.rs` interface

    pub use agent::Agent;
    pub use tools::{ToolContext, summarize_tool_args};
    pub use crate::types::{ConversationItem, GitState, SessionRecord, TaskOutcome};

### Changed `Agent::send()` signature

Before:

    pub async fn send(&mut self, message: Message) -> anyhow::Result<Option<Message>>

After:

    pub async fn send(&mut self, content: String) -> anyhow::Result<Option<String>>

### Removed types

- `Message` struct (formerly in `src/models/messages.rs`) — replaced by `(Role, String)` on input and `String` on output
- `src/models/roles.rs` — `Role` moves to `types/conversation.rs`
- `src/models/messages.rs` — `Message` is removed
- `src/models/mod.rs` — entire directory removed
- `src/clients/types.rs` — entire file removed
