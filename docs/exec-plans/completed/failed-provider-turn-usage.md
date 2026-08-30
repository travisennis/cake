# Persist usage before provider-turn retry and discard decisions

This ExecPlan is a living document, maintained per `docs/workflow/exec-plans.md`.

## Purpose / Big Picture

Cake currently persists `turn_usage` only after the agent receives a completed provider turn. A retryable response, a terminal provider failure, or a response discarded because it cannot be decoded can still report billable tokens, but those tokens disappear from the session ledger and from the task totals. After this change, every provider attempt that reports usage settles that usage into the session record and in-memory totals before Cake classifies the attempt, retries it, or discards it. A failed run therefore keeps an auditable usage record without changing live stream-json output.

The observable proof is a provider fixture that reports usage on a failed Responses API attempt: the session contains a `turn_usage` record with the attempt ordinal and terminal class, and the final task usage includes those tokens. A retry fixture produces one usage record per billed attempt. Existing successful `turn_usage` and stream-json snapshots remain compatible.

## Progress

- [x] (2026-08-30) Inspect issue #346, architecture, integration contracts, current agent loop, provider parsers, session records, telemetry, tests, and complexity rules.
- [x] (2026-08-30) Decide that this cross-cutting session/provider change requires an ExecPlan.
- [x] (2026-08-30) Add a shared terminal-class vocabulary and optional attempt metadata to `TurnUsageData`.
- [x] (2026-08-30) Preserve provider-reported usage through failed and discarded response parsing paths.
- [x] (2026-08-30) Settle each reported provider-attempt usage before retry or discard classification; include it in totals.
- [x] (2026-08-30) Add focused failure, retry, totals, parser, serialization, and unchanged stream-json tests.
- [x] (2026-08-30) Preserve usage from terminal `response.incomplete` streams and keep provider-attempt ordinals across native-constraint fallback requests.
- [x] (2026-08-30) Update snapshots, `docs/integrations.md`, and add ADR 025 for the durable record decision.
- [x] (2026-08-30) Run formatting, focused tests, repository gates, preflight, and inspect the final diff. `just check` passed after the final fixes; earlier partial runs hit unrelated macOS outer-sandbox failures.
- [x] (2026-08-30) Record issue acceptance notes, archive this plan, commit, push, and open a PR that closes #346.

## Surprises & Discoveries

- `AgentRunner` already records provider-attempt usage in the telemetry sidecar only when parsing succeeds. Parse errors such as `response.failed` currently lose usage before the agent loop can persist it.
- `response.incomplete` is a terminal Responses SSE event that can carry usage even when no `response.completed` event follows. The parser now retains that usage for the typed body-parse error, while preserving the existing completion requirement and semantic recovery behavior.
- Native structured-output fallback is a second request within the same logical turn, not a new turn. The shared next-attempt counter now gives its telemetry and session records distinct ordinals.
- `just check` passed the full local test and lint gate after the final fixes: 1,381 tests passed and 2 ignored, plus all integration targets. Earlier partial runs hit four macOS outer-sandbox failures, but the final routed gate passed without source workarounds.

## Decision Log

- Decision: Extend the existing session-only `turn_usage` record instead of adding a sibling record kind. Record one entry for each provider attempt that reports usage, including attempts later retried or discarded. Rationale: the existing record is already the session usage ledger, one record per provider request matches the provider billing boundary, and a new record kind would add another consumer branch without improving accounting. Date/Author: 2026-08-30 / Cake.
- Decision: Add optional `attempt` and `terminal_class` fields to `TurnUsageData`. Successful first attempts omit both fields to preserve the established serialization shape; retries and failed/discarded attempts include the 1-based attempt ordinal and bounded provider-attempt class. Rationale: fields are additive, stream-json is unaffected, and the existing telemetry vocabulary gives consumers a stable classification. Date/Author: 2026-08-30 / Cake.
- Decision: Include the native output-schema fallback in the same provider-attempt sequence as the rejected constrained request. Rationale: both requests serve one logical turn, so session and telemetry ordinals must remain unique and ordered. Date/Author: 2026-08-30 / Cake.
- Decision: Preserve usage from `response.incomplete` in the typed stream parse error without changing the existing requirement for `response.completed`. Rationale: the provider can report usage on a terminal incomplete event, and the issue requires retaining any usage Cake observes. Date/Author: 2026-08-30 / Cake.

