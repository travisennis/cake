# Retry transient LLM-judge failures within a bounded deadline

This ExecPlan is a living document, maintained per `docs/workflow/exec-plans.md`. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be kept current as work proceeds.

## Purpose / Big Picture

The Bash command-safety judge makes exactly one bounded provider request before every non-empty Bash command. A timeout or a retryable transport failure (connection reset, broken pipe, stale connection) immediately blocks the command with a fail-closed tool error, even when the provider would have answered a second request within a few seconds. Evidence in issue #204: in one session two identical `gh pr merge` calls timed out after a full 30 seconds each before the third succeeded in 14.7 seconds; across ten sessions, 7 of 412 judge events timed out. Fail-closed behavior is correct, but one un-retried transient request is not reliable enough for a gate that runs before every Bash command.

After this change, a judge operation that hits a timeout or retryable transport/HTTP failure makes at most one recovery attempt within one documented total deadline. Valid verdicts, refusals, and malformed verdicts are never retried; an exhausted recovery still blocks the command before spawn. Operators can see every attempt, the retry reason, the wait, and the deadline in session telemetry, and `just judge-bench` measures whether recovery improves availability against the SLOs from #205.

The compatibility effect: `timeout_secs` keeps its documented meaning as the bound for one judge provider call. The complete judge operation gains a documented deadline of `timeout_secs + retry_budget_secs` (default 45 seconds with defaults of 30 + 15), which never reaches two full configured timeout periods. A retry only happens after a timeout or retryable transport/HTTP failure; verdicts, refusals, and malformed responses remain terminal, and every failure path still fails closed.

## Progress

- [x] (2026-08-13 21:15Z) Read issue #204, verified dependency #202 and the measurement harness #205 are closed, confirmed the board's Blocked status has no formal blocker, claimed #204 (Blocked -> In Progress), and created `feat/judge-retry-bounded-deadline` from `origin/master`.
- [x] (2026-08-13 21:30Z) Read the task and ExecPlan workflows, `docs/security.md` (fail-closed trust boundary), `src/clients/judge.rs`, `src/clients/judge_observer.rs`, `src/clients/retry.rs`, `src/clients/agent_runner.rs`, `src/clients/tools/bash.rs`, `src/cli/bash.rs`, `src/session_telemetry.rs`, `src/config/settings.rs`, `docs/configuration.md`, `docs/integrations.md`, and the completed `judge-attempt-diagnostics` and `judge-benchmark-slos` plans.
- [x] (2026-08-13 21:45Z) Chose the design recorded below: a documented `retry_budget_secs` setting, a two-attempt observed judge loop under one operation deadline, reuse of the agent runner's retry classification, fresh-client recovery on stale/timeout classes, and additive telemetry fields.
- [x] (2026-08-13 22:30Z) Milestone 1: added `retry_budget_secs` to the judge settings surface (partial + resolved + loader + defaults), carried it onto `JudgeClient::new`, updated both call sites and all test constructors, and documented the key in `docs/configuration.md`.
- [x] (2026-08-13 23:15Z) Milestone 2: rewrote `src/clients/judge_observer.rs` as a two-attempt driver under `timeout + retry_budget`, reusing `retry::classify_http_failure` / `classify_transport_error` with a judge `RetryPolicy`, swapping in a fresh client (reuse disabled) on stale/timeout classes, and keeping verdicts/refusals/malformed/response-parse terminal. Extracted `retry_classification` to hold the CC target.
- [x] (2026-08-13 23:30Z) Milestone 3: added `retry_reason`, `retry_delay_ms`, and `effective_deadline_ms` to `JudgeAttemptTelemetry`, made the Bash preflight report cumulative wall time, and added a retry-era metrics-parser test.
- [x] (2026-08-13 23:45Z) Milestone 4: added the acceptance-matrix tests (timeout/transport/HTTP retry, budget zero and exhaustion, verdict/refusal/malformed never retried, cancellation, telemetry shape, Bash-tool no-spawn on exhausted recovery, `cake bash check` parity) plus a settings resolution test.
- [x] (2026-08-14 00:10Z) Milestone 5: updated `docs/configuration.md`, `docs/integrations.md`, and `docs/security.md` (instruction-size budgets held by consolidating wording), then passed `just clippy-strict`, `just cc-check`, `just docs-check`, `just judge-bench-check`, the metrics suite, and `just ci`.
- [ ] Run the large-change preflight, apply its findings, and update this plan's Outcomes before opening the pull request.

