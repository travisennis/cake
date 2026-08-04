# Recover incomplete model turns inside the original invocation

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with [docs/workflow/exec-plans.md](../../workflow/exec-plans.md).

## Purpose / Big Picture

After this change, a successful provider response that contains reasoning or other partial output but no final assistant message will not immediately end the Cake invocation. Cake will make one bounded continuation request in the same session and task, asking the model to provide the missing final answer without repeating completed tool work. A user can observe one ordinary successful `task_complete` when recovery works, or one existing `cut_off` completion with an exact `cake --resume <UUID> "try again"` command when recovery is unavailable or exhausted.

The change preserves the global rule that a bare invocation starts a new session. Recovery is entirely inside `Agent::send`, so session selection, exit meanings, and the persisted format version do not change.

## Progress

- [x] (2026-07-29 20:50Z) Started task 322, confirmed completed dependency 321, and read the agent-loop, provider, retry, persistence, telemetry, hook, and integration-contract authorities.
- [x] (2026-07-29 20:50Z) Chose a one-turn semantic recovery design that persists partial output, appends explicit model-visible continuation context, disables tools during recovery, and defers terminal completion and Stop-hook execution.
- [x] (2026-07-29 21:04Z) Implemented bounded tool-free recovery, semantic retry telemetry and text progress, non-retryable termination handling, provider-native reasoning replay, and the exact resume diagnostic.
- [x] (2026-07-29 21:04Z) Added focused tests for success, exhaustion, content filtering/refusal, completed-tool preservation, persistence/stream ordering, telemetry, counters, both provider backends, text progress, and Stop-hook ordering.
- [x] (2026-07-30 01:48Z) Resolved the termination-precedence escalation by choosing and implementing global signal aggregation; contradictory top-level, output-item, reason, and refusal signals now follow one explicit severity order.
- [x] (2026-07-30 01:59Z) Finished required subagent review with an all clear, completed all three L/XL preflight passes, and passed `just ci` after recapturing the intentional change-risk baseline.
- [x] (2026-07-30 01:59Z) Filled task 322 acceptance evidence and prepared this completed plan for archival and managed task completion.

## Surprises & Discoveries

- Observation: Provider retries run inside `AgentRunner`, but a semantic incomplete response is a successful parsed API turn and therefore must be recovered one layer higher in `Agent::send`. Evidence: `src/clients/agent_runner.rs` returns `TurnResult` immediately for a successful parsed response, while `src/clients/agent/agent_loop.rs` currently creates `CutOffError` after it finds no assistant message.

- Observation: The two backends already have different safe treatment for reasoning history. Chat Completions holds reasoning until it can attach it to a real assistant message and drops an unpaired reasoning suffix; Responses serializes reasoning as its native structured input item. Evidence: `ChatMessageBuilder::remember_reasoning` and `finish` in `src/clients/chat_completions.rs` do not synthesize an assistant message, while `ResponsesApiInputItem::from` in `src/clients/responses.rs` maps `ConversationItem::Reasoning` directly to a reasoning input item.

- Observation: Stop and error hooks run only after `Agent::send` returns, so keeping recovery inside `send` naturally prevents an intermediate hook or `task_complete`. Evidence: `CodingAssistant::execute_agent_turn` calls `handle_agent_turn_result` only after awaiting `client.send`.

- Observation: Chat Completions parsing synthesizes an empty assistant message for a response with no content, tool calls, or reasoning, and the old final-message resolver treated that empty string as success. Evidence: Focused inspection of `parse_choices` in `src/clients/chat_completions.rs` and `resolve_assistant_message` in `src/clients/agent_state.rs`; the resolver now ignores empty assistant content and its regression test passes.

- Observation: Cake logging is file-only, so a tracing event alone does not satisfy user-visible text progress. Evidence: `src/logger.rs` explicitly configures no stderr output. `CliOutputSink` now attaches a progress callback only for text mode; JSON and stream-JSON continue suppressing prose progress.

- Observation: Three consecutive review rounds found contradictory provider-termination signals being resolved incorrectly by source-local fallback logic. Evidence: The final escalated case was top-level Responses `status: "failed"` combined with an output item `status: "incomplete"`; the former `effective_response_status` fallback selected the retryable item status before the terminal top-level status. Trav resolved the escalation by selecting global severity aggregation, and contradictory-signal tests now enforce that policy.

- Observation: The first full `just ci` run passed compilation, formatting, both strict Clippy modes, all 1,144 tests, and 92.45% total coverage, then failed only because the committed cargo-crap baseline did not yet include six intentional branches added by this task. Evidence: The detailed report identified `Agent::send`, `RetryReasonSnapshot::from`, `chat_termination`, `cut_off_error`, `ChatMessageBuilder::push_message`, and `CliOutputSink::attach_callbacks`; five were 100% covered, and the agent-loop branch set is exercised by the end-to-end recovery matrix.

## Decision Log