## Outcomes & Retrospective

Implementation is complete. Cake now settles one session-only `turn_usage` record for every provider attempt that reports normalized usage, before retry or discard classification. Failed Responses `response.failed` and `response.incomplete` events, malformed but usage-bearing response bodies, and usage-bearing HTTP error bodies retain their normalized usage. Retry attempts contribute to `task_complete.usage`, telemetry `api_attempt.usage`, and the session ledger. Optional `attempt` and `terminal_class` fields identify retry and failure records, while first-attempt successful records and live stream-json retain their prior shape.

Focused regression tests cover terminal response failures, retryable response failures, discarded context-overflow responses, native-constraint fallback attempt numbering, incomplete streaming usage, Chat Completions and Responses parse failures, serialization, legacy deserialization, aggregate totals, telemetry agreement, and absence of `turn_usage` from stream-json. `cargo fmt --all -- --check`, strict Clippy in both feature modes, `just cc-check`, `just lint-imports`, `just lint-deps`, `just lint-module-size`, and `just docs-check` pass. The final `just check` passes its full local test and lint gate: 1,381 tests passed and 2 ignored, plus all integration targets.

## Preflight Review

### How did we do?

The initial preflight review found two correctness gaps: the native output-schema fallback reset provider-attempt ordinals, and usage on terminal `response.incomplete` SSE events was discarded. Both were fixed with focused tests. A follow-up review of the updated diff confirmed that usage extraction remains provider-boundary-local, settlement occurs before retry classification, and the new stream error wrapper preserves typed `response.failed` retry handling. Final `just check` passed; the earlier sandbox failures did not reproduce in the final routed run.

### Feedback to keep

Keep the session-only record boundary, optional metadata, provider-neutral terminal classes, typed usage carriers, and one record per provider attempt. Keep the explicit regression checks that compare session records, task totals, telemetry, and stream output.

### Feedback to ignore

No unrelated source change is justified. Earlier partial runs failed because the restricted environment denied temporary-directory creation for four Bash path tests; the final routed gate passed without weakening sandbox behavior.

### Plan of attack

The source and test work is complete. Update the issue acceptance notes, move this plan to `docs/exec-plans/completed/`, commit the focused files, push the branch, and open the issue-linked PR.

### Preflight compliance

- Root `AGENTS.md`: read.
- Nested `AGENTS.md`: none under changed paths.
- Task context: issue #346, inspected and claimed.
- ExecPlan: this plan, required because the change crosses provider parsing, agent-loop accounting, persistence, telemetry, tests, and docs.
- Durable docs: `ARCHITECTURE.md`, `docs/integrations.md`, `CONTRIBUTING.md`, `docs/workflow/tasks.md`, `docs/workflow/exec-plans.md`, and `docs/guardrails/complexity-targets.md` read; `docs/integrations.md` updated.
- ADRs: `docs/adr/README.md`, ADR 001, and ADR 004 read; ADR 025 added for the new durable ledger decision.
- Documentation impact: integration semantics, ADR, and ExecPlan updated; `just docs-check` passes.
- Changed files and diff: reviewed with `git diff --stat`, targeted diffs, and `git diff --check`.
- Validation: focused affected tests pass; strict Clippy, both feature modes, formatting, complexity, dependency/import, module-size, docs checks, and the final `just check` gate all pass.

