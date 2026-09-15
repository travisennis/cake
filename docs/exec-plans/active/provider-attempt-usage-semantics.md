# Preserve provider response metadata and usage presence per attempt

This ExecPlan is a living document for issue #354, maintained per `docs/workflow/exec-plans.md`. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be kept current as work proceeds.

## Purpose / Big Picture

After this change, a person reading a session file or a telemetry sidecar can answer three questions that today they cannot: did the provider actually report this token count, or is the zero in the record an unknown value; which provider model served the response; and which provider response produced this usage record.

Concretely, `turn_usage` records in a session file gain `usage_presence`, `model`, `response_model`, and `provider_request_id`, and `api_attempt` records in the telemetry sidecar gain `provider`, `model`, `response_model`, and `usage_presence`. A `usage_presence` of `partial` means the provider sent a usage object with at least one required counter missing, so the zero in that position is unknown rather than measured; `unreported` means the provider sent no usage object at all. Reading a session for a run against a gateway that omits `total_tokens` shows `"usage_presence": "partial"` instead of a silently understated total.

Existing aggregates do not change shape or meaning: `task_complete.usage` and the completion JSON `usage` remain the sum of what providers reported.

## Progress

- [x] (2026-09-15) Inspect issue #354, parent #439, ADR-025, ADR-028, and the current telemetry, session, parser, and agent-runner code.
- [x] (2026-09-15) Record the decision as ADR-030 and open the feature branch.
- [x] (2026-09-15) Add `UsagePresence` and the internal `ReportedUsage` carrier in `src/types/usage.rs`.
- [x] (2026-09-15) Compute presence and capture the provider-reported model in both backends; thread both through `TurnResult` and the parse-error types.
- [x] (2026-09-15) Add provider, model, response model, and usage presence to `api_attempt` telemetry and `turn_usage` session records.
- [x] (2026-09-15) Update parser, telemetry, and session tests, including end-to-end coverage for success, HTTP failure, transport failure, and parse failure.
- [x] (2026-09-15) Update `docs/integrations.md` with the three-layer semantics and the presence policy; check the other documents the change touches.
- [x] (2026-09-15) Run the focused tests and the routed gates.

## Surprises & Discoveries

- The completed `api_attempt` record never carried the model at all; only the preceding `api_attempt_in_flight` record did. A reader that filters `api_attempt` records, as `scripts/session-metrics` does, therefore learned the model only by joining to the in-flight record or to `telemetry_init`.
- The temporary-signature design of ADR-025 ("a first successful attempt omits `attempt` and `terminal_class`") cannot extend to the new identity fields: those fields are most valuable exactly on the first successful attempt, which is the common case and the shape the rule protects. The rule therefore keeps applying to `attempt`/`terminal_class` only, and the new fields are written whenever they are known.
- `src/types/` may not import from `src/config/` (enforced by `just lint-deps`), so the session `turn_usage` record carries the model as a string and omits the resolved provider enum, which only `src/session_telemetry.rs` may serialize.

## Decision Log

- Decision: represent presence as a bounded `complete`/`partial`/`unreported` marker beside an unchanged `Usage`, rather than making `Usage` fields optional. Rationale: `Usage` is a sum accumulator embedded in several records and their snapshots; only the reporting layer needs the distinction. Date/Author: 2026-09-15 / ADR-030.
- Decision: carry usage and presence together in a small `ReportedUsage` value through the parser, the parse errors, `TurnResult`, and the settlement. Rationale: it keeps the two from drifting apart across the many plumbing sites. Date/Author: 2026-09-15 / ADR-030.
- Decision: capture the provider-reported model on the successful parse path only. Rationale: a terminal `response.failed` event carries bounded error metadata, not a full response, and inferring a model there would invent data. Date/Author: 2026-09-15 / ADR-030.
- Decision: do not add presence to the judge attempt record. Rationale: the issue scopes the main-provider path, and the judge's own diagnostics already record its usage, model, and API type. Date/Author: 2026-09-15.

## Outcomes & Retrospective

Delivered the contract in ADR-030. `UsagePresence` (`complete`, `partial`, `unreported`) and a `ReportedUsage` carrier exist in `src/types/usage.rs`; both backends compute presence from the raw usage object, capture the provider-reported `model`, and thread it through `TurnResult` and the parse-error types that already carried usage. `api_attempt` telemetry records now name the resolved provider, configured model, response-reported model, and usage presence, and `turn_usage` session records carry usage presence plus the provider response ID, configured model, and response-reported model.