- Decision: Represent semantic recovery as one additional agent-loop turn rather than an HTTP retry of the same request. Rationale: The first response is valid provider output that must count toward usage, turn totals, persistence, and streaming. The recovery request needs new model-visible context, so repeating the same wire request would not meet the behavior. Date/Author: 2026-07-29 / Codex

- Decision: Append a user continuation message and disable tools for the recovery request. Rationale: A user message is portable model-visible context across both APIs. Disabling tools makes the request final-answer-only and prevents completed tool calls from being blindly executed again. Any indispensable additional tool work can still be requested by the user after the bounded recovery ends. Date/Author: 2026-07-29 / Codex

- Decision: Treat `content_filter` and provider-declared failed/refusal states as non-retryable; retry completed-without-message, token-limit, incomplete, unknown, and missing termination metadata once. Rationale: Content policy and explicit failure/refusal are not repaired by asking for the same answer again, while all other cases may be transient or represent token exhaustion. Date/Author: 2026-07-29 / Codex

- Decision: Keep partial reasoning in the canonical conversation and rely on each backend's native request translation rather than converting reasoning to an assistant message. Rationale: Persisted and streamed records must retain what the provider returned. Chat Completions cannot portably replay provider reasoning as ordinary assistant text; Responses has a native reasoning item specifically for this purpose. Date/Author: 2026-07-29 / Codex

- Decision: Reuse the existing retry telemetry record with a new `semantic_incomplete` reason and zero delay. Rationale: This preserves the telemetry schema shape while making the different retry semantics queryable. The request is a new counted turn, not another transport attempt within the prior turn. Date/Author: 2026-07-29 / Codex

- Decision: Route semantic retry progress through an agent observer callback attached only by text output mode. Rationale: Text users need immediate progress on stderr, while JSON stdout and stream-JSON must remain machine-readable and free of prose. The callback keeps output-format policy at the CLI boundary. Date/Author: 2026-07-29 / Codex

- Decision: Globally aggregate every Responses top-level status, output-item status, incomplete reason, and refusal signal using explicit severity precedence: content filtering; refusal/failure/cancellation; token limit; incomplete/in-progress/queued; completed; unknown. Rationale: Local fallback ordering produced three findings of the same class. Global aggregation ensures any terminal non-retryable signal dominates retryable or completed signals, including contradictory OpenAI-compatible provider payloads. Date/Author: 2026-07-29 / Trav and Codex

- Decision: Recapture `ci/cargo-crap-baseline.json` with the repository's `just change-risk-baseline` recipe. Rationale: The reported complexity changes are the intended bounded-recovery control flow and provider/output classifications, not uncovered accidental risk. The repository recipe explicitly owns baseline updates after intentional code and test changes; the regenerated baseline reports zero regressions against the same LCOV data. Date/Author: 2026-07-29 / Codex

## Outcomes & Retrospective

Task 322 is complete. Cake now recognizes a successful but semantically incomplete provider turn, preserves and emits that partial turn, records a zero-delay `semantic_incomplete` retry, appends one explicit final-answer continuation in the same session and task, and performs one tool-free recovery request. A recovered answer returns through the existing success path with cumulative counters; an exhausted recovery returns the existing cut-off outcome once with the exact resume command.

Both provider adapters preserve their native boundaries. Responses replays structured reasoning and globally aggregates contradictory termination signals with terminal failures taking precedence. Chat Completions drops reasoning that cannot be safely paired with an assistant message. Actual refusals and content-filter/failure signals do not retry, and completed tool work remains recorded without re-execution.

The durable integration contract and ADR 001 now describe the new bounded turn and observable record ordering. Final `just ci` passed 1,144 tests, 92.46% line coverage, zero CRAP regressions, both strict Clippy configurations, formatting, compilation, and repository policy lints. There are no known remaining task-level gaps. The main design lesson was to aggregate provider termination evidence under one explicit precedence policy rather than incrementally layering source-local fallbacks.

## Context and Orientation

Cake stores its provider-neutral conversation as `ConversationItem` values. `Agent::send` in `src/clients/agent/agent_loop.rs` pushes the user's prompt, calls a provider through `AgentRunner`, streams and persists every returned item, accumulates usage and turn counts, executes requested tools, and returns only when it can resolve a final assistant message. A "semantic incomplete response" is an HTTP-successful, parseable provider turn whose returned items contain no non-empty final assistant message and no executable function calls.

`src/clients/chat_completions.rs` and `src/clients/responses.rs` translate the shared history to their provider wire formats. Chat Completions can attach provider reasoning only to a real assistant message. Responses has a native reasoning input item. Neither backend should invent an assistant message from reasoning text.

`src/clients/retry.rs` defines retry reasons and `src/session_telemetry.rs` serializes retry records. `src/main.rs` owns final hook invocation and the single `task_complete` record after `Agent::send` returns. `docs/integrations.md` documents retry, stream ordering, persisted records, and hook semantics.

## Plan of Work

First, extend retry observability with a `SemanticIncomplete` retry reason and its `semantic_incomplete` telemetry serialization. Add an agent-level helper that records a zero-delay retry event for the next turn and logs visible progress without routing the event through the HTTP retry classifier.

