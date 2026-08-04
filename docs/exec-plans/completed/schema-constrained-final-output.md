# Schema-Constrained Final Output

This ExecPlan implements task 249 and the decision recorded in `docs/adr/012-schema-constrained-final-output.md` (ADR-012). Read ADR-012 before starting; if its status is still `proposed`, accept it (set `status: accepted` in its front matter) before writing code. This plan is self-contained: everything needed to implement the feature is restated here.

## Purpose

After this change, a caller can run:

```
cake --output-schema review.schema.json --output-format stream-json "Review this diff and report findings"
```

and the final `task_complete` record's `result` field is guaranteed to be a single JSON document that validates against the schema in `review.schema.json` --- no Markdown fences, no surrounding prose. If cake cannot produce a conforming document, the run fails loudly: the `task_complete` record carries a new `error_output_schema` subtype and the process exits nonzero. A caller can therefore apply cake's output mechanically without parsing prose. The motivating consumer is ahm's delegation commands (`ahm task groom`, `ahm audit`), which hand a procedure prompt to cake and machine-apply the structured result.

Everything else about a run is unchanged: the agent still uses tools, streams intermediate messages, persists sessions, and honors `--continue`/`--resume`/`--fork`. Only the final assistant response is constrained. Runs without `--output-schema` are byte-for-byte unaffected.

## Orientation

cake is a one-shot, non-interactive Rust CLI (Rust 2024, binary-only). One invocation sends one prompt, runs an agent loop until the model stops calling tools, prints the result, and exits. There is no REPL, so "the final response" always means the last assistant message of this invocation.

Terms used in this plan:

- The **agent loop** is `Agent::send` in `src/clients/agent/agent_loop.rs`. It pushes the user message, then loops: call the model (one **turn**), execute any tool calls the model requested, feed the outputs back, repeat. When a turn contains no tool calls, the loop returns `self.conversation.resolve_assistant_message()` --- that string is the final response. This return point (the `function_calls.is_empty()` branch, around line 89) is where schema enforcement plugs in.
- A **backend** is one of two OpenAI-compatible request/response codecs: `src/clients/responses.rs` (Responses API) and `src/clients/chat_completions.rs` (Chat Completions). Both expose `send_request(client, config, history, tools, overrides)` and `parse_response(response)`, dispatched through the `Backend` enum in `src/clients/backend.rs`. Request body DTOs live in `src/clients/responses_types.rs` (`Request`) and `src/clients/chat_types.rs` (`ChatRequest`). Neither currently sends any structured-output field.
- `AgentRunner::complete_turn` in `src/clients/agent_runner.rs` wraps one backend call in the HTTP retry loop (rate limits, overload, context overflow). The agent loop reaches it via `Agent::complete_turn` in `agent_loop.rs`.
- The **conversation** is `ConversationState` in `src/clients/agent_state.rs`, a `Vec<ConversationItem>`. Relevant methods: `push_user_message` (appends a user item and returns it so the caller can stream it), `append_developer_context` (appends developer-role items; `src/main.rs` calls it before `send` so they land in the System/Developer prefix that `emit_prompt_context_records` persists), and `resolve_assistant_message`.
- **stream-json** is the machine-readable output mode: with `--output-format stream-json`, every conversation item and lifecycle record is printed as one JSON line. The lifecycle records are defined in `src/types/session.rs`: `TaskOutcome` / `TaskCompleteSubtype` currently have `Success`, `ErrorDuringExecution`, and `Interrupted` variants, serialized into the `task_complete` record with `subtype`, `is_error`, `result`, `error` fields. `src/main.rs::handle_agent_turn_result` maps the `Result<String>` from `Agent::send` to a `TaskOutcome` and emits the record.
- **Exit codes** are defined in `src/exit_code.rs`: 0 success, 1 agent error, 2 API error, 3 input error, 130 interrupted. `classify_to_u8` walks an `anyhow::Error` chain, downcasting typed errors first (see `ApiError` as the precedent) before falling back to string matching.
- **Session files** are append-only JSONL transcripts under the data dir. Conversation items pushed through the normal loop are persisted and replayed on `--resume`, so anything we append as an ordinary conversation item is resume-safe for free.