## Surprises & Discoveries

- Observation: the judge's per-attempt timeout is currently the operation deadline. `ObservedJudgeCall::remaining(client.timeout)` counts down from the moment the observed call starts (request build, send, parse, verdict), so one `timeout_secs` bounds the entire operation today. Evidence: `src/clients/judge_observer.rs::ObservedJudgeCall::remaining`.
- Observation: the judge observer discards the `anyhow::Error` for transport failures; `send` converts it to a `JudgeError::Transport { detail }` string immediately. The agent runner's retry classification (`retry::classify_transport_error`, `retry::should_disable_connection_reuse`) needs the `anyhow::Error` chain, so the observer must retain it for classification. Evidence: `src/clients/judge_observer.rs::ObservedJudgeCall::send`; `src/clients/retry.rs::transport_retry_detail`.
- Observation: the agent runner already treats a request timeout as a stale-connection transport class: reqwest timeouts surface as `is_timeout()` errors, `transport_retry_detail` maps them to "stale connection timeout", and `should_disable_connection_reuse` returns true, so the runner rebuilds the client with `pool_max_idle_per_host(0)`. The judge should mirror that: a judge timeout also warrants fresh-client recovery. Evidence: `src/clients/retry.rs::transport_retry_detail`, `src/clients/agent_runner.rs` lines 207-230.
- Observation: `retry::classify_http_failure` and `retry::classify_transport_error` take a `session_id` for deterministic jitter and compare `attempt >= policy.max_retries`. To allow exactly one recovery, the judge policy must set `max_retries: 2` (attempt 1 may retry; attempt 2 may not). The judge has no session id; `Uuid::nil()` makes the small jitter deterministic and test-friendly. Evidence: `src/clients/retry.rs::classify_http_failure` (attempt guard), `fallback_delay`.
- Observation: `JudgeAttemptTelemetry` already carries `attempt`, `retry_ordinal` (always 0 today), and `configured_timeout_ms`, and `RetryReasonSnapshot` already exists for `retry_scheduled` records, so retry telemetry reuses the established vocabulary. Evidence: `src/session_telemetry.rs` lines 110-148, 301-320.
- Observation: the metrics loader (`scripts/session-metrics/cakelib.py`) appends whole `judge_attempt` records as dicts and reads known fields, so additive fields are tolerated without code change, but a parser test with a retry-era record should prove it. Evidence: `scripts/session-metrics/cakelib.py` lines 271-274.
- Observation: wiremock's `expect(n)` only sets the verification expectation range; the mock keeps matching until `up_to_n_times(n)` is set. Scripted one-shot stubs (delayed then allow) need `up_to_n_times(1)`, and matching order is mount order. Evidence: the first timeout-then-allow test served the allow stub to request 1; wiremock `mock.rs::expect` vs `up_to_n_times`.
- Observation: strict clippy (`clippy-strict`) denies `expect()` on `Result` in production, so the judge client's `Mutex<reqwest::Client>` uses `unwrap_or_else(PoisonError::into_inner)` to recover a poisoned lock rather than panicking. Evidence: `-D clippy::expect-used` in the strict gate.
- Observation: the instruction-size lint is enforced and `docs/security.md` sat exactly at its budget before this change, so the retry statement had to displace existing wording rather than add to it. Evidence: `scripts/lint-instruction-size.py` reported 1500/1500 before edits.

## Decision Log