Second, change `Agent::send` to track whether semantic recovery has been used and whether the current request is the recovery request. After each successful provider turn, continue to count usage, stream its records, and append it to history before deciding what to do. If there are no function calls and no final assistant message, inspect the provider-neutral termination classification. On the first retryable occurrence, append a fixed continuation user message, stream and persist it, mark recovery used, and run one more turn with tools disabled. On a non-retryable occurrence or a second incomplete result, return the existing `CutOffError`. Only the exhausted-recovery detail gains the exact explicit-resume command.

Third, generalize the current correction-mode request controls so both schema corrections and semantic recovery can disable tools. If an output schema is configured, the recovery request should also attach the native structured-output constraint while it remains enabled. Keep schema-correction accounting separate from semantic-recovery accounting.

Fourth, add provider-focused request tests. Chat Completions must not synthesize or replay trailing unpaired reasoning as assistant content when followed by the continuation prompt. Responses must preserve the same reasoning as a native reasoning item. Both requests must omit tools during semantic recovery.

Fifth, add end-to-end agent tests with sequenced mock responses. Cover successful recovery, exhausted recovery, content-filter/refusal non-retry behavior, cumulative usage and turn count, unchanged session/task identity, persistence and stream ordering, telemetry reason, and no repeated tool execution. Extend main hook tests only where needed to prove one Stop hook and one terminal completion occur after final success or exhaustion.

Finally, update `docs/integrations.md` to describe the bounded semantic recovery and added optional retry reason, run focused tests, obtain the required subagent review, perform preflight, and run `just ci`.

## Concrete Steps

Run commands from `/Users/travisennis/Projects/cake`.

During implementation, format and run focused tests:

```
cargo fmt --all
cargo test semantic_incomplete
cargo test cut_off
cargo test reasoning
cargo test handle_agent_turn_result
```

After review fixes, run the required full gate:

```
env GIT_CONFIG_GLOBAL=/dev/null just ci
```

All focused tests passed. The final full gate reported 1,144 tests, 92.46% line coverage, zero CRAP regressions, and no formatting, Clippy, import, dependency, or module-size failures.

## Validation and Acceptance

A mock provider returning reasoning-only output followed by a normal assistant message must receive exactly two provider turns. The persisted and streamed sequence must retain the first reasoning item and the synthetic continuation message, then contain one final success. Session ID and task ID must remain unchanged; usage and turn counts must include both responses.

If the second response is also reasoning-only or empty, Cake must return one `CutOffError`, emit one `task_complete/cut_off`, run the Stop hook only once after exhaustion, and include `cake --resume <UUID> "try again"` in the diagnostic.

A response classified as content-filtered, failed, or refused must not receive the semantic continuation request. Existing provider/CLI error and outcome meanings must otherwise remain unchanged.

Completed tool calls from turns before the incomplete response must remain in history with their outputs, while the semantic recovery request offers no tool definitions. Provider request tests must prove the Chat Completions and Responses reasoning shapes rather than depending on an invented assistant message.

## Idempotence and Recovery

All source and documentation edits are additive or local and can be rerun through formatting and tests. Session fixtures use temporary directories and mock HTTP servers, so they must not modify real user sessions. If the implementation fails partway through, retain the task and plan as in progress, update `Progress` with the exact stopping point, and resume from this file without rewriting existing session data.

## Artifacts and Notes

The compatibility surfaces intentionally changed are one extra provider request and counted turn, one synthetic user conversation record before that request, retry telemetry with a new reason, and increased final latency when recovery is attempted. Session format version, record names, required fields, task identity, exit semantics, and global session selection remain unchanged.

## Interfaces and Dependencies

No new crate dependency is required. `RetryReason` and `RetryReasonSnapshot` gain `SemanticIncomplete`. `Agent::send` gains local recovery state and uses an internal turn mode or equivalent booleans to request a final-answer-only turn. The public CLI and serialized session types do not gain fields. `RetryScheduledTelemetry` retains its existing record shape and serializes the new reason through the existing tagged enum behavior.

Plan revision note (2026-07-29): Created the initial self-contained plan after source inspection because task 322 crosses the agent loop, both provider adapters, persistence, telemetry, hooks, and integration contracts.

Plan revision note (2026-07-29 21:04Z): Recorded the implemented turn-mode design, the empty-assistant and file-only-logging discoveries, focused verification, and the remaining review/preflight/full-gate work.

Plan revision note (2026-07-29 21:16Z): Recorded the required third-round review escalation and stopped implementation pending an explicit provider-termination aggregation decision.

Plan revision note (2026-07-30 01:48Z): Recorded Trav's global-aggregation decision, replaced fallback status selection with explicit severity precedence, added contradictory-signal coverage, and resumed review and verification.

Plan revision note (2026-07-30 01:55Z): Recorded the first full-gate CRAP-only failure and the repository-directed baseline recapture before rerunning final validation.

Plan revision note (2026-07-30 01:59Z): Recorded final green verification, completed the retrospective, and prepared the plan for archival with task 322.
