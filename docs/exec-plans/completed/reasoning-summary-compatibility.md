## Preserve typed Responses reasoning summaries

This completed ExecPlan is maintained per `docs/workflow/exec-plans.md`.
The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes &
Retrospective must be kept current as work proceeds.

## Purpose / Big Picture

Cake currently stores reasoning summaries as an array of plain strings. That
matches one OpenAI-compatible provider's shortcut, but the OpenAI Responses API
returns each summary as an object with a `type` and `text`. A Responses response
with the standard object form therefore fails before the judge can parse its
verdict and may consume the bounded recovery retry.

After this change, Cake's canonical conversation and session representation will
preserve typed reasoning-summary objects. The Responses decoder will continue to
accept legacy arrays of strings by converting each string to a `summary_text`
object. Existing sessions will remain loadable, while new records will write the
canonical object form.

## Progress

- [x] (2026-08-17) Confirmed the current branch is clean and identified the
  affected conversation, session, Responses DTO, and test surfaces.
- [x] (2026-08-17) Chosen canonical `ReasoningSummary` object with compatibility
  deserialization for legacy strings.
- [x] (2026-08-17) Added the shared internal type and updated
  conversation/session records.
- [x] (2026-08-17) Updated Responses decoding, request conversion, and provider
  fallbacks.
- [x] (2026-08-17) Added focused compatibility, round-trip, and no-retry tests.
- [x] (2026-08-17) Ran formatting and focused pure/integration tests; WireMock
  suites were then verified with the required network permission.
- [x] (2026-08-17) Ran `just ci`; all repository verification gates passed,
  including the full test suite, 92.79% coverage, CRAP, complexity, and lint
  checks.
- [x] (2026-08-17) Completed the preflight review and archived this plan.

## Surprises & Discoveries

- The current internal `ConversationItem::Reasoning.summary` and persisted
  `ReasoningData.summary` are both `Option<Vec<String>>`, so fixing only the
  Responses DTO would immediately discard the object `type` during conversion.
- The Responses request DTO already emits summary objects from the string
  representation. The change should make that conversion direct from the
  canonical object instead of reconstructing a fixed `summary_text` type.
- Strict Clippy required the provider name in the new documentation comment to
  use code formatting; this was fixed before the final gate.
- Persisted sessions are append-only and must not be rewritten. The summary
  type must therefore deserialize both old string entries and new object entries.

## Decision Log

- Decision: use a typed internal summary object with a string `type` field and a
  string `text` field. Rationale: it matches the provider wire shape, preserves
  future summary item kinds, and keeps the domain representation independent of
  OpenRouter's string shortcut. Date/Author: 2026-08-17 / Codex.
- Decision: implement legacy string support in the summary type's deserializer,
  not as a provider-name or model-name branch. Rationale: old sessions and any
  OpenRouter-compatible response remain readable without spreading provider
  quirks through the agent loop. Date/Author: 2026-08-17 / Codex.
- Decision: retain bounded retries for actual undecodable bodies. Rationale:
  empty, truncated, proxy, and non-JSON responses remain transient candidates;
  valid typed summaries should be normalized before retry classification. Date/
  Author: 2026-08-17 / Codex.

## Outcomes & Retrospective

The Responses decoder now accepts both the standard object form and the
legacy string form for reasoning summaries. The shared conversation and
session types retain each summary item's `type` and `text`, new records emit
objects, and historical records normalize strings to `summary_text` objects.
Request conversion echoes the preserved type instead of reconstructing every
summary as `summary_text`. A WireMock judge test demonstrates that a valid
typed-summary response reaches its verdict in one provider attempt, while the
existing malformed-body retry tests continue to pass.

The implementation stayed within the existing serde and retry boundaries, so
no provider-specific branch, session version, dependency, or retry category
was needed. The default sandbox could not bind WireMock ports, but the same
focused suites passed with the required network permission; `just ci` then
passed all repository gates. Preflight found no worthwhile follow-up fixes.