- Decision: add a documented `[tools.bash.judge] retry_budget_secs` setting (default 15; `0` disables recovery entirely; values of 1 or more are the budget in seconds) rather than deriving the budget implicitly. Rationale: the acceptance criteria allow the settings contract to change when documented, a named budget is precisely testable, and `0` gives operators a clean latency/cost escape hatch. Date/Author: 2026-08-13 / cake.
- Decision: the complete judge operation deadline is `timeout_secs + retry_budget_secs`. Attempt 1 keeps the full configured `timeout_secs` per-call bound (unchanged semantics); one recovery attempt may consume at most `min(timeout_secs, remaining_after_wait)`. Rationale: giving attempt 1 less than `timeout_secs` would regress the common single-attempt case; giving the recovery a second full period would violate the acceptance criterion. With defaults the worst case is 45 seconds, below two full 30-second periods. Date/Author: 2026-08-13 / cake.
- Decision: reuse `retry::classify_http_failure` and `retry::classify_transport_error` with a judge-specific `RetryPolicy { max_retries: 2, base_delay: 500ms, max_backoff: min(retry_budget_secs, 5s), jitter_divisor: 5 }`, passing `Uuid::nil()` for jitter. A judge timeout synthesizes `RetryReason::RequestTimeout` directly (there is no `anyhow::Error` to classify). Rationale: one classification vocabulary for the agent loop and the judge keeps retry semantics consistent and reuses tested code. Date/Author: 2026-08-13 / cake.
- Decision: recovery rebuilds the HTTP client with connection reuse disabled (`build_http_client(true)`) when the retry reason is `Network` (stale transport) or `RequestTimeout` (a stalled request may leave a bad pooled connection), and keeps the same client for HTTP-status retries (429, 5xx, overloaded). The swapped client replaces the judge client's stored client so later commands do not keep paying for the stale pool, matching the agent runner's permanent swap. Date/Author: 2026-08-13 / cake.
- Decision: retry only error outcomes. Valid block/warn/allow verdicts, refusals, malformed verdicts, and response-parse failures are terminal; the recovery path is never entered with a verdict in hand, so a block can never be retried in search of an allow. Date/Author: 2026-08-13 / cake.
- Decision: record the bounded-recovery decision as ADR-020 (`docs/adr/020-bounded-llm-judge-recovery.md`), which partially supersedes ADR-018's "no retries in version 1" clause, with a reciprocal note in ADR-018's More Information. Rationale: the change reverses a documented architectural decision on a security boundary, and the ADR workflow requires a new ADR rather than an in-place edit. Date/Author: 2026-08-14 / cake.
- Decision: on exhausted recovery, surface the final (recovery) attempt's error and terminal class; if the recovery never starts because the remaining budget is consumed by the wait, surface attempt 1's error. The Bash tool blocks before spawn in both cases. Date/Author: 2026-08-13 / cake.
- Decision: add three additive fields to `JudgeAttemptTelemetry`: `retry_reason` (the `RetryReasonSnapshot` that triggered this attempt; absent on attempt 1), `retry_delay_ms` (the backoff wait before this attempt; 0 on attempt 1), and `effective_deadline_ms` (the operation deadline, same on every attempt of one evaluation). The Bash preflight reports cumulative wall time (sum of attempt `total_ms` plus `retry_delay_ms`) as the judge latency instead of the last attempt's `total_ms`. Rationale: satisfies the acceptance telemetry criterion without new record types or raw content. Date/Author: 2026-08-13 / cake.

### Security-impact analysis (fail-closed trust boundary)

`docs/security.md` requires enumerating bypass classes before editing a security boundary. The judge is the command-safety gate above the OS sandbox; retry logic lives inside the existing fail-closed path. The change must defend against:

1. A retry converting a valid `block`/`warn`/`allow` verdict into a different verdict. Defense: retries are keyed only on `JudgeError` outcomes; a verdict ends the loop.
2. A retry turning a refusal or malformed verdict into an allow. Defense: `Refusal` and `Malformed` are terminal classes, never classified for retry.
3. Exhausted recovery running the command ungated. Defense: the final `JudgeError` maps to the same `fail_closed_tool_error` path as today, before any spawn; the Bash tool asserts command-not-spawned in tests.
4. A retry masking a configuration or auth failure. Defense: HTTP `400`, `401`, `403`, `404`, and other non-retryable statuses stay terminal via `classify_http_failure`.
5. A retry widening authority or bypassing the allowlist/override. Defense: the recovery re-sends the identical `JudgeRequest` with the same settings, rubric, and allowlist; the override applies only to a real `block` verdict, unchanged.
6. A retry extending latency beyond the documented bound. Defense: the operation deadline `timeout_secs + retry_budget_secs` bounds every attempt and wait; the recovery allowance is `min(timeout_secs, remaining)`.
7. Telemetry leaking command or prompt text through retry fields. Defense: `retry_reason`, `retry_delay_ms`, and `effective_deadline_ms` carry no request content; the existing `sanitize_attempt_provider_fields` and redaction path still runs on every attempt.
8. Bypass (`CAKE_JUDGE=off` or `enabled = false`) making calls or retries. Defense: the bypass short-circuit precedes client construction and any attempt, unchanged.
9. Cancellation mid-retry spawning the command. Defense: cancellation drops the in-flight request and wait; the command only ever runs after a verdict, and the Bash tool aborts on cancellation as today.

## Outcomes & Retrospective

Not yet completed. This section will record what was achieved, verification results, and lessons learned before the plan moves to `docs/exec-plans/completed/`.

## Context and Orientation

Cake is a Rust binary. Every non-empty model-generated Bash command passes through the LLM judge before the Bash executor may spawn it. The judge is default-on and fail-closed: any judge error blocks the command. The judge path has two callers that must share semantics: the Bash tool preflight (`src/clients/tools/bash.rs::bash_judge_preflight`, via `evaluate_command_observed`) and the standalone `cake bash check` command (`src/cli/bash.rs`, via `evaluate_command`). Both funnel through `JudgeClient` in `src/clients/judge.rs` and the observed call in `src/clients/judge_observer.rs`, so retry logic in the observer gives both callers identical behavior.

`JudgeClient` holds a `reqwest::Client`, a `ResolvedModelConfig`, a per-call `timeout: Duration` (from `[tools.bash.judge] timeout_secs`, default 30), and an optional user rubric. `observer::judge_observed` runs one bounded provider call: it builds the judge history and request JSON, sends it through the configured backend (Chat Completions or Responses) with `tokio::time::timeout(self.remaining(client.timeout), ...)`, parses the response, and classifies the terminal outcome (`Verdict`, `Timeout`, `Transport`, `HttpError`, `ResponseParse`, `MalformedVerdict`, `Refusal`). It returns one `JudgeAttemptTelemetry` per call; `evaluate_command_observed` wraps it into a `JudgeEvaluation { outcome, attempts, diagnostic }`.

The agent loop's retry machinery lives in `src/clients/retry.rs` and `src/clients/agent_runner.rs`. `retry::classify_http_failure` decides retryability from status, headers (`Retry-After`, `x-should-retry`), and body (overloaded markers, context overflow). `retry::classify_transport_error` and `retry::should_disable_connection_reuse` decide retryability of `anyhow::Error` transport failures from stale-connection markers. Both return a `RetryDecision` carrying a `RetryStatus { attempt, max_retries, delay, reason, detail }`. `RetryReason` covers RateLimit, Overloaded, ServerError, RequestTimeout, LockTimeout, Network, ContextOverflow, and SemanticIncomplete. `RetryReasonSnapshot` in `src/session_telemetry.rs` is the serialized form. `agent_runner::build_http_client(disable_connection_reuse)` builds the reqwest client; `true` sets `pool_max_idle_per_host(0)`.

Judge settings resolve in `src/config/settings.rs`: `JudgeSettingsPartial` (serde input) merges into `JudgeSettings` (resolved), with `timeout_secs` defaulting to `DEFAULT_JUDGE_TIMEOUT_SECS` (30) and never below 1. `JudgeContext` (in `src/clients/judge.rs`) builds the run's `JudgeClient` lazily once and caches it; `src/cli/session_factory.rs::attach_judge` wires it onto the tool context. `src/cli/bash.rs::resolve_run_judge_client` builds a standalone client for `cake bash check`.