The agent loop lives in `src/clients/agent/agent_loop.rs`. It sends each logical turn through `AgentRunner::complete_turn`, then counts the completed turn, processes conversation items, and logs context budget. The runner in `src/clients/agent_runner.rs` performs provider requests, emits `ApiAttemptTelemetry`, settles recognized usage, and classifies retryable versus terminal outcomes. Usage settlement now runs before the classification callback, so failed attempts reach session persistence without changing the runner's user-facing error result.

`TurnResult` is defined in `src/clients/agent.rs` and contains conversation items, optional normalized usage, optional provider termination, and an optional provider request id. `Usage` is the backend-neutral token structure in `src/types/usage.rs`. `TurnUsageData` and `SessionRecord` are in `src/types/session.rs`; session records are append-only JSONL, and `TurnUsage` is intentionally not represented in `StreamRecord`.

The Responses parser is in `src/clients/responses.rs`, with provider DTOs in `src/clients/responses_types.rs`. The Chat Completions parser is in `src/clients/chat_completions.rs`, with DTOs in `src/clients/chat_types.rs`. `ResponseDecodeError` and the usage-preserving response-parse wrapper are in `src/clients/backend.rs`. The Responses `response.failed` error is `ResponsesStreamFailed`; it stores bounded error metadata plus optional normalized usage internally.

The public compatibility rules are in `docs/integrations.md`: session files are versioned append-only records, consumers must ignore unknown optional stream fields, and `turn_usage` describes normalized usage for provider attempts. This change updates that explanation while keeping session format version 4 and live stream-json shapes unchanged. The durable choice to reuse `turn_usage` is recorded in `docs/adr/025-provider-usage-settlement.md`.

Serialization tests and snapshots are in `src/types/session_tests.rs` and `src/types/snapshots/`. Agent loop fixtures and end-to-end provider tests are in `src/clients/agent/agent_tests.rs`; parser tests are in `src/clients/responses_response_parsing_tests.rs`, `src/clients/responses_tests.rs`, and `src/clients/chat_completions_tests.rs`; process-level telemetry and session tests are in `tests/session_telemetry.rs`.

## Plan of Work

First, move or share the existing bounded `ApiAttemptTerminalClass` vocabulary with the domain session types and add it as a deserializable optional field on `TurnUsageData`, together with an optional 1-based attempt ordinal. Keep the established fields and format version unchanged. Update the canonical `turn_usage` serialization test to cover the new metadata and retain a test proving the legacy successful shape omits it.

Next, preserve normalized usage when a provider response does not become a successful `TurnResult`. The typed Responses failure metadata will carry optional usage internally and extract it from the failed response event. Response-body decode errors and semantic output-parse errors will carry optional usage extracted from a valid usage block. Both backends will expose a bounded usage extractor for HTTP error bodies. No raw response body or sensitive request data enters a session usage record or telemetry metadata.

Then, extend `AgentRunner::complete_turn` with a usage-settlement callback. After an attempt's provider response is parsed or its failure body is read, report the recognized usage and terminal class through that callback before calling retry classification. The existing API-attempt telemetry will use the same recognized usage. The callback fires once per provider attempt, including successful attempts and attempts that are later retried; attempts with no usage produce no session usage record.

Finally, make the agent callback update total usage, last reported usage, and the append-only `turn_usage` record. Completed-turn processing will only increment the logical turn counter, stream and append conversation items, and log context budget. Add regression coverage for a terminal `response.failed` with usage, a retry series with usage on multiple attempts, a malformed/discarded response with usage, aggregate totals, serialization/deserialization, and a live stream-json assertion that only existing task/conversation records are emitted.

## Concrete Steps

All commands run from `/Users/travisennis/Projects/cake/cake-2`.

1. Edit the domain/session and backend parser types described above. Run `cargo fmt --all` and the focused unit tests while iterating.