## Context and Orientation

`src/types/conversation.rs` owns the backend-neutral `ConversationItem` enum.
Its `Reasoning` variant is used by both provider adapters and is the canonical
conversation state. `src/types/session.rs` converts that state into append-only
stream and session records, so its reasoning data must preserve the same shape
and accept historical records.

`src/clients/responses_types.rs` owns Responses wire DTOs. The response DTO must
accept the standard summary object array and legacy string arrays. `src/clients/
responses.rs` converts decoded output into `ConversationItem` and converts
conversation history back into Responses request items.

`src/clients/chat_completions.rs` synthesizes reasoning records for the Chat
Completions path. It must construct the canonical `summary_text` object when it
needs a synthetic summary. Tests under `src/types/`, `src/clients/`, and `tests/`
protect JSONL, stream-json, request, and response behavior.

## Plan of Work

First add `ReasoningSummary` to the shared conversation types. Give it serde
serialization for the Responses object shape and a compatibility deserializer
that accepts either an object or a legacy string, normalizing strings to
`summary_text`. Replace string vectors in `ConversationItem::Reasoning` and
`ReasoningData` with vectors of this type.

Next update the Responses response DTO and conversion code to retain typed
summary items. The request DTO should borrow the canonical type or map its
fields without changing them. Fallback summaries derived from reasoning content
and synthetic Chat Completions summaries should use `summary_text` objects.

Finally update focused tests and snapshots. Tests must prove that standard
object responses parse, legacy strings parse and normalize, old session records
load, new records serialize objects, and a valid object-summary response reaches
the judge verdict in one attempt. Keep the retry behavior tests for malformed
body failures unchanged.

## Concrete Steps

From `/Users/travisennis/Projects/cake-2`, run focused tests for the changed
modules while iterating:

    cargo test types::conversation
    cargo test types::session
    cargo test clients::responses
    cargo test clients::judge

Format the Rust changes with:

    cargo fmt --all -- --check

Before handoff, run the repository gate:

    just ci

Expected behavior is that standard Responses reasoning objects and legacy
string summaries both parse successfully, serialized new records contain
`summary` objects, and no retry is recorded for a valid response containing
typed summaries. The existing transient decode test must still demonstrate one
bounded recovery attempt.

## Validation and Acceptance

The change is accepted when a Responses payload containing
`"summary":[{"type":"summary_text","text":"..."}]` parses into a
`ConversationItem::Reasoning` whose summary retains both fields. A payload
containing `"summary":["..."]` parses into the same canonical object with
`type == "summary_text"`.

An existing session/stream record containing a string summary must deserialize,
and a newly serialized record must emit an object summary. A judge response with
typed reasoning output and a final verdict must complete with one provider
attempt. Empty or invalid 2xx bodies must retain the existing bounded retry and
fail-closed behavior when recovery is exhausted.

## Idempotence and Recovery

All code and test edits are additive or type-preserving; rerunning formatting and
focused tests is safe. Existing session files are read through compatibility
deserialization and are never rewritten. If a focused test exposes an unrelated
snapshot or baseline change, preserve it and isolate this plan's paths before
continuing. Do not reset or discard unrelated worktree changes.

## Artifacts and Notes

The primary artifacts are the shared summary type, its compatibility tests, the
updated Responses DTO conversion, and updated serialization snapshots. The
final test results and the WireMock sandbox limitation are recorded above; this
plan moves to `docs/exec-plans/completed/` after the retrospective.

## Interfaces and Dependencies

The implementation uses existing `serde` and `serde_json` facilities only. The
stable internal interface is `crate::types::conversation::ReasoningSummary`.
`crate::types::ConversationItem::Reasoning` and
`crate::types::session::ReasoningData` carry `Option<Vec<ReasoningSummary>>`.
The Responses backend consumes and produces the object shape while its
compatibility deserializer accepts legacy strings. No new dependency, provider
configuration key, session version, or retry category is required.
