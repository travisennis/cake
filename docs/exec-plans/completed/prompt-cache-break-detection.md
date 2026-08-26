# Detect prompt-cache breaks from usage telemetry

This ExecPlan is a living document, maintained per `docs/workflow/exec-plans.md`.

## Purpose / Big Picture

Cake currently preserves only cache-read token counts and therefore cannot tell a user or a session analyst that a large prompt was billed again after a prompt-cache break. After this work, both OpenAI-compatible response parsers will preserve cache-write counts, and the session-metrics report will scan telemetry sidecars to identify economically meaningful cache misses. The report will show missed tokens, miss count, model-switch and idle-time evidence, and a clear statement when dollar pricing is unavailable. It will never invent cost from token counts because the repository has no provider/model price source.

The implementation will remain additive. It will not add cache controls to provider requests, change session replay or resume behavior, add cache analysis to stream-json, or write analysis records into the append-only session transcript. The first request in a scan segment and providers that never report cache activity are ignored. A resumed invocation remains part of the same chronological session scan so an idle-time miss can be identified.

## Progress

- [x] (2026-08-26) Inspect issue #182, existing usage parsers, telemetry records, session-metrics loader, and prior cache-break algorithm research.
- [x] (2026-08-26) Claim issue #182 and create branch `feat/prompt-cache-break-detection`.
- [x] (2026-08-26) Add cache-write fields to provider DTOs and normalized usage, with compatibility tests.
- [x] (2026-08-26) Add sidecar cache-break scanner and focused Python tests.
- [x] (2026-08-26) Integrate the scanner into the combined and standalone session-metrics reports; update operator documentation.
- [x] (2026-08-26) Run formatting, focused tests, the required final gate, and review the diff.
- [ ] Post verification and acceptance notes on issue #182 and prepare the pull request.

## Surprises & Discoveries

- The current `Usage.input_tokens` is the provider's total prompt-token count. It is used for context-window accounting, so this change must not redefine it as fresh uncached input. Cache-write and cache-read details remain separate, and the scanner derives fresh input when needed.
- `ApiAttemptTelemetry` has no model field, but each attempt belongs to an invocation with a `telemetry_init` record that has the resolved model. The scanner can join attempts to their invocation and then order completed turns across invocations in one session.
- No compaction record or compaction implementation exists. The scanner will not guess at compaction. It will preserve cache state across invocation boundaries, including continue/resume, because the sidecar timestamps are the only evidence for an idle-TTL break today.
- No pricing table, provider cost field, or cost configuration exists. Dollar waste will be represented as unavailable rather than as a false estimate. Missed-token volume and miss count remain available.

## Decision Log

- Decision: Implement historical detection in `scripts/session-metrics`, not in the agent loop. Rationale: issue #182 names telemetry sidecars as the source; the existing metrics suite already loads and groups sidecar records, and this avoids changing user-facing output or session record semantics. Date/Author: 2026-08-26 / cake.
- Decision: Scan terminal usage-bearing attempts once per `(session_id, invocation_id, turn_index)` and order those turns by telemetry timestamp. Rationale: retries create multiple `api_attempt` records for one turn, while invocation-local turn indexes restart on continue/resume. Date/Author: 2026-08-26 / cake.
- Decision: Keep the previous turn across invocation boundaries. Rationale: a resume after the provider TTL is precisely an observable idle cache break; dropping the previous turn would hide it. Date/Author: 2026-08-26 / cake.
- Decision: Use a 1,024-token noise floor and a five-minute idle threshold for labels. Rationale: these are the thresholds in the prior algorithm research and the issue's Anthropic-default guidance. Date/Author: 2026-08-26 / cake.
- Decision: Do not expose a dollar estimate until a provider/model price source exists. Rationale: cache-read and uncached token prices vary by provider and are absent from telemetry; reporting a number would be misleading. Date/Author: 2026-08-26 / cake.

## Outcomes & Retrospective

The implementation is complete and verified. Chat Completions and Responses parsing now preserve optional `cache_write_tokens` in normalized usage, aggregate usage and additive session records retain the field, and `scripts/session-metrics/cache_breaks.py` scans telemetry turns with retry deduplication, cache-reporting latching, a 1,024-token noise floor, model-switch and five-minute idle labels, and cross-invocation ordering. The combined report includes the new section and token totals include cache writes. `cargo test --all-features`, the 53-test session-metrics suite, `just docs-check`, formatting, whitespace checks, and a clean `just ci` all passed. Dollar waste remains unavailable because no provider pricing source exists; compaction markers also remain future work because cake has no compaction implementation.

## Context and Orientation

Cake is a Rust binary with a Python, standard-library-only session-metrics suite. OpenAI-compatible Chat Completions responses are parsed in `src/clients/chat_completions.rs` from DTOs in `src/clients/chat_types.rs`. Responses API responses are parsed in `src/clients/responses.rs` from DTOs in `src/clients/responses_types.rs`. Both map provider usage to `src/types/usage.rs::Usage`, which is serialized in session and telemetry records.

The telemetry sidecar is an append-only NDJSON file at the configured session-telemetry directory. Every provider attempt is an `api_attempt` record with `session_id`, `invocation_id`, `timestamp`, `turn_index`, `attempt`, and optional normalized `usage`. Retries for one turn have the same `turn_index`; a continue/resume invocation has a new `invocation_id` and starts its local turn index again. `telemetry_init` records the invocation's model and working directory. `scripts/session-metrics/cakelib.py` loads these records into `Invocation` objects, and `scripts/session-metrics/tokens.py` is the existing token report.

