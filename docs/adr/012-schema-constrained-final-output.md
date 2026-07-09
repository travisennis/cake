---
status: accepted
date: 2026-07-08
---
# Schema-Constrained Final Output

## Context and Problem Statement

ahm is adding delegation commands (`ahm task groom`, `ahm audit`) that hand a procedure prompt to a coding agent and mechanically apply the structured result. cake is ahm's default work agent, so its final response must be a JSON document that is machine-verifiably valid against a caller-supplied JSON Schema. Prompt-only JSON instructions are not reliable enough for this contract.

The constraint applies only to the final response — the `result` of the `task_complete` record. Intermediate reasoning, tool use, and multi-turn agent behavior must remain unchanged. The hard requirement is that a caller must never receive a successful `task_complete.result` that silently contains non-conforming prose.

Two facts shape the solution space:

- Neither backend (`src/clients/responses.rs`, `src/clients/chat_completions.rs`) currently sends structured-output parameters, and provider support varies: OpenAI-compatible servers behind `base_url` (OpenRouter, local servers, Zen) may or may not honor `response_format`/`text.format`, and strict modes enforce only a subset of JSON Schema.
- cake has no JSON Schema validator in its dependency tree, so local validation means a new runtime dependency in a binary whose size is actively audited.

## Decision Drivers

- The success contract must be a hard guarantee, independent of which provider is configured.
- Intermediate turns must not be constrained: attaching `response_format` to every request would force assistant preamble text during tool-use turns into JSON, changing agent behavior.
- Stream-json, session-file, and exit-code compatibility surfaces must only change additively, and failure signaling must be machine-distinguishable from success.
- Binary size and dependency count are actively managed; new dependencies need the smallest feature set.

## Considered Options

- Local validation with a bounded retry loop, plus native structured output attached only to retry (finalizer) turns.
- Backend-native structured output only, trusting the provider's strict mode, with no local validator.
- Local validation only, with prompt-guided retries and no native structured-output request shaping.

## Decision Outcome

Chosen option: local validation plus native constraint on finalizer turns, because it is the only option that guarantees the contract on every provider while leaving intermediate turns untouched and still using native strict modes where they help.

The concrete behavior:

- A new `--output-schema <path>` flag applies to any cake run (cake is a one-shot non-interactive CLI) and composes with every `--output-format` value and with `--continue`, `--resume`, and `--fork`.
- Before the run starts, cake reads, parses, and compiles the schema. The supported dialect is JSON Schema draft 2020-12 as implemented by the `jsonschema` crate, with remote/file `$ref` resolution disabled — schemas must be self-contained. Unreadable or invalid schema files fail with exit code 3 (input error) before any `task_start` is emitted.
- The agent loop runs unchanged. The schema requirement is injected as developer context so the model aims for conforming output on its own.
- When the model produces a final message (no tool calls), cake validates it locally against the compiled schema. If it validates, the run succeeds and `result` is exactly the JSON document — no fences, no prose.
- If it does not validate, cake runs a bounded corrective loop (2 retry turns): it appends a corrective message containing the validation errors, disables tools for that request (no tools offered), and attaches the native structured-output constraint (Responses API `text.format` with `json_schema`; Chat Completions `response_format` with `json_schema`, both `strict`). If the provider rejects the constrained request with HTTP 400, the turn is retried without the native constraint. Local validation remains authoritative in all cases; native enforcement is best-effort acceleration.
- On retry exhaustion, refusal, or truncation, the run fails loudly on both channels: a `task_complete` record with new subtype `error_output_schema`, `is_error: true`, and validation detail in `error`; and process exit code 1 (agent error). The new subtype is additive, following the precedent set by `interrupted` in ADR-011 — consumers keying on `is_error` are unaffected.
- With `--output-format json`, the top-level `result` field stays a JSON string containing the document (no shape change). With `--output-format text`, stdout is exactly the JSON document.
- The schema is per-invocation and is not persisted to the session file. Corrective turns are ordinary conversation items, so resumed sessions replay cleanly.

### Consequences

- Good, because the contract holds on any OpenAI-compatible provider, including ones with no structured-output support at all.
- Good, because agent behavior during tool use is byte-for-byte unchanged when the flag is absent, and unconstrained-at-the-API-level when it is present.
- Good, because all contract changes (new flag, new subtype) are additive to existing compatibility surfaces.
- Bad, because the `jsonschema` crate and its transitive dependencies add compile time and binary size; the implementation must use `default-features = false` plus only the needed features, and the binary-size audit must be run before and after.
- Bad, because a non-conforming final answer costs up to two extra API turns.
- Bad, because native strict modes support only a subset of draft 2020-12, so some valid schemas will be rejected by the provider (HTTP 400) and fall back to the unconstrained-retry path, which is slower and less certain to converge.

## More Information

- Task 249: Add Schema-Constrained Final Output.
- ExecPlan: `.agents/exec-plans/active/schema-constrained-final-output.md`.
- ADR-011 established the additive-`TaskCompleteSubtype` precedent (`interrupted`).
- Key code: `src/clients/agent/agent_loop.rs` (`Agent::send`, final-message branch), `src/clients/responses_types.rs` / `src/clients/chat_types.rs` (request DTOs), `src/types/session.rs` (`TaskOutcome`, `TaskCompleteSubtype`), `src/exit_code.rs` (classification), `src/main.rs` (flag parsing and pre-run validation).