Telemetry: `JudgeAttemptTelemetry` (in `src/session_telemetry.rs`) is the metadata-only per-attempt record persisted as a `judge_attempt` sidecar line through `JudgeAttemptSink`. The Bash preflight records attempts as soon as judging completes and emits a `judge_verdict` or `judge_fail_closed` compensation event with a `latency_ms`. `cake bash check` renders the verdict or the fail-closed error with exit classification (timeout/network/auth/rate-limit errors exit 2; other transport, malformed, and refusal errors exit 1).

The `#205` benchmark (`src/clients/judge_benchmark_tests.rs`, `just judge-bench-check` / `just judge-bench`) drives the real `JudgeClient` over the committed corpus with a wiremock provider and already aggregates multi-attempt trials, so it will measure the retry's effect on latency and timeout rates once this change lands.

## Plan of Work

### Milestone 1: Settings and plumbing

Add `retry_budget_secs` to the judge settings surface and carry it onto `JudgeClient`. At the end of this milestone the setting resolves, `JudgeClient` holds the budget, both call sites pass it, and `docs/configuration.md` documents the new key. No behavior changes yet: the observer still makes one attempt, and all existing tests pass unchanged except constructor call sites.

Edit `src/config/settings.rs`: add `retry_budget_secs: Option<u64>` to `JudgeSettingsPartial`, `retry_budget_secs: u64` to `JudgeSettings` (default 15 via a new `DEFAULT_JUDGE_RETRY_BUDGET_SECS` const), and carry it through the loader merge where `timeout_secs` is handled. Values of 0 disable recovery; values of 1 or more are the budget in seconds; values below 1 other than 0 are raised to 1 (mirror `timeout_secs` normalization). Verify the unrecognized-settings-key test from #234 still passes (the new key must be a recognized key).

Change `JudgeClient::new` to take the retry budget: `JudgeClient::new(config: ResolvedModelConfig, timeout: Duration, retry_budget: Duration)`. Update `JudgeContext::judge_client` (`src/clients/judge.rs`, currently `Duration::from_secs(self.settings.timeout_secs)`), `resolve_run_judge_client` (`src/cli/bash.rs`), and the `judge_client` test helper in `src/clients/judge_tests.rs` plus any other construction sites (search `JudgeClient::new`).

### Milestone 2: Retry-aware judge observer

Make `observer::judge_observed` drive at most two attempts under one operation deadline, reusing the retry classification. At the end of this milestone the acceptance matrix behavior exists: timeout-then-allow, transport-then-allow, timeout-then-timeout, non-retryable HTTP failure, no retry on verdicts/refusals/malformed, fresh-client recovery, exhausted-recovery fail-closed, and a wall time inside the documented deadline.

Restructure `src/clients/judge_observer.rs`:

- Change the per-attempt `ObservedJudgeCall` so the request/parse allowance is a parameter (attempt 1: `client.timeout`; recovery: `min(client.timeout, remaining_after_wait)`), with each attempt's own `total_start`.
- `observer::judge_observed` becomes the retry driver: run attempt 1; if it failed with a retryable class and `attempts_made < 2`, compute the wait from the judge `RetryPolicy` and the remaining operation budget; if a wait of that size still leaves budget, sleep the wait, build the recovery attempt (fresh client when the reason is `Network` or `RequestTimeout`), and run it. Return the final result plus the `Vec` of attempts (1 or 2) and the last attempt's diagnostic.
- The operation deadline is `client.timeout + client.retry_budget`. `remaining_after_wait = deadline - elapsed_since_operation_start - wait`; the recovery runs only when that is positive.
- Retain the `anyhow::Error` from `ObservedJudgeCall::send` so `retry::classify_transport_error` and `retry::should_disable_connection_reuse` can classify it; retain HTTP status, headers, and body so `retry::classify_http_failure` can classify HTTP failures. Map the terminal classes to retry decisions: `Timeout` -> `RequestTimeout` (fresh client), `Transport` with a retryable classification -> `Network` (fresh client) or terminal, `HttpError` with a retryable status -> the classified reason (same client) or terminal, everything else terminal.
- Give `JudgeClient` interior mutability for the swap: store `client: std::sync::Mutex<reqwest::Client>` (clone is cheap; the observer clones for each send and replaces on recovery so later commands use the fresh pool).
- Keep `JudgeClient::judge` (used by `cake bash check`) and `judge_observed` (used by the Bash preflight) both going through the same observed driver, preserving the `JudgeError` taxonomy. `evaluate_command` and `evaluate_command_observed` keep their signatures; `JudgeEvaluation.attempts` now carries 1 or 2 attempts.
- Keep new functions at or below the cyclomatic-complexity target; `just cc-check` is the gate. If the driver exceeds the target, extract the classification decision (failure -> retry decision) and the budget/wait computation into small pure functions.

