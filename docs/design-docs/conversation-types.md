# Conversation Types

The `types` module defines the core data structures for representing conversations with AI models. `ConversationItem` (in `types::conversation`) is the canonical representation used by both the Responses API and Chat Completions API backends. Each backend handles translation to and from its own wire format, making `ConversationItem` the single source of truth for conversation state.

The enum's variants and fields are defined in `types::conversation`; this document describes what each variant means and the translation contracts around them.

## Overview

All conversation state flows through a single type: `ConversationItem`. This enum represents every possible item in a conversation:

- **`Message`**: a text message from any role (`System`, `Developer`, `Assistant`, `User`, `Tool`). Assistant messages carry the provider message `id` and a `status` such as `completed` or `incomplete`.
- **`FunctionCall`**: a model request to execute a tool --- the provider item `id`, a `call_id` correlating the eventual output, the tool `name`, and the raw JSON `arguments` string exactly as the model produced it.
- **`FunctionCallOutput`**: the result of a tool execution, joined to its call by `call_id`.
- **`Reasoning`**: reasoning-model output --- human-readable `summary` strings, optional opaque `encrypted_content` that must be echoed back to the provider on later turns, and an optional provider `content` array preserved for Chat Completions round-tripping.

Every variant carries an optional UTC timestamp. When session files or stream-json records are serialized, serde writes timestamps as UTC RFC 3339 strings. Loaded sessions normalize legacy missing conversation timestamps from the required `session_meta.timestamp`, so resumed conversation history has a timestamp even when an older JSONL record omitted the field.

## Backend Translation

Each backend translates `Vec<ConversationItem>` into its own wire format independently:

- **Responses API** (`ResponsesApiInputItem::from` in `clients::responses_types`): messages become content arrays (`input_text` for user/system, `output_text` for assistant); reasoning summaries are wrapped in `summary_text` objects; assistant messages include `id` and `status`.
- **Chat Completions** (`build_messages` in `clients::chat_completions`): consecutive `FunctionCall` items are grouped into a single assistant message with multiple `tool_calls`; `Developer`-role messages are emitted with the `developer` role, which `ProviderStrategy::transform_chat_messages` demotes to `user` for providers that don't support it; `Reasoning` text is preserved as provider-specific `reasoning_content` on the next assistant message. Encrypted reasoning traces have no representation in this format.

`StreamRecord::from_conversation_item` produces the `--output-format stream-json` records: plain-text message content and plain-string reasoning summaries, more compact than the API wire formats.

## Usage Tracking

`types::usage` defines the backend-agnostic token usage shape (`Usage`, with cached-input and reasoning-output detail) that both backends normalize their provider responses into. Aggregate usage per task is reported in `task_complete` records.

## Persisted and Streamed Records

`SessionRecord` is the persisted JSONL schema for files in `~/.local/share/cake/sessions/`. It wraps conversation items with `session_meta`, `task_start`, hook, prompt-context, skill-activation, and `task_complete` records.

`StreamRecord` is the `--output-format stream-json` schema for the current task. It has the same task, hook, and conversation records as `SessionRecord`, but intentionally excludes `session_meta` and session-only audit records such as `prompt_context` and `skill_activated`.

The field-level record contracts are documented in [session-management.md](./session-management.md); [streaming-json-output.md](./streaming-json-output.md) documents the stream-specific differences.

## Internal Types

Request/response DTOs live next to the backend that owns them: `clients::responses_types` for the Responses API and `clients::chat_types` for Chat Completions. They are internal implementation details of the `clients` module, not part of the canonical conversation representation.

## Design Decisions

### Single Enum vs. Multiple Types

Using a single `ConversationItem` enum rather than separate types for each item simplifies:

- **Collections**: `Vec<ConversationItem>` for history
- **Serialization**: One `#[serde(tag = "type")]` implementation
- **Pattern matching**: Exhaustive matching on all item types
- **Streaming**: Unified handling for all item types

### Content Arrays vs. Plain Text

The API uses content arrays for flexibility, but this adds complexity. The design:

- Stores plain text internally for simplicity
- Transforms to content arrays only when sending to API
- Keeps original content arrays for reasoning round-tripping

### Dual-Backend Translation

Storing plain text internally decouples conversation state from any specific wire format. Each backend translates `ConversationItem` independently, so adding or changing a backend does not affect the canonical conversation representation or the other backend.

### Encrypted Content Preservation

Reasoning models return encrypted content that must be echoed back. The design:

- Stores encrypted content verbatim
- Skips serialization when `None` to reduce payload size
- Preserves content arrays for Chat Completions provider compatibility