A cache read is a token reported in `input_tokens_details.cached_tokens`. A cache write is a token reported in the new `input_tokens_details.cache_write_tokens`. The provider's total prompt count remains `Usage.input_tokens`; fresh input is `input_tokens - cached_tokens - cache_write_tokens`, saturating at zero. A cache break is inferred only when a current prompt is materially larger than the cache-read count compared with the previous request, and only after some turn in the session reported cache activity.

## Plan of Work

First extend the two response DTOs and the shared `InputTokensDetails` with `cache_write_tokens`. Map absent provider fields to zero, deserialize old session and sidecar records with the default, and add parser tests for nonzero and missing values. Update Rust test fixtures and snapshots only as required by the new additive serialized field; do not alter request wire formats.

Next add `scripts/session-metrics/cache_breaks.py`. It will expose small pure helpers so tests can provide synthetic telemetry without files. The scanner will join each invocation's model to its usage-bearing API attempts, discard non-terminal retry duplicates by taking the highest attempt with usage for each invocation-local turn, sort completed turns by timestamp within each session, and compare adjacent turns. It will carry a `reported_cache` latch, skip zero-prompt turns, use `min(previous_prompt, current_prompt) - current_cached` with saturation, ignore misses at or below 1,024 tokens, and label a counted miss as `model switch`, `idle >= 5m`, or `generic`. Its result will include missed-token count and miss count, plus optional detail rows. Dollar cost will be `None` because no valid price source exists.

Finally call the scanner from `tokens.py` or a dedicated report section, preferring a dedicated `cache_breaks.py` section so ordinary token totals stay stable and the feature has a clear name. Add the module to `report.py`, document it in `scripts/session-metrics/README.md`, and test the report helpers for cache writes, unreported providers, retries, model switches, idle gaps, small noise, and cross-invocation ordering. Update `docs/integrations.md` only to describe the additive cache-write usage detail and the fact that telemetry-sidecar analysis is diagnostic; do not change session format or output contracts.

## Concrete Steps

All commands run from `/Users/travisennis/Projects/cake-1`.

1. Edit the Rust usage DTOs and mappings, then run `cargo fmt -- --check` and focused parser/type tests. Expected result: parser tests pass and a response containing `cache_write_tokens` produces the same value in normalized `Usage`.
2. Add the pure Python scanner and tests under `scripts/session-metrics/tests/`. Run `python3 -m unittest discover -s scripts/session-metrics/tests -v`. Expected result: all metrics tests pass, including the new cache-break cases.
3. Integrate the report section and documentation. Run `python3 scripts/session-metrics/report.py --help` and `just session-metrics-check`. Expected result: the report accepts the existing flags and the suite passes without requiring credentials or network access.
4. Run `cargo fmt`, focused Rust tests, and `just ci`. Review `git diff --check`, `git status --short`, and the final diff. If the full gate cannot run, record the exact failing prerequisite and narrower checks instead of treating it as unrelated.
5. Update this plan's Outcomes & Retrospective, post issue acceptance notes with exact verification, and open the pull request only after the plan and documentation assessment are complete.

## Validation and Acceptance

A synthetic sidecar sequence with a first 100,000-token request that writes 100,000 tokens, a second request reading 99,500 and writing 500, and a third request reading zero must report one miss of 100,000 tokens. A sequence with no cache-read or cache-write activity must report zero misses. A retry pair for one turn must count once. A model change must label the miss as a model switch. A five-minute-or-longer gap without a model change must label it as an idle-TTL miss. A gap below five minutes must use the generic label. A difference of 1,024 tokens or less must be ignored. The first usage-bearing turn and zero-prompt turns must not produce a miss.

The combined report must include a cache-break section when telemetry has usage and must state that dollar waste is unavailable when no pricing source exists. Existing token totals, transcript cross-checks, invocation filtering, and report command-line options must continue to work. Old usage JSON without `cache_write_tokens` must still deserialize, and new nonzero cache-write values must survive both Chat Completions and Responses parsing.

## Idempotence and Recovery

The Rust and Python changes are ordinary source edits and can be rerun safely. Formatting and tests do not mutate runtime session data. Snapshot updates, if needed, must be limited to snapshots affected by the additive usage field. The metrics scanner is read-only and tolerates absent directories, malformed records, unknown record types, missing timestamps, missing usage, and old sidecars. If an integration test fails because a snapshot is stale, regenerate only the named snapshot and inspect its diff; do not accept unrelated snapshot changes. If `just ci` fails for an existing environment prerequisite, retain the focused evidence and report the exact blocker.

## Artifacts and Notes

The durable artifacts are the additive Rust usage field, the cache-break metrics module and tests, the report integration, and the updated operator documentation. The primary proof is the focused synthetic scanner suite plus the existing Rust parser and full repository gate. The sidecar remains append-only and no cache analysis data is appended to it.

## Interfaces and Dependencies

`crate::types::usage::InputTokensDetails` gains `cache_write_tokens: u64`, defaulting to zero during deserialization. `crate::clients::chat_types::PromptTokensDetails` and `crate::clients::responses_types::ApiInputTokensDetails` gain optional provider fields, mapped by `crate::clients::chat_completions::parse_response` and `crate::clients::responses::map_usage`. `scripts/session-metrics/cache_breaks.py` owns the pure scan result and formatting helpers; it depends only on the existing `cakelib.Invocation` and standard-library types. `scripts/session-metrics/report.py` imports the new section, while `scripts/session-metrics/tokens.py` uses the new cache-write detail in its existing token totals. No new crate or Python dependency is needed.

Revision note: The plan was revised on 2026-08-26 after implementation to record the selected session-metrics boundary, compatibility behavior, validation results, and the explicit no-pricing limitation.
