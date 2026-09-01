# Emit in-flight provider-attempt telemetry

This ExecPlan is a living document for issue #110, maintained per `docs/workflow/exec-plans.md`.

## Purpose / Big Picture

Operators should be able to inspect a session telemetry sidecar while a provider request is still pending and tell whether Cake is waiting for response headers or reading a response body. Each provider attempt must eventually retain the existing completed `api_attempt` record, with explicit timeout, cancellation, or parse classification when the operation does not complete normally.

## Progress

- [x] (2026-08-31) Inspect issue #110, the telemetry writer/schema, provider backends, agent runner, readers, and focused tests.
- [x] (2026-08-31) Add bounded in-flight start and header-to-body phase records with session, invocation, task, turn, attempt, provider, model, and start-time context.
- [x] (2026-08-31) Close normal, timeout, parse-failure, HTTP, transport, and cancellation paths through the existing completed attempt record.
- [x] (2026-08-31) Add an incomplete-response-body regression test and serialization tests.
- [x] (2026-08-31) Run focused tests, strict Clippy, formatting, complexity, metrics, and documentation checks; record the inherited-sandbox blocker from the final fast gate.

## Surprises & Discoveries

- `reqwest::Client::send` resolves after response headers, while both Responses and Chat Completions parsers await the body afterward. This makes a bounded second phase record sufficient to distinguish header waits from body stalls without changing provider wire code.
- Existing sidecar readers append only records whose type is `api_attempt`; an additive `api_attempt_in_flight` type therefore preserves completed-attempt counts and old sidecars.
- The full local test gate currently has four unrelated macOS sandbox tests failing because the inherited execution sandbox denies temporary directories under `/Users/travisennis/Projects/cake`; focused provider and telemetry tests pass.

## Decision Log

- Decision: use append-only `api_attempt_in_flight` records rather than changing the existing `api_attempt` meaning or rewriting a prior record. Rationale: telemetry sidecars are append-only, and existing readers must continue to count only completed attempts. Date/Author: 2026-08-31 / Codex.
- Decision: emit at most two in-flight records per attempt: `awaiting_headers` before the request future and `reading_body` after headers arrive. Rationale: this gives live phase visibility while keeping record volume bounded. Date/Author: 2026-08-31 / Codex.
- Decision: use an attempt lifecycle guard to emit a `cancelled` completion when the async provider future is dropped. Rationale: code after an `.await` cannot run on cancellation, but the append-only sidecar should not leave an in-flight attempt ambiguous. Date/Author: 2026-08-31 / Codex.

## Outcomes & Retrospective

The sidecar now emits a flushed `api_attempt_in_flight` record before each provider request and a second bounded phase record after headers arrive. Existing `api_attempt` records remain the completed-attempt source for readers, with optional terminal phase and explicit timeout/cancellation classes. The incomplete-body test proves a body-phase timeout is recorded with status and duration; the cancellation test proves a dropped provider future closes its attempt. Focused Rust tests, the session telemetry integration suite, strict Clippy, `just cc-check`, formatting, documentation, and session-metrics tests pass. The fast `just check` and unfiltered full Rust suite are blocked by four pre-existing macOS sandbox tests that attempt to create temporary directories under `/Users/travisennis/Projects/cake`, outside the inherited execution grant; the same suite passes when only those four tests are skipped.

No provider request wire format, session transcript record, stream-json record, retry decision, or existing completed-attempt reader behavior changed.

## Context and Orientation

`src/clients/agent_runner.rs` owns the provider request/retry loop. It currently starts request and total timers, awaits `Backend::send_request`, awaits backend response parsing, and emits one `ApiAttemptTelemetry` only after those awaits settle. `src/session_telemetry.rs` owns the serializable sidecar record types and shared append-only writer. `src/clients/agent/agent_telemetry.rs` maps runner events to sidecar records. `src/types/session.rs` owns the shared bounded provider-attempt terminal vocabulary. `docs/integrations.md` and `scripts/session-metrics/cakelib.py` document or consume the sidecar while ignoring unknown record types.

The configured provider HTTP client has a five-minute total timeout covering request and response-body phases. The implementation must not change that production timeout or provider request payloads. A test-only client timeout hook is allowed so the incomplete-body test can finish quickly.

## Plan of Work

Add an `ApiAttemptInFlightTelemetry` record and a small `ApiAttemptPhase` enum to `src/session_telemetry.rs`, keeping the existing `ApiAttemptTelemetry` fields intact and adding only an optional terminal phase. Map the new runner event to an additive `api_attempt_in_flight` sidecar record. Extend `ApiAttemptTerminalClass` with bounded `timeout` and `cancelled` values.

In `src/clients/agent_runner.rs`, create an attempt lifecycle guard immediately before `Backend::send_request`. It emits the start record synchronously, emits a body-phase record after headers arrive, and emits the existing completed attempt event on every normal terminal path. Its `Drop` implementation emits a cancellation completion if an outer task drops the provider future. Classify request and body timeout errors as `timeout`; retain the existing request, parse, total, status, usage, retry, and response-failed behavior otherwise. Pass the existing task identity from `src/clients/agent/agent_loop.rs` and use the canonical provider strategy to identify inferred providers.

Add focused serialization coverage in `src/session_telemetry.rs` and an agent test with a raw TCP server that sends response headers plus a partial body and keeps the connection open. Configure a short test-only client timeout and assert that the sidecar contains the start record, body phase, and one completed `api_attempt` classified as a body-phase timeout. Update the telemetry integration contract and metrics loader record-type documentation.

## Concrete Steps

From `/Users/travisennis/Projects/cake/cake-2`, run the focused tests while iterating:

```
cargo test --bin cake clients::agent::tests::error_tests::incomplete_response_body_emits_in_flight_and_timeout_telemetry -- --exact
cargo test --test session_telemetry
cargo fmt --all -- --check
just cc-check
```

Before handoff, run `just check`. If the inherited macOS sandbox prevents the four existing tests that create temporary directories outside this checkout, retain the exact failure paths and report the focused passing checks; do not weaken sandbox rules for this telemetry change.

## Validation and Acceptance

A pending provider attempt produces a flushed `api_attempt_in_flight` record before request completion. If headers arrive, a second record has phase `reading_body` and the HTTP status. The final existing `api_attempt` record retains request/parse/total durations and status, and its terminal class explicitly reports `completed`, `timeout`, `cancelled`, `transport`, `http`, `body_parse`, or `response_failed` as applicable. Existing readers still count only `api_attempt` records, old records remain valid, and no request or response body enters telemetry.

## Idempotence and Recovery

All telemetry writes remain best-effort through the existing fail-stop shared writer. Re-running tests uses temporary directories and does not alter session data. If formatting changes only generated formatting, rerun the focused tests and inspect the diff before retaining it. If final validation is blocked by the inherited sandbox, do not modify unrelated sandbox code; report the blocker and the successful narrower checks.

## Artifacts and Notes

The primary proof is the raw sidecar emitted by the incomplete-body test: two bounded in-flight records followed by one completed timeout attempt. The existing `tests/session_telemetry.rs` suite is the compatibility proof for successful attempts, retries, usage, compensation, semantic recovery, and interruption.

## Interfaces and Dependencies

The change uses the existing `SessionTelemetryRecord`, `AgentRunnerTelemetryEvent`, `SharedSessionTelemetryWriter`, `Backend`, `ResolvedModelConfig`, `ModelProvider`, and `ApiAttemptTerminalClass` types. It adds no dependency and changes no provider wire interface. The new sidecar record is additive and consumers must continue to ignore unknown optional record types.