2. Run the targeted Rust tests:

   ```
   cargo test types::session
   cargo test clients::agent
   cargo test clients::responses
   cargo test clients::chat_completions
   cargo test --test session_telemetry
   ```

   Expected result: all targeted tests pass, including new failed-attempt usage records and unchanged stream-json assertions.

3. Update snapshots with `just snapshots` if the snapshot runner reports an intentional change, then inspect every changed snapshot. Do not accept unrelated snapshot churn.

4. Run the routed final checks:

   ```
   cargo fmt --all -- --check
   just cc-check
   just check
   ```

   `just check` is required because the change touches Rust, tests, and snapshots. Use `just check-full` only if the repository state or a gate requires the complete local suite.

5. Review with `git diff --check`, `git diff --stat`, and `git diff`, then run the preflight review procedure for the completed change. Fix only findings within issue #346.

6. Update the issue acceptance notes with the exact checks and documentation assessment. Move this plan to `docs/exec-plans/completed/` and fill in Outcomes & Retrospective before opening the PR.

7. Commit only the issue files with a valid Conventional Commit such as `fix(session): record failed provider usage (#346)`, push the feature branch, and open the PR with `Closes #346` using `just pr` when authentication and repository permissions allow it.

## Validation and Acceptance

A terminal provider attempt that reports normalized usage must append one `turn_usage` session record even when the logical agent turn returns an error. That record has the logical turn index, the usage values, `attempt`, and `terminal_class`; it does not add a live stream-json event. If a retryable attempt reports usage and a later attempt also reports usage, both records exist and task completion totals equal the sum of both attempts and any other completed turns.

A successful first attempt still serializes with the existing `turn_usage` shape when optional metadata is absent. New optional fields deserialize when present and old records deserialize when absent. Existing `task_complete` and conversation stream-json records retain their names, fields, and order; failed usage remains session-only. The telemetry `api_attempt.usage` value agrees with the normalized usage settled into the session ledger.

A provider response that reports no usage produces no `turn_usage` record, including a transport failure. This is intentional: Cake cannot invent token counts that the provider did not report. Raw provider response bodies, prompts, credentials, and tool output remain absent from the new audit data.

## Idempotence and Recovery

`cargo fmt`, focused tests, snapshot checks, and `just check` are safe to repeat. Snapshot updates must be reviewed and can be reverted by restoring only the affected snapshot if the test exposed unrelated churn. Git staging must name each changed path; never stage the whole working tree. If push or PR creation fails, keep the commit and report the exact authentication or network error; do not force-push or change remote history.

The session format remains version 4. New optional fields are ignored by older consumers that already tolerate unknown fields, and missing fields default when newer code reads old records. Usage records are append-only. A process crash after a usage callback but before a retry can leave a settled usage record without a later result, which is the intended durable accounting behavior; a process crash before the provider reports usage remains outside the observable accounting boundary.

## Artifacts and Notes

The final plan records the focused test output, final gate result, snapshot name, issue acceptance notes, commit hash, and pull request URL after implementation.

## Interfaces and Dependencies

The implementation uses these stable repository interfaces:

- `crate::types::Usage` remains the normalized token vocabulary.
- `crate::types::ApiAttemptTerminalClass` is the bounded provider-attempt terminal vocabulary shared by telemetry and session usage audit records.
- `crate::types::TurnUsageData` keeps its existing required fields and adds optional `attempt` and `terminal_class` fields.
- `crate::clients::agent_runner::AgentRunner::complete_turn` gains a next-attempt counter and a usage-settlement callback. It still returns `anyhow::Result<TurnResult>` and does not change user-facing errors.
- `crate::clients::agent::record_turn_usage` (or its field-splitting equivalent) updates `Agent::total_usage`, `Agent::last_usage`, and the session-only persistence callback.
- `crate::clients::backend::ResponseDecodeError` and the response-parse error wrapper carry only optional normalized usage needed by the runner; they do not expose raw provider bodies.

No new dependency or session-format version is required.
