# Add Per-Session Telemetry Logs

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agents/PLANS.md`.

## Purpose / Big Picture

After this change, every saved cake session will also have a structured per-session telemetry sidecar that explains how the run performed. A developer will be able to answer questions such as "how many API attempts happened," "where did the time go," "which retries fired," and "how long did each tool call take" without digging through the global daily log or reconstructing timing from the session transcript. The user-visible behavior is simple: after running `cake`, a second file appears in a dedicated cache-side telemetry directory for that session, and that file contains one JSON object per telemetry event.

The feature matters because cake currently preserves the semantic transcript in the session JSONL file and preserves process-wide debugging output in the daily rotating log, but it does not preserve a durable, session-scoped performance timeline. The Amp log that motivated this feature is exactly that missing middle layer. cake should have its own version so performance work, retry analysis, and support debugging are all grounded in the same session identifier.

## Progress

- [x] (2026-05-02 17:52Z) Read `.agents/PLANS.md`, the session persistence code, the daily logger, the agent loop, retry handling, and the existing session documentation.
- [x] (2026-05-02 17:52Z) Chose the high-level design: a structured sidecar file per session, appended incrementally during execution.
- [x] (2026-05-02 17:52Z) Authored the initial ExecPlan in `.agents/.plans/per-session-telemetry-plan.md`; it now lives at `.agents/exec-plans/active/per-session-telemetry-plan.md`.
- [x] (2026-05-02 18:02Z) Revised the storage location to `~/.cache/cake/session-telemetry/` (or `$CAKE_DATA_DIR/session-telemetry/`) so telemetry stays out of the resumable session directory.
- [ ] Implement the telemetry record schema, append-only writer, and session-sidecar path helper.
- [ ] Wire telemetry into session startup, API attempts, retries, tool execution, and final session summary.
- [ ] Add end-to-end and unit tests that prove sidecar creation, retry recording, and `--no-session` behavior.
- [ ] Update the user-facing and agent-facing docs, then run `just ci` from the repository root.

## Surprises & Discoveries

- Observation: `src/config/data_dir.rs` discovers the latest saved session by scanning every file in the sessions directory whose extension is exactly `.jsonl`.
  Evidence: `load_latest_session()` filters with `entry.path().extension().is_some_and(|ext| ext == "jsonl")` before reading headers.

- Observation: the session transcript format and `--output-format stream-json` intentionally share the same `SessionRecord` schema so redirected stream output can be resumed later.
  Evidence: `src/clients/types.rs` documents that every line in both stream-json output and session history files is a `SessionRecord`, and `src/config/data_dir.rs` explicitly supports `--resume <path>` for redirected stream output.

- Observation: cake already knows total session duration and token usage at the end of a run, but it does not retain the intermediate timings that explain why a run was slow.
  Evidence: `src/main.rs` measures one overall `duration_ms` and `src/clients/agent.rs` accumulates usage totals, while `complete_turn()` currently discards request-attempt timing once the turn finishes.

## Decision Log

- Decision: Store telemetry in a dedicated cache subdirectory at `~/.cache/cake/session-telemetry/` by default, or `$CAKE_DATA_DIR/session-telemetry/` when `CAKE_DATA_DIR` is set. The file name should be `{session_id}.ndjson`.
  Rationale: Telemetry is operational logging, not resumable conversation state. Keeping it out of `sessions/` preserves the mental model that the session directory only contains transcript files and avoids polluting session discovery with non-session artifacts.
  Date/Author: 2026-05-02 / Amp

- Decision: Keep telemetry separate from `SessionRecord` instead of adding new record variants to the resumable transcript.
  Rationale: The transcript file must remain focused on conversation state and resume semantics. A performance log has different retention, fields, and query patterns, and it should not change what `--output-format stream-json` emits.
  Date/Author: 2026-05-02 / Amp

- Decision: Append telemetry records incrementally during the run and flush after each record.
  Rationale: A sidecar is most valuable when the run fails, retries repeatedly, or is interrupted. End-of-session buffering would lose the exact cases this feature is meant to debug.
  Date/Author: 2026-05-02 / Amp

- Decision: Include a fresh `invocation_id` on every telemetry record in addition to `session_id`.
  Rationale: cake reuses the same session identifier across `--continue` and `--resume`, so a true per-session file can span multiple CLI invocations. `invocation_id` is the clean way to separate those runs without creating another file naming scheme.
  Date/Author: 2026-05-02 / Amp

- Decision: Do not store prompt text, assistant text, or raw tool output bodies in telemetry records.
  Rationale: The session transcript already holds semantic content. The telemetry sidecar should stay compact, low-sensitivity, and performance-oriented. It should store counts, durations, status, and short classifications instead.
  Date/Author: 2026-05-02 / Amp

- Decision: Telemetry is best-effort and must never fail the user's primary session.
  Rationale: A debugging aid cannot become a new source of fatal errors. If telemetry writing fails, cake should emit one warning to the daily log, disable further telemetry for that run, and continue normally.
  Date/Author: 2026-05-02 / Amp

## Outcomes & Retrospective

This plan captures the feature as a durable session-sidecar design rather than a transcript format change. That keeps the implementation narrow and preserves all existing resume behavior. The storage location has been refined so telemetry now lives in a dedicated cache subdirectory instead of beside session transcripts. No code has been written yet, so there is no runtime outcome to report beyond the design itself. The main remaining risk is implementation discipline: the telemetry hooks need to land in the right lifecycle points without turning the agent loop into a maze of incidental bookkeeping.

## Context and Orientation

cake currently has two adjacent persistence layers.

The first is the durable session transcript. `src/main.rs` builds a `Session`, runs the agent, drains the agent's stream buffer, and saves a `{session_id}.jsonl` transcript via `src/config/data_dir.rs` and `src/config/session.rs`. The transcript is built from `SessionRecord` values defined in `src/clients/types.rs`. Those records capture the conversation itself: init metadata, messages, function calls, function call outputs, reasoning summaries, and the final result. That file is intentionally resumable and is the same schema used by `--output-format stream-json`.

The second is the global daily log. `src/logger.rs` configures a rotating file logger under `~/.cache/cake/` (or `$CAKE_DATA_DIR/` when overridden). That log is process-wide rather than session-scoped. It is helpful for broad debugging, but it is a poor artifact for answering session-specific performance questions because it mixes unrelated runs and does not preserve a structured per-session timeline.

The missing piece is a telemetry sidecar. In this plan, "telemetry" means structured operational facts about a session run: timestamps, durations, retry reasons, tool timings, token usage, and success or failure classification. In this plan, "sidecar" means a second file keyed by the same session identifier as the transcript but written to a dedicated cache-side telemetry directory instead of the transcript directory.

The files and modules that matter most are:

`src/main.rs` owns the end-to-end session lifecycle, including when a session starts, when the final duration is known, and whether `--no-session`, `--continue`, `--resume`, or `--fork` is active.

`src/clients/agent.rs` owns the inner loop, including API attempts, retry waits, tool execution, usage accumulation, and the transition from model output to function-call execution.

`src/clients/retry.rs` already classifies retry reasons and delays. The telemetry sidecar should record those same decisions rather than inventing new retry taxonomy.

`src/config/data_dir.rs` decides where session artifacts and cache artifacts live. It should also become the single place that derives the telemetry sidecar path under the cache tree.

`README.md`, `docs/design-docs/session-management.md`, and `.agents/skills/debugging-cake/SKILL.md` already teach people and future agents where to find session artifacts. They should be updated so this new file is discoverable.

One important non-goal must stay explicit: this feature should not attempt to reproduce streaming token timing such as "time to first token." cake currently performs non-streaming provider calls internally, so the telemetry file can accurately measure request/response phases, parse time, tool time, retry delays, and whole-session time, but not token-by-token latency.

## Plan of Work

### Milestone 1: Define and persist a structured telemetry sidecar

Start by introducing a new module at `src/session_telemetry.rs`. That module should define the telemetry record schema and an append-only writer that emits one JSON object per line to `session-telemetry/{session_id}.ndjson` under the cache tree. The writer should open the file in create-and-append mode, write a newline after every record, and flush immediately so the file remains useful even if cake exits unexpectedly. This module should also define the small metadata types used by the records, including the run mode (`new`, `continue`, `resume`, or `fork`) and the optional invocation-scoped settings that influence performance such as `api_type`, `output_format`, `max_output_tokens`, `reasoning_effort`, and `reasoning_max_tokens`.

At the same time, add a helper to `src/config/data_dir.rs` that derives the sidecar path from a `session_id`. The helper must return a path under `get_cache_dir().join("session-telemetry")` and ensure that parent directory can be created before the writer opens the file. The new helper should be used everywhere the telemetry writer is initialized so path decisions remain centralized.

This milestone is complete when a small unit test in `src/session_telemetry.rs` proves that writing a few telemetry records produces newline-delimited JSON and when a `DataDir` test proves the sidecar path lives under the cache telemetry directory with the expected suffix.

### Milestone 2: Attach telemetry to the session lifecycle in `main.rs`

Once the writer exists, thread it into the main session lifecycle without changing the transcript semantics. `src/main.rs` should create the telemetry writer only when the run will actually be persisted. That means telemetry should be skipped entirely when `--no-session` is set, because that flag already promises that cake will not leave session artifacts on disk.

When telemetry is enabled, `main.rs` should allocate a fresh `invocation_id`, determine the run mode, and emit an initial telemetry record before the first API turn begins. This init record should include the session identifier, invocation identifier, current working directory, resolved model name, API type, output format, selected tool names, and any relevant request-budget settings that are already known before the first turn.

At the end of the run, `main.rs` should emit a final session-summary telemetry record using the same end-to-end `duration_ms` and accumulated usage totals that already feed `format_done_summary()` and `emit_result_message()`. The summary must exist for both successful and failed runs so the sidecar answers "how did this session end?" in one place.

This milestone is complete when a run that currently produces `sessions/{uuid}.jsonl` also produces `session-telemetry/{uuid}.ndjson`, and when `--no-session` still leaves the data directory empty.

### Milestone 3: Instrument API attempts, retries, and tool execution in `src/clients/agent.rs`

The agent loop is where the interesting timings live, so the telemetry writer should become an optional field on `Agent`, set via a builder-style method such as `with_session_telemetry(...)`. `Agent` should expose small helper methods that append telemetry records and automatically disable telemetry after the first write failure. That keeps the call sites readable and enforces the best-effort guarantee in one place.

Inside `complete_turn()`, measure each API attempt with `Instant`. For every attempt, record enough data to explain the outcome: logical turn index, attempt number, response status code when present, the time spent sending the request and waiting for the initial HTTP response, the time spent parsing the response body, the total attempt duration, the request overrides active for that attempt, the number of conversation items already in history, and the optional usage block when parsing succeeds. When parsing fails after a `2xx` response, emit an attempt record that carries the parse failure string before returning the error upward.

Still inside `complete_turn()`, every retry decision produced by `src/clients/retry.rs` should also become a telemetry record. Record the same reason and delay that the retry callback already surfaces, along with the detail string and whether the retry changed request overrides. This part is important because a future developer debugging slow sessions will often care more about "why did we back off" than about the raw HTTP body.

Inside `send()`, measure tool execution per call. The current implementation executes tools concurrently and only keeps the output string. Preserve that behavior, but return a richer internal struct from the asynchronous tool futures so the loop also has `duration_ms`, `output_bytes`, and a boolean that says whether the tool resolved through the existing error string path. The telemetry sidecar should write one tool-call record per completed tool, keyed by `call_id`, with the logical turn index and tool name so later analysis can group tool delays by turn.

This milestone is complete when a successful tool-free run produces at least one `api_attempt` record and one `session_summary` record, and a tool-using or retrying run produces additional `tool_call` and `retry_scheduled` records without changing any existing user-visible output.

### Milestone 4: Prove the feature with tests and document how to use it

Add a new integration test file at `tests/session_telemetry.rs`. Use the existing `tests/support/mod.rs` harness so each test gets its own temporary `CAKE_DATA_DIR`. At minimum, add one success-path test that runs the CLI against a `wiremock` server, verifies that both the session transcript and the telemetry sidecar exist in their separate directories, and checks that the telemetry file contains `telemetry_init`, `api_attempt`, and `session_summary` records. Add one retry-path test that forces an initial retriable failure followed by success and verifies that at least one `retry_scheduled` record and multiple `api_attempt` records were written. Extend the existing `tests/stdin_handling.rs` `--no-session` coverage so it also asserts that no telemetry file is created.

After the tests are in place, update `README.md` to mention the new telemetry directory alongside the existing session-transcript description. Update `docs/design-docs/session-management.md` so the storage layout shows `sessions/{uuid}.jsonl` and `session-telemetry/{uuid}.ndjson`, and clearly state that only the `.jsonl` file is resumable. Update `docs/design-docs/logging.md` so the telemetry directory is documented as session-scoped structured logging. Update `.agents/skills/debugging-cake/SKILL.md` so future agents know the sidecar exists and can inspect it with `jq` when investigating performance or retries.

This milestone is complete when the new tests pass, the docs name the file and its purpose, and `just ci` succeeds from the repository root.

## Concrete Steps

All commands below are run from the repository root.

1. Implement the telemetry module and path helper.

   cargo test session_telemetry

   Expected result: the new unit tests in `src/session_telemetry.rs` and `src/config/data_dir.rs` pass, proving newline-delimited writes and the cache-based `session-telemetry/{uuid}.ndjson` path.

2. Wire the telemetry writer into `src/main.rs` and `src/clients/agent.rs`, then add the integration tests.

   cargo test session_telemetry_creates_sidecar_on_success -- --exact --nocapture
   cargo test session_telemetry_records_retry_attempts -- --exact --nocapture
   cargo test test_no_session_prevents_session_save -- --exact --nocapture

   Expected result: the success test reports both artifact files, the retry test finds retry metadata in the sidecar, and the existing `--no-session` contract still holds.

3. Optionally perform a manual run if a model is configured locally.

   CAKE_DATA_DIR=$(mktemp -d) cargo run -- "List the top-level files in this repository"
   ls "$CAKE_DATA_DIR/sessions"
   ls "$CAKE_DATA_DIR/session-telemetry"
   jq -c '. | {type, session_id, invocation_id, turn_index, attempt, total_ms, delay_ms, name, duration_ms, success}' "$CAKE_DATA_DIR"/session-telemetry/*.ndjson

   Expected result: the sessions directory contains one `.jsonl` transcript and the cache telemetry directory contains one `.ndjson` sidecar. The `jq` command prints a short timeline of the run. If local model configuration is unavailable, skip this step and rely on the automated tests.

4. Run the full project checks required by this repository.

   cargo fmt
   just ci

   Expected result: formatting is clean and the full CI recipe passes.

## Validation and Acceptance

Acceptance is behavioral rather than structural.

After implementation, a normal saved session must leave behind two artifacts with the same session identifier in different directories: the resumable transcript at `sessions/{uuid}.jsonl` and the telemetry sidecar at `session-telemetry/{uuid}.ndjson`. The sidecar must be valid newline-delimited JSON. It must contain enough records to explain the run without consulting the daily log: an init record, at least one API-attempt record, zero or more retry records, zero or more tool-call records, and one final session-summary record.

The retry acceptance case is important enough to be explicit. When a provider call fails with a retriable error and succeeds on a later attempt, the telemetry sidecar must show both the failed and successful attempts plus the retry delay and classification in between. A developer reading only the sidecar should be able to tell why the turn was slow.

The `--no-session` acceptance case is equally important. When the user opts out of session persistence, cake must not create either the transcript or the telemetry sidecar.

The transcript acceptance case must remain unchanged. `--resume` and `--continue` must still operate on `.jsonl` transcript files exactly as before. Telemetry files must never participate in latest-session discovery, explicit session loading, or stream-json resume behavior because they live outside the sessions directory and use their own extension.

## Idempotence and Recovery

The implementation steps are safe to repeat. The writer uses append mode, so repeated continues and resumes for the same session will add another invocation's telemetry to the same sidecar instead of overwriting previous runs. That is intentional, and the `invocation_id` field is what keeps those runs separable.

Because the sidecar lives under the cache tree, the implementation should not promise indefinite retention. A future cleanup command or retention policy can safely target `session-telemetry/` without touching resumable transcripts.

If a manual test needs a clean slate, prefer setting `CAKE_DATA_DIR` to a new temporary directory rather than deleting files from the default data directory. This keeps validation isolated and makes reruns deterministic.

If telemetry writing fails at runtime because the file cannot be opened or appended, cake should log the failure to the daily log once, disable telemetry for the rest of that invocation, and continue serving the user. The session transcript should still be saved if session persistence is enabled.

If an abrupt termination leaves a partially written last line in the sidecar, the damage is limited to that final event because the file is append-only and every prior record was flushed independently. The transcript remains the source of truth for resume behavior.

## Artifacts and Notes

The sidecar should look conceptually like this after a successful run with one API attempt and one tool call:

    {"type":"telemetry_init","session_id":"550e8400-e29b-41d4-a716-446655440000","invocation_id":"8c0f2cc6-2b4d-47fa-b4b0-3e1f91a57aa2","mode":"new","model":"glm-5.1","api_type":"responses","output_format":"text"}
    {"type":"api_attempt","session_id":"550e8400-e29b-41d4-a716-446655440000","invocation_id":"8c0f2cc6-2b4d-47fa-b4b0-3e1f91a57aa2","turn_index":1,"attempt":1,"status_code":200,"request_ms":684,"parse_ms":42,"total_ms":726,"history_items":2,"usage":{"input_tokens":1200,"output_tokens":250,"total_tokens":1450}}
    {"type":"tool_call","session_id":"550e8400-e29b-41d4-a716-446655440000","invocation_id":"8c0f2cc6-2b4d-47fa-b4b0-3e1f91a57aa2","turn_index":1,"call_id":"call_123","name":"Bash","duration_ms":118,"output_bytes":512,"was_error":false}
    {"type":"session_summary","session_id":"550e8400-e29b-41d4-a716-446655440000","invocation_id":"8c0f2cc6-2b4d-47fa-b4b0-3e1f91a57aa2","success":true,"duration_ms":1840,"turn_count":1,"usage":{"input_tokens":1200,"output_tokens":250,"total_tokens":1450}}

For a retrying run, the timeline should include an explicit retry record between attempts:

    {"type":"api_attempt","turn_index":1,"attempt":1,"status_code":429,"total_ms":910,"error":"429 rate limit"}
    {"type":"retry_scheduled","turn_index":1,"attempt":2,"reason":"rate_limit","delay_ms":2000,"detail":"429 rate limit"}
    {"type":"api_attempt","turn_index":1,"attempt":2,"status_code":200,"total_ms":701}

The recommended `jq` query for quick inspection is:

    jq -c '. | {type, invocation_id, turn_index, attempt, total_ms, delay_ms, name, duration_ms, success}' ~/.cache/cake/session-telemetry/{uuid}.ndjson

## Interfaces and Dependencies

Be prescriptive about the new interfaces so the implementation stays small and consistent.

In `src/session_telemetry.rs`, define a telemetry record enum and writer with stable, crate-local names:

    pub(crate) enum SessionTelemetryRecord {
        TelemetryInit { ... },
        ApiAttempt { ... },
        RetryScheduled { ... },
        ToolCall { ... },
        SessionSummary { ... },
    }

    pub(crate) struct SessionTelemetryWriter {
        ...
    }

    impl SessionTelemetryWriter {
        pub fn open(path: &Path) -> anyhow::Result<Self>;
        pub fn append(&mut self, record: &SessionTelemetryRecord) -> anyhow::Result<()>;
    }

Every record must include `session_id`, `invocation_id`, and a UTC timestamp. `ApiAttempt` records must include `turn_index`, `attempt`, `request_ms`, `parse_ms`, `total_ms`, `history_items`, `status_code` when known, `error` when present, `usage` when present, and a summary of the active `RequestOverrides`. `RetryScheduled` records must include the retry reason, delay, attempt number, and detail string from `RetryStatus`. `ToolCall` records must include the logical turn index, `call_id`, tool name, duration, output size in bytes, and a boolean that says whether the output came from the error path. `SessionSummary` must include overall success, final duration, turn count, and total usage.

In `src/config/data_dir.rs`, add a single path helper rather than duplicating path formatting:

    pub fn session_telemetry_path(&self, session_id: uuid::Uuid) -> PathBuf;

That helper should resolve to `self.get_cache_dir().join("session-telemetry").join(format!("{session_id}.ndjson"))`.

In `src/clients/agent.rs`, add an optional telemetry writer field and one builder method:

    pub fn with_session_telemetry(mut self, telemetry: SessionTelemetryWriter) -> Self;

Also add small internal helper methods that append records and disable telemetry after the first append failure. Do not introduce a general logging abstraction or trait hierarchy for this feature. A single optional writer owned by `Agent` is enough.

Preserve existing dependencies and reuse the ones already in the crate. Use `serde` and `serde_json` for NDJSON serialization, `chrono::Utc` for timestamps, `std::time::Instant` for duration measurement, and the existing retry types in `src/clients/retry.rs` for retry classifications. Do not add a new crate for logging or metrics.

Revision note: created the initial ExecPlan on 2026-05-02 to capture a session-scoped telemetry sidecar feature inspired by Amp-style per-thread timing logs while preserving cake's existing transcript and resume semantics.

Revision note: revised on 2026-05-02 to move telemetry storage from the `sessions/` directory into `~/.cache/cake/session-telemetry/` (or `$CAKE_DATA_DIR/session-telemetry/`) because these artifacts are operational logs and should not pollute the resumable session store.

Revision note: moved this active ExecPlan from `.agents/.plans/` to `.agents/exec-plans/active/` on 2026-05-07 during the ExecPlan directory migration.