### Milestone 3: Retry telemetry and cumulative latency

Add the additive telemetry fields and make the Bash preflight report cumulative latency. At the end of this milestone a two-attempt evaluation records two `judge_attempt` lines where attempt 2 has `retry_ordinal: 1`, a `retry_reason`, a nonzero `retry_delay_ms` (when a wait occurred), and the same `effective_deadline_ms` as attempt 1; the compensation `latency_ms` reflects the whole operation.

Edit `src/session_telemetry.rs`: add `retry_reason: Option<RetryReasonSnapshot>` (serde-skip when `None`), `retry_delay_ms: u64`, and `effective_deadline_ms: u64` to `JudgeAttemptTelemetry`. Initialize them in `judge_observer.rs::initial_attempt` (`retry_reason: None`, `retry_delay_ms: 0`, `effective_deadline_ms: duration_ms(client.timeout + client.retry_budget)`) and set them on the recovery attempt before finishing it. Confirm the fields pass through `sanitize_attempt_provider_fields` untouched (they carry no provider text).

Edit `src/clients/tools/bash.rs::observed_evaluation_to_preflight` so `latency_ms` is `sum(attempt.total_ms) + sum(attempt.retry_delay_ms)` instead of `attempts.last().total_ms`. The `cake bash check` path already measures wall time with `started.elapsed()` and needs no change.

Add a metrics-parser test in `scripts/session-metrics/tests` proving a sidecar with a two-attempt retry-era `judge_attempt` record (including the new fields) loads with zero parse errors and the existing aggregates are unchanged.

### Milestone 4: Acceptance-matrix tests

Add focused stub-provider tests (wiremock, short timeouts) in `src/clients/judge_tests.rs` covering every acceptance scenario, and a Bash-tool-level test that an exhausted recovery never spawns the command. At the end of this milestone the acceptance matrix is green.

- timeout-then-allow: attempt 1 stalls past its allowance (wiremock delay), attempt 2 returns `allow`; assert two attempts, the allow verdict, `retry_reason = request_timeout`, and wall time inside `timeout + retry_budget + tolerance`.
- transport-error-then-allow: attempt 1 fails with a connection-reset-class transport error, attempt 2 returns `allow`; assert `retry_reason = network` and that recovery used a fresh client (the swap path).
- timeout-then-timeout: both attempts stall; assert the final `JudgeError::Timeout`, two attempts, and wall time inside the deadline plus tolerance.
- non-retryable HTTP failure: a `400` (and `401`/`403`/`404`) response; assert one attempt, no retry, `JudgeError::Transport { status: Some(400), .. }`.
- retryable HTTP failure then allow: a `429` with a small `Retry-After`, then `allow`; assert the retry honored the wait, capped by budget, and kept the same client.
- valid block without retry: a `block` verdict on attempt 1; assert one attempt and the block preserved (also `warn` and `allow`).
- refusal without retry and malformed without retry: assert one attempt each, terminal class `refusal` / `malformed_verdict`, no retry.
- retry budget zero: `retry_budget_secs = 0` with a timeout-then-allow script; assert one attempt and the timeout error (recovery disabled).
- retry budget exhaustion by wait: a `429` whose classified delay exceeds the remaining budget; assert no recovery attempt and the attempt-1 error.
- cancellation: abort the judge future mid-request (drop it after a short delay); assert no panic and no command spawn.
- command-not-spawned on exhausted recovery: a Bash tool test with a stub judge that times out twice; assert the `fail_closed` tool error with class `timeout` and that the command process never spawned (reuse the existing Bash judge test harness).
- stale reused connection then fresh success: assert that after a stale-transport failure, recovery issues the next request on a client built with `pool_max_idle_per_host(0)` (unit-test the swap decision and/or observe two wiremock requests with a fresh-client path).
- telemetry shape: assert attempt 2's serialized record has `retry_ordinal: 1`, `retry_reason`, `retry_delay_ms`, `effective_deadline_ms`, and that no command, reason, cwd, prompt, API key, or response body text appears in either record.
- `cake bash check` parity: drive `evaluate_with_client` with a stub judge that times out once then allows; assert exit 0, `allow` output, and a latency that reflects both attempts.