## The Contract

- `--output-schema <path>`: path to a JSON Schema file (draft 2020-12) describing the shape of the final response. Applies to any run; composes with all `--output-format` values and with `--continue`, `--resume`, `--fork`.
- Success: the final response is exactly the schema-valid JSON document. In `stream-json` mode it is the `result` of the `task_complete` record; in `json` mode the top-level `result` field remains a JSON *string* containing the document (no shape change); in `text` mode stdout is exactly the document.
- Unreadable or syntactically/semantically invalid schema file: fail before the run starts (before `task_start` is emitted, before any worktree is created) with a clear human-readable error on stderr and exit code 3.
- Final output that cannot be made schema-valid (refusal, truncation, correction exhaustion): `task_complete` with `subtype: "error_output_schema"`, `is_error: true`, validation detail in `error`, `result` omitted; exit code 1.
- A caller must never see a `success` `task_complete` whose `result` does not validate.

## Design (from ADR-012)

Local validation is authoritative; native provider enforcement is best-effort acceleration. The flow:

1. Pre-run, load and compile the schema with the `jsonschema` crate (new dependency, minimal features, no remote/file `$ref` resolution --- schemas must be self-contained).
2. Inject a developer-context message stating the final-output requirement (with the schema inline) so the model usually gets it right the first time. The agent loop itself runs completely unconstrained --- attaching `response_format` to every request would force preamble text on tool-use turns into JSON, which task 249 forbids.
3. When the model produces a final (no-tool-call) message, validate it locally. Valid → done.
4. Invalid → **correction mode**, at most 2 correction turns: append a corrective user message listing the validation errors, and issue the next turn with no tools offered and the native structured-output constraint attached (Responses `text.format` json_schema, Chat Completions `response_format` json_schema, both `strict: true`). If the provider rejects the constrained request with HTTP 400 (strict modes support only a subset of draft 2020-12), permanently drop the native constraint for this run and retry that turn unconstrained --- this fallback does not consume a correction turn and may happen only once.
5. Exhaustion → typed `OutputSchemaError::Unsatisfied` error, mapped to the `error_output_schema` outcome and exit 1.

Correction turns run *inside* the existing loop in `Agent::send` (a mode flag plus counter, not a separate loop), so turn counting, usage accumulation, streaming, persistence, and telemetry all work unchanged.

## Milestone 1: Schema loading, CLI flag, pre-run failure

At the end of this milestone, `cake --output-schema <path> "<prompt>"` accepts the flag, fails fast with exit 3 on a bad schema file, and threads a compiled schema into the `Agent`. No enforcement happens yet.

Add the dependency (do not update any other dependency):

```
cargo add jsonschema --no-default-features
```

Check the crate's feature list for the version that gets pinned; enable only what draft 2020-12 compile-and-validate needs. The goal is explicitly *no* network or filesystem `$ref` resolution --- a schema that references external resources should fail to compile. Keep `Cargo.toml` and `Cargo.lock` consistent. Record the release binary size before and after (`cargo build --release`, then note the size of `target/release/cake`) in Surprises & Discoveries; binary size is audited in this repository.