Verification: focused parser, agent, session-record, and session-telemetry suite runs, then the routed gates (`just check`, documentation checks). What remains: nothing functional; issue #429 still owns documenting `turn_usage` ordering separately, and #422 still owns `api_attempt_in_flight` lifecycle verification.

## Context and Orientation

Cake is a Rust binary-only CLI. Three layers report token usage, and the issue asks for them to be distinguishable:

- **Provider attempt** --- one HTTP request. `src/clients/agent_runner.rs` owns the request/retry loop and emits `ApiAttemptTelemetry` (an `api_attempt` sidecar record) once per attempt, plus `api_attempt_in_flight` phase records through `src/clients/agent/agent_telemetry.rs`.
- **Agent turn** --- one loop iteration that may contain several attempts. `src/clients/agent.rs` owns `TurnResult` (the parsed provider turn) and settles usage into the session ledger through `record_turn_usage`, which writes one `turn_usage` record per attempt that reports usage (ADR-025).
- **Task aggregate** --- `task_complete.usage` in the session file and `usage` in completion JSON, summed in `src/clients/agent_state.rs::accumulate_usage` and formatted in `src/cli/output.rs`.

Provider parsing lives in `src/clients/responses.rs` (Responses API, including its SSE stream) and `src/clients/chat_completions.rs` (Chat Completions). Their wire DTOs live in `src/clients/responses_types.rs` and `src/clients/chat_types.rs`. Both map the provider's usage object through a private `map_usage` into the canonical `Usage` in `src/types/usage.rs`, whose counters are plain `u64` fields. `Usage` is `Copy` and has a `Default`, and it is embedded in `turn_usage`, `task_complete`, `session_summary`, and judge telemetry, with snapshots under `src/types/snapshots/`.

Serialized shapes live in two places: `src/types/session.rs` for session JSONL records (`TurnUsageData`), and `src/session_telemetry.rs` for sidecar records (`ApiAttemptTelemetry`, `ApiAttemptInFlightTelemetry`, and the `SessionTelemetryRecord` enum that wraps them). `docs/integrations.md` states the consumer-facing semantics of both.

Terms used here: a **usage object** is the provider's `usage` JSON member; a **required counter** is one of `input_tokens`/`output_tokens`/`total_tokens` (the Responses names) or `prompt_tokens`/`completion_tokens`/`total_tokens` (the Chat Completions names); **presence** is whether those counters were present; the **response ID** is the provider's `id`; the **response-reported model** is the provider's own `model` member, which may differ from the configured model when a gateway routes an alias; and the **terminal class** is the bounded `ApiAttemptTerminalClass` vocabulary (`completed`, `timeout`, `cancelled`, `transport`, `http`, `body_parse`, `response_failed`).

## Plan of Work

In `src/types/usage.rs`, add a bounded `UsagePresence` enum (`unreported`, `partial`, `complete`) with snake_case serialization and an `unreported` default, and a `ReportedUsage` value pairing a `Usage` with its presence. Re-export both from `src/types/mod.rs`.

In `src/clients/responses.rs` and `src/clients/chat_completions.rs`, change `map_usage` to return `ReportedUsage`, computing presence from which of the three required counters were present in the raw usage object and keeping the existing per-field warning log for a missing counter. Add an optional `model` to `ApiResponse`/`ApiResponseEnvelope` in `src/clients/responses_types.rs` and to `ChatResponse` in `src/clients/chat_types.rs`, and carry it on `TurnResult` as `response_model`. For the Responses SSE path, add `model` to `StreamAccumulator` and extract it from the terminal event's response object in `apply_response_metadata`.

In `src/clients/backend.rs`, change `ResponseParseError`, `ResponseDecodeError`, `ResponsesStreamFailed`, and `Backend::reported_usage` to carry `ReportedUsage` instead of `Usage`, so presence survives the failure paths that already retained usage. Update `src/clients/judge_observer.rs` to project the plain `Usage` out of a `ReportedUsage` for the judge's own record.

In `src/clients/agent.rs`, extend `TurnResult` with `response_model` and change its `usage` to `Option<ReportedUsage>`; extend `TurnUsageSettlement` with the bounded provider identity (provider request ID, configured model, response model) and the presence-carrying usage; and write the new fields in `record_turn_usage`. In `src/types/session.rs`, add optional `usage_presence`, `model`, `response_model`, and `provider_request_id` to `TurnUsageData`, keeping the existing fields and omission rule for `attempt`/`terminal_class` untouched.

