# Retry undecodable judge responses without weakening fail-closed safety

This ExecPlan is a living document, maintained per `docs/workflow/exec-plans.md`. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be kept current as work proceeds.

## Purpose / Big Picture

Cake's command-safety judge blocks a Bash command when its provider cannot produce a verdict. A Responses API provider has intermittently returned HTTP 200 bodies that Cake cannot decode, leaving the user with an opaque error and no automatic recovery. After this change, an undecodable successful response gets one retry within the existing bounded deadline and still fails closed if the retry cannot produce a verdict. The final error includes enough bounded body detail to diagnose the provider response.

Issue #286 tracks the work. ADR-022 records the decision and partially supersedes ADR-020's treatment of body-decode failures as terminal.

## Progress

- [x] (2026-08-16) Confirmed the recurring failure class in persisted session telemetry and separated observed facts from possible causes.
- [x] (2026-08-16) Added a typed body-decode error with a bounded preview for both supported provider backends.
- [x] (2026-08-16) Added one bounded fresh-client recovery for body-decode failures while keeping semantic parse failures terminal.
- [x] (2026-08-16) Added Responses API regression coverage and updated the configuration, integration, and decision records.
- [x] (2026-08-16) Ran the focused Responses API recovery and terminal semantic-parse tests, then passed the repository's final `just ci` gate.

## Surprises & Discoveries

- Observation: the persisted judge telemetry proves repeated HTTP 200 `response_parse` outcomes for the configured Responses API model, but the old error path does not retain the response body. Evidence: failed attempts lack a provider response id, termination, and usage, while neighboring attempts with the same model and backend return valid verdicts.
- Observation: the initial implementation made every backend parse error retryable. Some backend parse errors occur after successful JSON envelope decoding and are semantic provider-output failures, so retrying all of them exceeded ADR-022's scope.
- Observation: the initial recovery test used Chat Completions even though the reported failure used the Responses API.

## Decision Log

- Decision: classify retryability with a typed `ResponseDecodeError` at the backend boundary. Rationale: the judge driver can recover only the approved body-decode class without string matching or retrying semantic parse errors. Date/Author: 2026-08-16 / Codex.
- Decision: use a fresh HTTP client for the recovery, but do not claim that a pooled connection caused the original failure. Rationale: a fresh client excludes connection reuse from the second attempt; the old diagnostics do not identify whether the invalid body came from the provider, proxy, or connection path. Date/Author: 2026-08-16 / Codex.
- Decision: keep the retry fail-closed and within the existing `timeout_secs + retry_budget_secs` operation deadline. Rationale: resilience must not create a path that runs a command without a valid verdict. Date/Author: 2026-08-16 / Travis Ennis.

## Outcomes & Retrospective

The implementation now distinguishes undecodable HTTP 2xx bodies from later semantic backend parse failures. Only the former receives one bounded retry, and Responses API tests cover recovery, disabled recovery, diagnostic detail, and terminal semantic parsing. The change improves client resilience but does not repair an upstream service that repeatedly returns invalid bodies. The bounded preview supplies the missing evidence needed for any provider-specific follow-up.

Verification passed with `cargo test judge_response_parse_failure`, `cargo test judge_semantic_response_parse_failure_does_not_retry`, and `just ci`. The focused tests required execution outside the workspace sandbox because wiremock binds localhost ports; the initial sandboxed attempt failed only at socket binding before test behavior ran.

## Context and Orientation

`src/clients/responses.rs` and `src/clients/chat_completions.rs` translate raw provider responses into Cake's shared `TurnResult`. `src/clients/backend.rs` dispatches between those backends. `src/clients/judge_observer.rs` records each judge attempt and decides whether one recovery attempt is allowed.

Before this work, any `anyhow::Error` from backend response parsing received the telemetry class `response_parse` and was terminal. The provider body was decoded through reqwest, whose top-level error omitted the underlying serde cause from Cake's user-visible message. ADR-020 already permits one recovery for bounded timeout, transport, and retryable HTTP failures.

## Plan of Work

Add a typed response-body decode error in `src/clients/backend.rs`. Both backend adapters read a successful response body once, deserialize from those bytes, and attach at most 400 bytes of lossy UTF-8 preview to that typed error.

In `src/clients/judge_observer.rs`, keep the existing `response_parse` telemetry class, but make retry input available only when the error downcasts to the typed body-decode error. Reuse the existing network retry reason, policy backoff, fresh-client construction, and total operation deadline.

In `src/clients/judge_tests.rs`, drive the failing Responses API path through wiremock. Cover successful recovery, zero-budget fail-closed behavior, diagnostic detail, and a decoded but malformed function-call envelope that must remain terminal. Update ADR-020, ADR-022, `docs/configuration.md`, and `docs/integrations.md` to state the narrowed behavior and the unresolved upstream cause.

## Concrete Steps

Work from `/Users/travisennis/Projects/cake-2` on `fix/retry-undecodable-judge-responses`.

Run the focused tests:

```
cargo test judge_response_parse_failure
cargo test judge_semantic_response_parse_failure_does_not_retry
```

Format and run the complete repository gate:

```
cargo fmt
just ci
```

Inspect the final change and commit only the task paths with a Conventional Commit message using the `tools` scope.

## Validation and Acceptance

A wiremock Responses endpoint that returns invalid JSON once and a valid verdict next must produce that verdict with two attempt records. With no retry budget, the first invalid body must fail closed after one attempt. A valid JSON envelope containing an unusable function call must fail after one attempt even when retry budget exists. The error for an invalid body must include the serde cause and bounded preview. `just ci` must pass before commit.

## Idempotence and Recovery

The tests, formatter, and verification commands are safe to rerun. If a test fails, retain the worktree and rerun the focused test after a narrow edit. Do not weaken fail-closed behavior or bypass the judge to make the production path pass. The branch was created from `origin/master`; no reset or destructive recovery is required.

## Artifacts and Notes

The local persisted-session and telemetry files are diagnostic inputs only. They are not modified or committed, and their private identifiers are not published in the issue or repository records.

## Interfaces and Dependencies

`crate::clients::backend::ResponseDecodeError` is the internal typed boundary between backend deserialization and judge retry classification. `crate::clients::judge_observer::judge_observed` remains the single bounded judge recovery driver. No public Rust API, configuration key, serialized telemetry field, CLI flag, exit code, sandbox permission, or dependency changes.