Create `src/config/output_schema.rs` (register it in `src/config/mod.rs` and re-export like the module's other types):

- `pub struct OutputSchema { pub name: String, pub raw: serde_json::Value, pub validator: jsonschema::Validator }`. `name` is the schema file stem (providers require a name in the json_schema payload; sanitize to `[a-zA-Z0-9_-]`, fall back to `"final_output"`).
- `pub enum OutputSchemaError` (thiserror), variants `Unreadable { path, source }`, `InvalidJson { path, source }`, `InvalidSchema { path, detail }`, and `Unsatisfied { attempts: u32, detail: String }` (used from Milestone 2 onward). Messages must be clear and human-readable; they go to stderr verbatim.
- `impl OutputSchema { pub fn load(path: &Path) -> Result<Self, OutputSchemaError> }` --- read file, parse JSON, compile with the crate's draft-2020-12 validator entry point.

Wire the flag in `src/main.rs` on the `CodingAssistant` clap struct, next to the other output options:

```
/// Constrain the final response to a JSON document valid against
/// this JSON Schema file (draft 2020-12). Only the final response
/// is constrained; tool use and intermediate output are unchanged.
#[arg(long, value_name = "PATH")]
pub output_schema: Option<String>,
```

Load the schema at the very top of `CmdRunner::run` for the no-subcommand path, before `prepare_run()` (which creates worktrees) --- bad input must not leave a stale worktree, mirroring how stdin is validated before worktree setup. Resolve the path against the startup directory. Wrap the compiled schema in `Arc<OutputSchema>` and attach it to the agent after `build_client_and_session` returns:

```
run_session.agent = run_session.agent.with_output_schema(Arc::clone(&schema));
```

(`with_output_schema` is added to `Agent` in Milestone 2; for this milestone it can be a stub that stores the field.)

Classify the new error in `src/exit_code.rs::classify_to_u8`, as a typed downcast before the string-matching walk (the `ApiError` downcast is the pattern to copy): `Unreadable`/`InvalidJson`/`InvalidSchema` → `code::INPUT_ERROR`; `Unsatisfied` → `code::AGENT_ERROR`.

Tests: unit tests in `output_schema.rs` for missing file, non-JSON file, and a structurally invalid schema (e.g. `{"type": 123}`); classification tests alongside the existing ones in `src/exit_code.rs`. Verify by hand:

```
cargo run -- --output-schema /nonexistent.json "hi"; echo $?
# clear error on stderr, exit code 3, no task_start emitted
```

## Milestone 2: Enforcement in the agent loop

At the end of this milestone, a run with a schema either returns a locally-validated JSON document or fails with `OutputSchemaError::Unsatisfied`. Correction turns work but do not yet attach the native provider constraint (that is Milestone 3); they rely on the corrective message alone.

`Agent` (`src/clients/agent.rs`) gains `output_schema: Option<Arc<OutputSchema>>` (default `None`) and a `with_output_schema` builder.

Developer-context injection: in `src/main.rs::execute_agent_turn`, before `client.send`, when a schema is attached, call `client.append_developer_context` with one message stating: the final response must be a single JSON document valid against the following JSON Schema, with no Markdown fences and no surrounding prose; then the schema JSON pretty-printed. This lands in the Developer prefix that `emit_prompt_context_records` already persists, so transcripts record what the model was told.

In `Agent::send` (`src/clients/agent/agent_loop.rs`), add loop-local state when a schema is present: `corrections_used: u32` and `in_correction_mode: bool`, plus a constant `MAX_SCHEMA_CORRECTION_TURNS: u32 = 2`. Change the `function_calls.is_empty()` branch:

- No schema attached: return the message exactly as today.
- Schema attached: take `resolve_assistant_message()`, trim it, and validate: first `serde_json::from_str::<serde_json::Value>` (parse failure is a validation failure), then `schema.validator.validate`. Deliberately **no** leniency such as stripping Markdown fences --- a fenced document is non-conforming, and the corrective message tells the model to remove the fences. On success return the trimmed string.
- On failure with `corrections_used == MAX_SCHEMA_CORRECTION_TURNS`: return `Err(OutputSchemaError::Unsatisfied { attempts, detail }.into())` where `detail` includes the collected validation error messages (bounded --- cap the number and length of reported errors so a pathological schema doesn't bloat the record).
- Otherwise: increment `corrections_used`, set `in_correction_mode = true`, push a corrective **user** message via the existing `push_user_message` (which returns the item for `stream_item`, so it streams and persists like any other item --- this is why a user-role message was chosen over an out-of-band mechanism), and continue the loop. The corrective message states that the previous response failed schema validation, lists the errors, and repeats: respond with only the JSON document, no fences, no prose.

While `in_correction_mode`, `Agent::complete_turn` passes an empty tool slice instead of `self.tools.definitions()`. `chat_completions.rs` already omits `tools`/`tool_choice` when the slice is empty (line \~53); make `responses.rs` do the same (it currently hardcodes `tool_choice: Some("auto")` at line \~64 --- omit both when tools are empty). Defensive edge: if a correction turn nevertheless returns function calls (a misbehaving provider), do not execute them --- treat that turn as a failed validation attempt and continue the correction loop.

Tests in `src/clients/agent/agent_tests.rs` and the wiremock-backed backend tests, following existing fixtures: (a) conforming first answer passes through untouched and consumes no correction turn; (b) non-conforming answer followed by a conforming correction succeeds, transcript contains the corrective user message, `turn_count` reflects the extra turn; (c) persistent non-conformance exhausts after 2 corrections and returns `Unsatisfied`; (d) fenced-JSON answer fails validation; (e) no schema → behavior identical to before (assert no new items).

## Milestone 3: Native structured output on correction turns

At the end of this milestone, correction-turn requests carry the provider's structured-output field, with snapshot coverage of the exact wire shape, and the HTTP 400 fallback works.

Define a small borrow-friendly carrier, e.g. in `src/clients/backend.rs`: `pub(super) struct FinalOutputConstraint<'a> { pub name: &'a str, pub schema: &'a serde_json::Value }`. Thread `Option<FinalOutputConstraint>` as a **new parameter** through `Backend::send_request` → `AgentRunner::complete_turn` → both backends' `send_request`. Do not stuff it into `RequestOverrides`: that struct drives HTTP retry semantics and is snapshotted into telemetry (`RequestOverridesSnapshot`), and the schema is not a retry concern.

Wire shapes (both `strict: true`):

- Responses API (`src/clients/responses_types.rs::Request`): add an optional `text` field, skipped when `None`:

  ```
  "text": {"format": {"type": "json_schema", "name": "<name>", "strict": true, "schema": { ... }}}
  ```

- Chat Completions (`src/clients/chat_types.rs::ChatRequest`): add an optional `response_format` field, skipped when `None`:

  ```
  "response_format": {"type": "json_schema", "json_schema": {"name": "<name>", "strict": true, "schema": { ... }}}
  ```

`Agent::complete_turn` passes the constraint only while `in_correction_mode` and while a run-scoped `native_constraint_enabled` flag (initialized `true` when a schema is attached) holds. The 400 fallback lives in the correction branch of `Agent::send`: if the turn errors and the chain downcasts to `exit_code::ApiError` with `status == 400` while the constraint was attached, set `native_constraint_enabled = false`, log a warning naming the fallback, and retry the same turn without consuming a correction turn. The flag never resets, so this branch runs at most once per invocation --- no loop risk.

Snapshot tests: this repo snapshot-tests API request construction with insta (see `src/clients/snapshots/`). Add request snapshots for both backends with a constraint attached; run `just snapshots` and review with `cargo insta review` --- never leave `.snap.new` files in the worktree. Add a wiremock test for the 400 fallback: first constrained request answered with 400, unconstrained retry answered with a conforming body, run succeeds.

## Milestone 4: Failure contract --- outcome subtype and exit code

At the end of this milestone, exhaustion is machine-distinguishable in the stream and at the process boundary.

In `src/types/session.rs`, add `ErrorOutputSchema` to `TaskCompleteSubtype` (serialized `error_output_schema`) and `TaskOutcome::ErrorOutputSchema { error: String }` with `is_error() == true`. Extend the manual `Serialize`/`Deserialize` impls exactly like `ErrorDuringExecution` (`error` populated, `result` omitted; deserialization requires `error`). This is additive --- the same precedent as ADR-011's `interrupted` subtype; consumers keying on `is_error` are unaffected. Update the serialization tests/snapshots in `src/types/session_tests.rs`.

In `src/main.rs::handle_agent_turn_result`, in the `Err` arm, downcast: `OutputSchemaError::Unsatisfied` → emit `TaskOutcome::ErrorOutputSchema` instead of `ErrorDuringExecution`. Exit-code classification was already handled in Milestone 1 (`Unsatisfied` → 1); add a test that the full error chain produced by the loop actually classifies to 1 (guard against a validation-detail string accidentally matching `is_input_error`/`is_api_network_error` patterns).

Rendering needs no structural change: `text` mode already propagates `Err` to stderr via `write_error`; `json` mode already emits `result: null` plus `error`. Add a `main_tests.rs` case asserting the emitted `task_complete` record for an exhausted run (subtype, `is_error`, `error` populated, `result` absent).

## Milestone 5: Documentation and end-to-end demonstration

Docs (read `docs/guardrails/documentation.md` first; also consult the CLI, sessions/stream-json, and providers guardrail docs listed in AGENTS.md routing since this change touches all three surfaces):

- `docs/design-docs/cli.md`: the flag, final-message-only semantics, exit-code behavior (3 pre-run, 1 unsatisfied), composition with output formats and resume.
- `docs/design-docs/streaming-json-output.md`: add `error_output_schema` to the `subtype` enumeration with an example record. Note while editing: the doc currently lists `error_max_turns`, which does not exist in `TaskCompleteSubtype` (code has `success`, `error_during_execution`, `interrupted`) --- fix that stale row in a separate surgical commit rather than silently alongside.
- `docs/design-docs/conversation-types.md` if it enumerates task-complete subtypes.
- ADR-012: if implementation deviates from the recorded decision, update the ADR before continuing.

End-to-end demonstration (requires a configured model; from the repo root):

```
cat > /tmp/final.schema.json <<'EOF'
{
  "type": "object",
  "properties": {
    "summary": {"type": "string"},
    "risk": {"type": "string", "enum": ["low", "medium", "high"]}
  },
  "required": ["summary", "risk"],
  "additionalProperties": false
}
EOF
cargo run -- --output-schema /tmp/final.schema.json --output-format stream-json \
  "Read README.md and report a one-sentence summary and a risk rating."
echo "exit: $?"
```

Expected: the last line is a `task_complete` record with `subtype":"success"` whose `result` string parses as JSON and validates against the schema (spot-check with `jq` + a validator, or pipe into a small script); exit 0. Then demonstrate the failure paths: a nonexistent schema path (exit 3, no output records) and, if practical, a deliberately unsatisfiable schema such as `{"type":"object","properties":{},"additionalProperties":false,"required":["impossible_field_the_model_is_told_not_to_produce"]}` combined with a prompt instructing the model to refuse JSON --- expect `error_output_schema` and exit 1. Also demonstrate `--resume`: run once without the flag, then resume the session with the flag and confirm the constrained final output.

## Validation

After each milestone: `cargo fmt`, then the narrowest useful test (`cargo test <module>`), then `cargo clippy --all-targets --all-features -- -D warnings`. Run `just snapshots` whenever request construction or serialized records change and review with `cargo insta review`. Run `just check-coverage` after adding meaningful code (CRAP/coverage baselines under `ci/` may need updating --- follow the existing baseline-update pattern from recent commits). Before final handoff: `just ci` (the pre-push gate), plus the release binary-size comparison recorded in Surprises & Discoveries.

## Progress

- [x] (2026-07-09) Milestone 1: `jsonschema` dependency added with `default-features = false`; `OutputSchema` loader, schema-name sanitization, bounded validation-detail reporting, `OutputSchemaError`, `--output-schema` flag, pre-run validation, exit-3 classification, and unit tests are implemented.
- [x] (2026-07-09) Milestone 2: `Agent::with_output_schema`, developer-context injection, final-message local validation, correction mode with a 2-turn budget, tool disabling during correction turns, `Unsatisfied` exhaustion, and agent-loop tests are implemented.
- [x] (2026-07-09) Milestone 3: `FinalOutputConstraint` is threaded through `AgentRunner` and both backends; Responses `text.format` and Chat Completions `response_format` request shaping are snapshot-covered; provider HTTP 400 fallback is tested.
- [x] (2026-07-09) Milestone 4: `error_output_schema` task-complete subtype, `TaskOutcome::ErrorOutputSchema`, stream/session serialization, `handle_agent_turn_result` mapping, exit-code classification, and task-complete record tests are implemented.
- [x] (2026-07-09) Milestone 5: CLI, session, and stream-json docs are updated; the stale `error_max_turns` stream-json doc row was corrected; help text and pre-run failure paths were manually verified; `just ci` is green; release binary-size delta is recorded below.

## Surprises & Discoveries

- (grooming, 2026-07-08) `docs/design-docs/streaming-json-output.md` documents an `error_max_turns` subtype that does not exist in `src/types/session.rs` --- pre-existing doc drift, to be corrected in Milestone 5.
- (implementation, 2026-07-09) The `jsonschema` crate brought a measurable release binary-size increase. A clean `HEAD` archive release build produced `/private/tmp/cake-task249-baseline-5650/target/release/cake` at 6,407,440 bytes; the completed working-tree release build produced `target/release/cake` at 8,542,336 bytes, a delta of 2,134,896 bytes (about 2.0 MiB).
- (verification, 2026-07-09) Initial `just ci` after the inherited implementation failed only the CRAP regression gate. Focused behavior-preserving extractions in `src/clients/agent/agent_loop.rs`, `src/types/session.rs`, `src/exit_code.rs`, and `src/main.rs` reduced the report from 7 regressions to 0 without regenerating `ci/cargo-crap-baseline.json`.

## Decision Log

- 2026-07-08 (grooming, maintainer-approved): local validation with the `jsonschema` crate is authoritative; native provider constraints are best-effort on correction turns only. Dialect is draft 2020-12, self-contained schemas only (no external `$ref`). Failure signals on both channels: `error_output_schema` subtype and exit 1; pre-run schema errors exit 3. Max 2 correction turns. See ADR-012.
- 2026-07-08 (planning): correction turns run inside the existing `Agent::send` loop via a mode flag rather than a separate post-loop, so turn counting, usage, streaming, persistence, and telemetry need no parallel plumbing.
- 2026-07-08 (planning): the corrective message is a user-role conversation item via `push_user_message`, because that path already streams and persists the item and replays on resume.
- 2026-07-08 (planning): the schema constraint is threaded as a new `Option<FinalOutputConstraint>` parameter, not a `RequestOverrides` field, because `RequestOverrides` drives HTTP retry semantics and is snapshotted into telemetry.
- 2026-07-08 (planning): no fence-stripping leniency --- a fenced JSON document is a validation failure handled by the correction loop, keeping the success contract exact.
- 2026-07-09 (completion): keep the CRAP baseline unchanged and reduce regressions through small helper extractions. Rationale: the reported regressions were in aggregation functions that had absorbed new branches; extracting existing logic made the risk profile clearer and allowed `just check-coverage` to pass without accepting a broader baseline change.

## Outcomes & Retrospective

Completed. `cake --output-schema <path>` now loads a self-contained draft 2020-12 JSON Schema before run setup, injects final-output guidance as developer context, validates only the final no-tool-call response locally, runs at most two tool-disabled correction turns, attaches native provider structured-output constraints only to correction turns, falls back from provider HTTP 400 strict-schema rejection, and emits `error_output_schema` plus exit 1 on exhaustion. Pre-run schema file errors fail before `task_start` with exit 3.

Verification completed:

- `cargo test output_schema --all-features`
- `cargo test task_outcome --all-features`
- `cargo test classify_output_schema --all-features`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `just check-coverage`
- `just ci`
- `just check-deps` was run and failed on pre-existing direct dependency `anyhow 1.0.102` advisory RUSTSEC-2026-0190. This task did not change `anyhow`, and the unrelated dependency update was left for a separate decision.
- Manual help check: `cargo run -- --help | rg -C 2 -- "--output-schema"`
- Manual unreadable schema check: `cargo run -- --output-schema /private/tmp/cake-task249/missing.schema.json --output-format stream-json hi; printf 'exit:%s\n' $?` printed only the schema read error and `exit:3`.
- Manual invalid schema check: `cargo run -- --output-schema /private/tmp/cake-task249/invalid.schema.json --output-format stream-json hi; printf 'exit:%s\n' $?` printed only the schema validation error and `exit:3`.
- Preflight: fixed ADR-012's ExecPlan reference to the completed plan path and verified no stale active-plan reference remains.

The live-model success and resume paths were not exercised against an external provider during completion; they are covered by wiremock-backed agent-loop/backend tests and session tests in the local suite.