In `src/session_telemetry.rs`, add optional `provider`, non-optional `model`, optional `response_model`, and non-optional `usage_presence` to `ApiAttemptTelemetry`. In `src/clients/agent_runner.rs`, populate them at each completion site: the successful path supplies the response model from the parsed turn, the HTTP-failure and transport-failure paths report `unreported`, and the cancellation path in the `Drop` implementation supplies the in-flight identity with `unreported` and no response model.

Tests are updated where shapes changed (`src/clients/responses_response_parsing_tests.rs`, `src/clients/responses_tests.rs`, `src/clients/chat_completions_tests.rs`, `src/clients/judge_tests.rs`, `src/clients/judge_benchmark_tests.rs`, `src/types/session_tests.rs`, `src/clients/agent/agent_tests.rs`) and added where the contract is new: parser tests for complete, partial, missing, and explicit-zero usage in both backends; session-record tests for the new fields and for legacy deserialization; and `tests/session_telemetry.rs` end-to-end coverage asserting the attempt record for a success, an HTTP failure, a transport failure, and a parse failure.

Finally, `docs/integrations.md` gains the provider-attempt versus agent-turn versus task-aggregate statement and the presence policy, and its record-semantics and telemetry-sidecar sections name the new fields.

## Concrete Steps

From `/Users/travisennis/Projects/cake/cake-1`, iterate with focused tests:

```
cargo test --bin cake clients::responses_response_parsing_tests
cargo test --bin cake clients::chat_completions_tests
cargo test --bin cake types::session
cargo test --test session_telemetry
```

Expected: each suite passes, including the new presence assertions. Then run the routed gate and the documentation checks:

```
cargo fmt --all
just check
just pre-push-docs
```

## Validation and Acceptance

Parsing a Responses or Chat Completions body whose usage object carries all three required counters records `usage_presence: complete` and preserves every counter. A body whose usage object carries explicit zeros records `complete` as well, with zeros in the counters, so a provider-reported zero stays distinguishable from an absent counter. A body whose usage object omits one or more required counters records `partial`, keeps the reported counters, and leaves the absent ones at zero. A body with no usage object records `usage` as null and `usage_presence` as `unreported`.

In a real invocation against a mock provider, the telemetry sidecar's `api_attempt` record names the configured model, the resolved provider when one is recognized, the response-reported model, and the presence; the session file's `turn_usage` record names the provider response ID, the configured model, the response-reported model, and the presence. An HTTP failure, a transport failure, and a body-parse failure each produce an `api_attempt` record with a status or error and `usage: null`, `usage_presence: unreported`. `task_complete.usage` still sums exactly the reported counters.

## Idempotence and Recovery

Every telemetry and session write remains append-only and best-effort, so re-running any test or invocation cannot corrupt an existing file. The snapshot updates are regenerated with `cargo insta accept` (or `just snapshots` then `cargo insta review`) and are safe to repeat. If the routed gate fails on an unrelated pre-existing test, record the exact failure and the narrower checks that passed rather than weakening the gate.

## Artifacts and Notes

A partial-usage attempt record looks like this (indented, from the session telemetry suite):

```
{
  "type": "api_attempt",
  "turn_index": 1,
  "attempt": 1,
  "model": "glm-5.1",
  "response_model": "glm-5.1",
  "usage": {"input_tokens": 100, "output_tokens": 50, "total_tokens": 0, ...},
  "usage_presence": "partial",
  "terminal_class": "completed"
}
```

The same attempt's `turn_usage` record carries the same presence plus the provider response ID:

```
{
  "type": "turn_usage",
  "turn": 1,
  "model": "glm-5.1",
  "response_model": "glm-5.1",
  "provider_request_id": "resp-123",
  "usage_presence": "partial",
  "usage": {"input_tokens": 100, "output_tokens": 50, "total_tokens": 0, ...}
}
```

## Interfaces and Dependencies

- `crate::types::UsagePresence` (new enum: `Unreported`, `Partial`, `Complete`) and `crate::types::ReportedUsage` (new struct: `usage: Usage`, `presence: UsagePresence`), both `Copy`.
- `crate::clients::agent::TurnResult` gains `response_model: Option<String>` and changes `usage` to `Option<ReportedUsage>`.
- `crate::types::TurnUsageData` gains optional `usage_presence`, `model`, `response_model`, and `provider_request_id`.
- `crate::session_telemetry::ApiAttemptTelemetry` gains `provider: Option<ModelProvider>`, `model: String`, `response_model: Option<String>`, and `usage_presence: UsagePresence`.
- No new dependency; no provider request payload, retry decision, exit code, or CLI flag changes.