### Milestone 5: Documentation and full verification

Update the three documentation surfaces and run the repository gate. At the end of this milestone the retry budget and fail-closed semantics are documented and `just ci` passes.

- `docs/configuration.md`: add `retry_budget_secs` to the `[tools.bash.judge]` example and bullet list. State: default 15; `0` disables recovery; values of 1 or more are the budget; the complete operation is bounded by `timeout_secs + retry_budget_secs`; verdicts, refusals, and malformed verdicts are never retried; exhausted recovery fails closed.
- `docs/integrations.md`: update the `judge_attempt` record description with the three new fields and state that a `judge_attempt` is emitted per provider attempt (1 or 2 per evaluation). State the retry behavior and that exit classification is unchanged (a timeout after exhausted recovery still exits 2 from `cake bash check`).
- `docs/security.md`: extend the judge section to state that one bounded recovery attempt may follow a timeout or retryable transport/HTTP failure within the documented deadline, that semantic outcomes are never retried, and that exhausted recovery still blocks before spawn. This preserves the fail-closed guarantee; no trust boundary widens.
- Run `cargo fmt`, `just cc-check`, focused test suites, the `judge-bench-check` deterministic harness (`just judge-bench-check`), the metrics suite, and `just ci`. Then run the repository's large-change preflight and apply its findings.

## Concrete Steps

All commands run from `/Users/travisennis/Projects/cake-1`.

After Milestone 1, the focused suites must pass with no snapshot drift:

```
cargo test config::settings
cargo test clients::judge_tests
cargo test cli::bash_tests
```

After Milestones 2-3:

```
cargo test clients::judge_tests
cargo test clients::tools::bash_tests::test_judge
cargo test session_telemetry
python3 -m unittest discover -s scripts/session-metrics/tests
```

After Milestone 4, the new acceptance tests:

```
cargo test clients::judge_tests -- --nocapture
cargo test clients::tools::bash_tests
```

After Milestone 5, the gate:

```
cargo fmt --check
just cc-check
just judge-bench-check
just ci
```

Expected result: every command exits zero. `just ci` includes the primary local checks (unit tests, integration suites, coverage, complexity, and repository lints). If a platform-dependent or credentialed check cannot run locally, record the exact skipped command and reason in the issue and pull request handoff rather than weakening the deterministic stub coverage.

## Validation and Acceptance

A stub judge that times out once and then allows must produce two `judge_attempt` records for the same evaluation: attempt 1 terminal class `timeout` with `total_ms` near its allowance, attempt 2 `retry_ordinal: 1`, `retry_reason: request_timeout`, a `retry_delay_ms` matching the wait, both sharing `effective_deadline_ms = timeout + retry_budget`, and the `allow` verdict driving one `judge_verdict` compensation whose `latency_ms` is the cumulative wall time. Neither attempt record may contain the command, reason, cwd, prompt, API key, or response body.

A stub that never answers must fail closed: the Bash tool returns the `timeout` fail-closed error, the command process never spawns, and the total wall time stays inside `timeout + retry_budget` plus a small scheduling tolerance (assert with short test deadlines, e.g. 2s + 1s).

