# Make cake retry transient API failures intelligently

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agents/PLANS.md`.

## Purpose / Big Picture

After this change, `cake` should recover from the transient provider failures that currently interrupt otherwise-valid runs. A short rate limit, overloaded upstream, stale pooled connection, or parseable context-window overflow should lead to a bounded, visible retry instead of an immediate failure. A user should be able to run `cake <prompt>`, see retry progress in text mode, and get the final answer when the provider recovers, while still failing fast on permanent problems such as bad credentials or invalid requests.

The observable outcome is simple. When an API returns `429`, `529`, `503`, a connection reset, or a parseable context-overflow error, `cake` retries with an appropriate delay and logs why. When an API returns `401`, `403`, or an ordinary `400`, `cake` stops immediately with the same actionable error it shows today.

## Progress

- [x] 2026-04-28 00:11Z Reviewed `.agents/.research/topics/retry-strategy.md` and the current retry path in `src/clients/agent.rs`, `src/clients/responses.rs`, `src/clients/chat_completions.rs`, `src/main.rs`, and `src/config/model.rs`.
- [x] 2026-04-28 00:11Z Wrote the initial ExecPlan and scoped the research down to ideas that fit cake's current architecture.
- [x] 2026-04-28 20:17Z Extracted retry classification, delay calculation, context-overflow recovery, and transport classification into `src/clients/retry.rs`, then threaded request overrides through both API backends.
- [x] 2026-04-28 20:17Z Replaced the inline retry loop in `src/clients/agent.rs`, added a retry callback, and wired text-mode spinner updates in `src/main.rs` without changing session history.
- [x] 2026-04-28 20:17Z Added focused retry unit tests and high-level `wiremock` coverage for `Retry-After`, vendor `529`, overloaded bodies, `x-should-retry: false`, and one-shot context-overflow recovery.
- [x] 2026-04-28 20:26Z Ran the focused retry tests, fixed the borrow and clippy issues they surfaced, and finished with a passing `just ci` from the repository root.
- [x] 2026-04-28 20:49Z Applied review follow-up fixes so overloaded `500/502/503/504` responses still retry when the body marks them as overloaded, hardened context-overflow parsing against trailing numeric metadata, and re-ran `just ci` successfully.

## Surprises & Discoveries

- Observation: `cake` currently retries only `429`, `500`, `502`, `503`, and `504`, plus `reqwest` connect and timeout errors. The delay sequence is fixed at `1s`, `2s`, and `4s`, with no jitter and no header parsing.
  Evidence: `src/clients/agent.rs` defines `MAX_RETRIES`, `INITIAL_DELAY_SECS`, `is_retryable_status`, `retry_delay`, and `check_retry`, then uses them directly in `Agent::complete_turn`.

- Observation: The current loop does not inspect `Retry-After`, `x-should-retry`, custom overload status `529`, or error bodies, so it cannot distinguish "wait briefly and retry" from "this request is malformed".
  Evidence: `Agent::complete_turn` only branches on `response.status()` and a small `reqwest::Error` check before either sleeping or returning the final error.

- Observation: `cake` already has a user-visible progress surface for text mode, but retry waits do not appear there because the spinner only reacts to `ConversationItem` values emitted after the provider has responded.
  Evidence: `src/main.rs` wires `with_text_progress` through `Agent::with_progress_callback`, and `format_spinner_message` only formats `ConversationItem` values.

- Observation: Both API backends return raw `reqwest::Response`, which means a shared retry layer can sit above `src/clients/responses.rs` and `src/clients/chat_completions.rs` without duplicating provider-specific request code.
  Evidence: `send_request` in both `src/clients/responses.rs` and `src/clients/chat_completions.rs` returns `anyhow::Result<reqwest::Response>`.

- Observation: The research note's fixed `FLOOR_OUTPUT_TOKENS = 3000` is not portable to cake because cake supports arbitrary OpenAI-compatible providers and user-selected output budgets that may be much smaller.
  Evidence: `src/config/model.rs` exposes optional, per-model `max_output_tokens` and `reasoning_max_tokens` instead of a single provider-specific token policy.

- Observation: The repository already has strong HTTP retry tests via `wiremock`, but those tests only model HTTP responses, not lower-level broken pipes or connection resets.
  Evidence: `src/clients/agent.rs` error tests mount `ResponseTemplate` values and never synthesize raw socket failures.

- Observation: Context-overflow recovery can be computed from the provider error body alone, so the retry loop does not need to mutate `ResolvedModelConfig` to lower output budgets.
  Evidence: `src/clients/retry.rs` now parses `input length and max_tokens exceed context limit: ...`, derives a one-shot `RequestOverrides`, and leaves `ResolvedModelConfig` untouched.

- Observation: `pool_max_idle_per_host(0)` is the smallest reqwest change that disables idle connection reuse while preserving the existing timeout configuration.
  Evidence: `src/clients/agent.rs` now builds the stale-connection repair client with the same timeout settings as the default client and only changes the idle-pool limit.

- Observation: A provider can send contradictory transient signals, such as `x-should-retry: false` alongside an `overloaded_error` body, so the classifier has to prioritize the stronger transient marker instead of applying header advice first.
  Evidence: `src/clients/retry.rs` now checks the overloaded marker before letting `x-should-retry: false` suppress retries for `500/502/503/504`, and the new `x_should_retry_false_does_not_block_overloaded_error` unit test covers that exact combination.

## Decision Log

- Decision: Implement only the retry strategies that match cake's current product shape: bounded transient retries, provider-header awareness, stale-connection recovery, and one-shot context-overflow recovery.
  Rationale: cake is an interactive CLI with environment-based API keys, no subscriber-tier metadata, no fast mode, no fallback-model mapping, and no long-running background workers. Importing the research note's fast-mode cooldowns, permanent overage disables, or multi-hour unattended loops would add complexity with no current runtime to support it.
  Date/Author: 2026-04-28 / Amp

- Decision: Keep `401` and `403` terminal for now.
  Rationale: cake resolves API keys from environment variables in `ResolvedModelConfig` and has no OAuth refresh, token cache, or remote-control-plane auth path. Retrying credential failures without refreshing credentials only delays a useful error.
  Date/Author: 2026-04-28 / Amp

- Decision: Add a dedicated `src/clients/retry.rs` module instead of expanding the existing inline loop.
  Rationale: the new logic needs pure, testable functions for delay calculation, header parsing, error classification, and request mutation. Keeping all of that inside `Agent::complete_turn` would make both the implementation and the tests fragile.
  Date/Author: 2026-04-28 / Amp

- Decision: Treat `x-should-retry` as advisory, not absolute.
  Rationale: provider headers are useful hints, but cake still needs to retry obvious transient failures such as `429`, `529`, `408`, `409`, and transport-level disconnects even if an upstream does not set the header consistently.
  Date/Author: 2026-04-28 / Amp

- Decision: Do not add retry wait events to `SessionRecord` in this iteration.
  Rationale: retry waits are transport noise, not conversation history. Persisting them would expand the session and stream-json schema for a runtime-only concern. Text mode should surface waits through the spinner, and all modes should log retry details through `tracing`.
  Date/Author: 2026-04-28 / Amp

- Decision: Use a generic minimum output floor of `256` tokens for context-overflow recovery instead of importing the research note's provider-specific `3000` token floor.
  Rationale: cake targets many providers and small-model configurations. A small, generic floor prevents nonsensical retries while staying compatible with modest token budgets.
  Date/Author: 2026-04-28 / Amp

- Decision: Prefer deterministic bounded jitter derived from `session_id` and attempt number over a new random-number dependency.
  Rationale: the goal is to de-synchronize retries across concurrent sessions, not to produce high-quality randomness. A small hash-based jitter keeps the implementation dependency-free and easy to unit test.
  Date/Author: 2026-04-28 / Amp

- Decision: Store the one-shot context-overflow guard inside `RequestOverrides` rather than adding a second retry-state struct to `Agent`.
  Rationale: the retry loop already threads `RequestOverrides` through both backends. Keeping the guard bit next to the override values makes the one-shot recovery rule local to `src/clients/retry.rs` and avoids another piece of mutable agent state.
  Date/Author: 2026-04-28 / Amp

## Outcomes & Retrospective

The retry work is complete. `cake` now retries transient HTTP failures and stale transport failures through a shared retry module, honors `Retry-After`, understands vendor `529` and overloaded-error bodies, performs a one-shot context-overflow retry with lowered token budgets, and surfaces retry waits in text mode without changing session history. The focused retry tests passed, `just ci` passed, and the remaining gap is only future product expansion if cake ever adds more provider-specific retry concepts.

## Context and Orientation

`cake` sends one provider request per agent turn. `Agent::send` in `src/clients/agent.rs` loops until the model stops making tool calls. Each turn calls `Agent::complete_turn`, which picks either `src/clients/responses.rs` or `src/clients/chat_completions.rs` based on `ResolvedModelConfig.config.api_type`. Both backends build a JSON request from the current conversation and return a raw `reqwest::Response`. After a successful response, they parse it into `TurnResult`, which contains `ConversationItem` values and optional token usage.

Today, retry behavior lives entirely inside `Agent::complete_turn`. A "transient error" means an error that may succeed a moment later without the user changing the prompt, model, or credentials. Typical examples are rate limits, overloaded upstreams, gateway errors, and dropped connections. A "retry budget" means the maximum number of additional attempts that cake will spend on one turn before surfacing the failure. A "request override" means a temporary modification to one request, such as lowering `max_output_tokens` after parsing a context-window overflow error, without rewriting the user's saved model settings.

Three existing modules matter most.

`src/clients/agent.rs` owns the retry loop, the reusable `reqwest::Client`, and the progress callbacks. `src/main.rs` turns those progress callbacks into the terminal spinner in text mode. `src/config/model.rs` defines the tunable token fields that a retry can safely override for one request: `max_output_tokens` for both backends, and `reasoning_max_tokens` for Responses API models.

The research note contains several ideas that do not belong in cake yet. Cake does not have fast mode, subscription tiers, background summarizers, fallback models, multi-hour unattended workers, provider-specific OAuth refresh, or streaming chunk watchdogs. Those branches are intentionally excluded from this plan unless the product surface changes later.

## Plan of Work

The first milestone is to extract retry logic into a shared module. Add `mod retry;` to `src/clients/mod.rs` and create `src/clients/retry.rs`. That file should hold the retry constants, the pure backoff functions, header parsing helpers, error classifiers, context-overflow parser, and the small state objects that describe per-turn retry progress. The goal is to make retry decisions testable without sending real HTTP requests or sleeping in unit tests.

The second milestone is to thread request overrides through both backends. Change the signatures of `responses::send_request` and `chat_completions::send_request` so they accept a `RequestOverrides` value from the retry layer. In `src/clients/responses.rs`, use the override value instead of `config.config.max_output_tokens` when present, and clamp `reasoning.max_tokens` when `reasoning_max_tokens` is configured and the retry state has lowered the available token budget. In `src/clients/chat_completions.rs`, use the override value for `max_completion_tokens`. These are per-turn overrides only. They must not mutate `ResolvedModelConfig` or anything written back to `settings.toml`.

The third milestone is to replace the inline loop in `Agent::complete_turn`. Change `complete_turn` to take `&mut self` so it can swap clients after stale-connection failures and report retry status while waiting. For non-success HTTP responses, read the status, clone the headers, and consume the response body into a structured `HttpFailure` value before deciding whether to retry. The new classifier should retry `408`, `409`, `429`, `500`, `502`, `503`, `504`, and vendor `529`. It should also treat a response body containing a parseable overloaded-provider marker such as `overloaded_error` as retryable even if the provider did not preserve `529`. `Retry-After` should override the exponential schedule when present, and the parser should support both integer seconds and HTTP-date values. `x-should-retry: false` should stop retries for ordinary `400`-class and `500`-class failures, but it must not suppress retries for `429`, `529`, `408`, `409`, or transport-level disconnects. `x-should-retry: true` should be treated as an extra signal to allow retry when the status is otherwise borderline, not as permission to retry malformed requests forever.

The fourth milestone is transport recovery. Add a helper in `src/clients/agent.rs` that builds the standard HTTP client with the existing timeouts, and a second variant that disables idle connection reuse. When a transport error is classified as a stale pooled connection, such as a connect failure, timeout, connection reset, broken pipe, or unexpected EOF from the source chain, retry with the no-reuse client for the remainder of that turn. After a successful turn, restore the default client for the next turn. Keep the retry budget bounded. This is a repair path for stale sockets, not a new persistent mode.

The fifth milestone is context-overflow recovery. When a response is `400` and the body matches the provider error shape `input length and max_tokens exceed context limit: {input_tokens} + {requested_tokens} > {context_limit}` or an equivalent parseable form, compute a one-shot token override instead of failing immediately. Use a `1024` token safety buffer. Calculate `available_output = context_limit - input_tokens - 1024`, saturating at zero. If the result is below `256`, fail with the original API error because there is no useful automatic recovery left. Otherwise, retry once with `max_output_tokens = available_output.min(previous_requested_output_tokens)` and, for Responses API only, `reasoning_max_tokens = min(previous_reasoning_budget, available_output.saturating_sub(1))` when a reasoning budget was configured. Do not apply this adjustment more than once per turn. If the second attempt still overflows, return the provider error.

The sixth milestone is user-visible retry reporting and logging. Keep session history unchanged, but add a dedicated retry-status callback on `Agent` so text mode can show messages such as `Retrying in 1.2s after 429 rate limit (attempt 2/5)` while the wait is happening. `src/main.rs` should format these status updates into spinner messages without inventing new `ConversationItem` variants. Every retry attempt should also emit a `debug!` or `info!` log with the status, reason, delay, and attempt count. That gives humans a clear view in text mode and keeps stream-json output stable.

The final milestone is verification. Add unit tests in `src/clients/retry.rs` for backoff capping, deterministic jitter bounds, `Retry-After` parsing, `x-should-retry` interpretation, and context-overflow parsing. Extend the existing `wiremock` coverage in `src/clients/agent.rs` with end-to-end tests for `429` with `Retry-After`, vendor `529`, overloaded JSON bodies, `503` with `x-should-retry: false`, and one-shot context-overflow recovery. Use unit tests rather than `wiremock` for stale-socket transport classification because those conditions are difficult to synthesize at the HTTP layer.

## Concrete Steps

Run all commands from the repository root, `/Users/travisennis/Projects/cake`.

Start by confirming the current baseline behavior before editing:

    cargo test test_429_too_many_requests_retries_and_succeeds
    cargo test test_401_unauthorized_returns_error

After adding `src/clients/retry.rs`, add focused unit tests and run them during development:

    cargo test parse_retry_after_delta_seconds
    cargo test parse_retry_after_http_date
    cargo test x_should_retry_false_blocks_retry
    cargo test parse_context_overflow_reduces_output_budget

After replacing the retry loop and threading request overrides through both backends, run the higher-level tests:

    cargo test test_429_retry_after_header_is_honored
    cargo test test_529_overloaded_retries_and_succeeds
    cargo test test_overloaded_error_body_retries_and_succeeds
    cargo test test_context_overflow_reduces_max_output_tokens_once
    cargo test test_401_unauthorized_returns_error

Finish with the project-wide verification required by this repository:

    just ci

The expected short transcript for the focused tests is ordinary Rust success output such as:

    test ...::parse_retry_after_delta_seconds ... ok
    test ...::test_529_overloaded_retries_and_succeeds ... ok
    test result: ok. N passed; 0 failed

The expected result of `just ci` is that formatting, linting, and the full test suite complete without errors.

## Validation and Acceptance

The implementation is acceptable when the following behavior is demonstrable.

Running `cake <prompt>` against a mock provider that returns `429` with `Retry-After: 1` should pause for roughly one second, show a retry message in text mode, then succeed on the next response without requiring any user intervention.

Running against a mock provider that returns vendor status `529` or an overloaded JSON error body should retry using bounded exponential backoff plus deterministic jitter, then succeed if a later response is `200`.

Running against a mock provider that returns `401` or `403` should still fail immediately, with no extra delay and the same exit-code classification that cake uses today for API failures.

Running against a mock provider that returns a parseable context-window overflow `400` should retry exactly once with a lower `max_output_tokens` value. The second request body must prove that the lowered budget was sent. If the provider still returns overflow, cake must stop instead of looping.

The focused retry tests and `just ci` must all pass.

## Idempotence and Recovery

This plan is intentionally additive. Extracting retry code into `src/clients/retry.rs` can be done incrementally while keeping the old loop in place until the new classifier is ready. Thread request overrides into the backend request builders before turning on context-overflow retries so that each step is testable in isolation.

If a partial implementation causes real sleeps in tests to become slow or flaky, keep the pure parsing and delay logic in unit tests and temporarily limit end-to-end retry tests to one delayed attempt with a very short wait. Do not add indefinite retry loops or long sleep durations during implementation. If the stale-connection repair path proves awkward with client mutation, fall back to rebuilding a fresh default client per retry attempt before considering broader architectural changes.

## Artifacts and Notes

The current code path that will be replaced is concise, which makes the gap easy to demonstrate:

    const MAX_RETRIES: u32 = 3;
    const INITIAL_DELAY_SECS: u64 = 1;

    const fn is_retryable_status(status: reqwest::StatusCode) -> bool {
        matches!(status.as_u16(), 429 | 500 | 502 | 503 | 504)
    }

That helper set is the reason cake currently misses `529`, `Retry-After`, `x-should-retry`, and context-overflow recovery.

The desired spinner message shape is equally small and concrete:

    Retrying in 1.2s after 429 rate limit (attempt 2/5)
    Retrying in 0.6s after stale connection reset (attempt 1/5)
    Retrying once with max_output_tokens=3584 after context overflow

Those messages are runtime-only. They should not be saved into session history.

## Interfaces and Dependencies

In `src/clients/retry.rs`, define the retry-specific types and helpers that the rest of the implementation depends on:

    pub(super) const MAX_RETRIES: u32 = 5;
    pub(super) const BASE_DELAY_MS: u64 = 500;
    pub(super) const MAX_BACKOFF_MS: u64 = 30_000;

    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub(super) struct RequestOverrides {
        pub max_output_tokens: Option<u32>,
        pub reasoning_max_tokens: Option<u32>,
        pub context_overflow_retry_used: bool,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) enum RetryReason {
        RateLimit,
        Overloaded,
        ServerError,
        RequestTimeout,
        LockTimeout,
        Network,
        ContextOverflow,
    }

    #[derive(Debug, Clone)]
    pub(super) struct RetryStatus {
        pub attempt: u32,
        pub max_retries: u32,
        pub delay: std::time::Duration,
        pub reason: RetryReason,
        pub detail: String,
    }

    #[derive(Debug, Clone)]
    pub(super) struct HttpFailure {
        pub status: u16,
        pub headers: reqwest::header::HeaderMap,
        pub body: String,
    }

    #[derive(Debug, Clone)]
    pub(super) enum RetryDecision {
        Retry { status: RetryStatus },
        RetryWithOverrides {
            status: RetryStatus,
            overrides: RequestOverrides,
        },
        DoNotRetry,
    }

    pub(super) fn classify_http_failure(
        failure: &HttpFailure,
        attempt: u32,
        session_id: uuid::Uuid,
        current_overrides: &RequestOverrides,
    ) -> RetryDecision;

    pub(super) fn classify_transport_error(
        error: &anyhow::Error,
        attempt: u32,
        session_id: uuid::Uuid,
    ) -> RetryDecision;

    pub(super) fn should_disable_connection_reuse(error: &anyhow::Error) -> bool;

In `src/clients/agent.rs`, change `async fn complete_turn(&self)` to `async fn complete_turn(&mut self)` and add a retry-status callback alongside the existing progress callback:

    pub fn with_retry_callback(
        mut self,
        callback: impl Fn(&crate::clients::retry::RetryStatus) + Send + Sync + 'static,
    ) -> Self;

Also add a small helper for client creation so default and no-reuse clients use the same timeout configuration.

In `src/clients/responses.rs` and `src/clients/chat_completions.rs`, change `send_request` to accept `&RequestOverrides` and prefer those values over `ResolvedModelConfig.config.max_output_tokens` and `ResolvedModelConfig.config.reasoning_max_tokens` when present.

No new external dependency should be introduced unless a concrete implementation obstacle appears. The retry module should rely on `reqwest`, `anyhow`, `uuid`, `tokio`, and the standard library types that are already in the repository.

Revision Note (2026-04-28 00:11Z, Amp): Created the initial ExecPlan from `.agents/.research/topics/retry-strategy.md`, translated the research into cake-specific milestones, and explicitly excluded fast-mode, auth-refresh, fallback-model, and persistent unattended retry ideas because cake does not implement those concepts today.

Revision Note (2026-04-28 20:17Z, Amp): Updated the living plan after landing the shared retry module, per-turn request overrides, the new agent retry loop, text-mode retry callback wiring, and focused retry tests so the next contributor can resume from the current implementation state instead of the initial planning state.

Revision Note (2026-04-28 20:26Z, Amp): Updated the living plan after the verification pass so `Progress` and `Outcomes & Retrospective` reflect the passing focused tests, the follow-up fixes they required, and the final successful `just ci` run.

Revision Note (2026-04-28 20:49Z, Amp): Updated the living plan after review-driven follow-up fixes so it records the overloaded-plus-header precedence fix, the more targeted context-overflow parser, and the final post-review `just ci` pass.

Revision Note (2026-05-07, Codex): Moved this completed ExecPlan from `.agents/.plans/` to `.agents/exec-plans/completed/` during the ExecPlan directory migration.