A stub returning a valid `block` on the first request must produce exactly one attempt and block the command; the retry path must never be entered with a verdict in hand.

`cake bash check -- <command>` and the Bash preflight must exhibit identical retry and deadline semantics because both call the same observed driver.

Running `just judge-bench-check` must still pass, and `just judge-bench` (live, credentialed, authorized spend) must measure the retry era: the report's per-trial attempt count becomes 1 or 2, timeout rates should drop, and latency percentiles should show the bounded retry cost.

## Idempotence and Recovery

All code, test, and documentation edits are safe to repeat. The runtime change is additive: a judge evaluation may make at most two provider calls instead of one, and telemetry appends one record per attempt. Session transcripts and prior sidecar lines are never rewritten. If a telemetry write fails, the existing best-effort writer disables telemetry without changing command execution semantics.

Use wiremock stub providers and short test deadlines for validation; do not run paid or credentialed provider calls for this issue. `just judge-bench` is the live measurement surface and is authorized separately. If a retry test hangs, the judge's per-attempt timeout bounds it; tests use short deadlines so a hang fails fast.

If a test observes two attempts where one was expected (or vice versa), first check the retry classification: only `Timeout`, retryable `Transport`, and retryable `HttpError` outcomes retry. If a snapshot or sidecar test changes unexpectedly, compare the old and new record shapes before updating the expectation; the three new fields are additive and must serialize without changing existing fields.

## Artifacts and Notes

The intended two-attempt telemetry record for a timeout-then-allow evaluation is conceptually:

```
{"type":"judge_attempt","attempt":1,"retry_ordinal":0,"retry_reason":null,"retry_delay_ms":0,"effective_deadline_ms":45000,"configured_timeout_ms":30000,"total_ms":30000,"terminal_class":"timeout",...}
{"type":"judge_attempt","attempt":2,"retry_ordinal":1,"retry_reason":"request_timeout","retry_delay_ms":512,"effective_deadline_ms":45000,"configured_timeout_ms":30000,"total_ms":1420,"status_code":200,"terminal_class":"verdict",...}
```

No example in this plan is a frozen field-level serialization contract; Rust types and focused tests remain the authority. The important properties are one attempt per provider call, per-attempt retry attribution, a shared effective deadline, cumulative latency on the compensation, and the absence of raw request content from default telemetry.

## Interfaces and Dependencies

`crate::config::settings::JudgeSettings` and `JudgeSettingsPartial` will carry `retry_budget_secs`, resolved with a new `DEFAULT_JUDGE_RETRY_BUDGET_SECS` constant.

`crate::clients::judge::JudgeClient::new` will take `(config, timeout, retry_budget)`; the client will store the budget and expose the stored `reqwest::Client` through interior mutability so recovery can swap in a fresh client.

`crate::clients::judge_observer` will retain its phase-observation role and gain the retry driver. `observer::judge_observed` returns the final result, the 1-or-2 attempt vector, and the last attempt's diagnostic; `JudgeClient::judge` and `judge_observed` and `evaluate_command` / `evaluate_command_observed` keep their signatures and callers.

`crate::clients::retry::{RetryPolicy, classify_http_failure, classify_transport_error, should_disable_connection_reuse, HttpFailure, RetryDecision, RetryStatus, RetryReason}` provide the classification, reused without modification. `crate::clients::agent_runner::build_http_client` provides the fresh-client construction.

`crate::session_telemetry::JudgeAttemptTelemetry` gains `retry_reason`, `retry_delay_ms`, and `effective_deadline_ms`; `RetryReasonSnapshot` is reused as-is.

`crate::clients::tools::bash` keeps its preflight signature; only the cumulative-latency computation changes inside `observed_evaluation_to_preflight`.

No new crate dependency is expected; `reqwest`, `tokio`, `serde`, and the existing telemetry and retry abstractions provide the required behavior.

Revision note (2026-08-13): created the initial self-contained plan after claiming issue #204 and inspecting the judge, retry, agent runner, Bash tool, CLI, telemetry, settings, security, and documentation surfaces.
