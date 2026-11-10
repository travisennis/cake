## Read-only session diagnostics and redacted export

This ExecPlan is a living document, maintained per docs/workflow/exec-plans.md. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be kept current as work proceeds.

## Purpose / Big Picture

Cake persists one session transcript, one optional telemetry sidecar, and date-rotated logs. Diagnosing a failed or slow task currently means opening those files by hand, finding the right invocation among resumed runs, and deciding which parts are safe to share. After this change, `cake debug session <UUID>` answers that question from one command.

A user can see the session's terminal state, whether a writer still holds it, the last activity, task boundaries, retries, slow operations, and the paths of the transcript, sidecar, and related logs. `--json` emits the standard versioned diagnostic document. `--export <PATH>` writes the same redacted document to a new owner-only local file. The command never writes the source session, resumes it, calls a provider, runs hooks or toolbox tools, sends anything off the machine, or reads prompts, tool output, hook text, provider payloads, or log bodies into the report.

## Progress

- [x] (2026-11-10) Read the current session, telemetry, debug, config, grant, and logging code after 077b and 077d landed.
- [x] Write this ExecPlan and start task 077f.
- [x] Add reusable session liveness probing and shared settings-load attempt.
- [x] Add the `debug session` command, report model, text renderer, JSON document, and export writer.
- [x] Reuse `ConfigReport` for a compact current-configuration context and `GrantReport` for its grant summary.
- [x] Add focused unit tests and `tests/debug_session.rs` fixtures for completed, interrupted, live, corrupt, missing-telemetry, resumed, malformed-tail, and export-redaction cases.
- [x] Update help, configuration, integration, and security documentation.
- [x] Run focused tests, strict clippy, `just check`, and docs checks. Final `just check` passed in 90.1s; focused suites, cc-check, import/dependency lint, and docs checks also passed.
- [x] Complete task 077f, archive this plan, commit, and push.

## Surprises & Discoveries

- Observation: telemetry records and transcript task records have no shared invocation identifier. Evidence: `TaskStartData` has `task_id`, while `SessionTelemetryRecord` has `invocation_id`; only some telemetry records carry both. The report therefore correlates by an observed telemetry `task_id` first and by nearest start timestamp second, and states which basis it used.
- Observation: `SessionTelemetryRecord` implements `Serialize` but not `Deserialize`. Evidence: `src/session_telemetry.rs` defines the enum only with `Serialize`. The diagnostic reads sidecars as framed `serde_json::Value` objects, which also keeps historical and unknown record types readable.
- Observation: a final valid JSON line without a newline is recoverable, while a newline-terminated malformed line is corruption. Evidence: `SessionFramer` and `Session::load` already make that distinction. The report preserves it.

## Decision Log

- Decision: add `cake debug session <UUID> [--json] [--export PATH] [--slow-ms MS]`; do not change `cake replay`, resume, fork, or session-record shapes. Rationale: replay is a stream re-emitter, while diagnosis is a read-only report over three artifact families. Date/Author: 2026-11-10, cake.
- Decision: correlate invocations to tasks by a telemetry-observed `task_id` when available, otherwise by nearest start time within 120 seconds, and expose `match` as `task_id`, `start_time`, or `null`. Rationale: no persisted identifier joins the two files, and a timestamp-only claim would be stronger than the data supports. Date/Author: 2026-11-10, cake.
- Decision: classify transcript and telemetry independently, then derive the top-level state. Transcript states are `ok`, `corrupt`, `unreadable`; telemetry states are `present`, `missing`, `corrupt`, `unreadable`. A corrupt or unreadable artifact is an error check and exits `1`; a missing sidecar or unknown liveness is a warning and exits `0`. Rationale: absence of optional evidence is not the same as invalid evidence, and no partial run is represented as complete. Date/Author: 2026-11-10, cake.
- Decision: detect a live writer with a non-blocking advisory lock attempt on a read-only handle. A failed probe reports `unknown`; it never blocks, waits, or writes. Rationale: the writer already holds the same advisory lock for the session lifetime, so this is the only local liveness signal without a second lock file. Date/Author: 2026-11-10, cake.
- Decision: the export is the same pretty-printed diagnostic document as `--json`, written `create_new` with mode `0600`; an existing target, a source artifact, or a symlink is refused. No bundle wrapper, prompt transcript, or raw log is added. Rationale: one redacted representation is easier to verify and share, and refusing overwrite protects source artifacts and prior exports. Date/Author: 2026-11-10, cake.
- Decision: reuse `ConfigReport` for a compact current-configuration context and `GrantReport` through its existing `summary` projection for grant counts. Omit the configured base URL, credential variable name, prompt path/body, hook text, and full grant path lists from that context. Rationale: the report is useful for comparing "now" with the recorded session, but it is not a historical snapshot, and the export must not gain a second disclosure surface. Date/Author: 2026-11-10, cake.
- Decision: `diagnosis_complete` is false for any unterminated final record, even when the final JSON object is valid; the partial-tail finding still exits `0` because the artifact remains readable. Rationale: the missing record boundary is evidence that the writer stopped before a normal close, and the documentation must not promise more than the file proves. Date/Author: 2026-11-10, cake.
- Decision: `session_summary` is not a slow operation. It reports the whole invocation duration, so including it would duplicate the task duration and crowd out the operations that explain where the time went. Date/Author: 2026-11-10, cake.
- Decision: no new ADR. The command uses ADR 029's diagnostic document, ADR 007's telemetry sidecar, and ADR 056's grant report; the additive CLI and export policy are documented in this plan and the durable docs. Date/Author: 2026-11-10, cake.

## Outcomes & Retrospective

Delivered. `cake debug session <UUID>` now reads one transcript, its optional telemetry sidecar, and the date-overlapping daily logs without resuming or mutating them. It reports the top-level session state (`completed`, `interrupted`, `live`, `unknown_liveness`, or `corrupt`), artifact states (including a missing sidecar), last activity, task boundaries and terminal subtypes, per-invocation telemetry summaries, retries, slow operations, and artifact paths. The current-configuration context is the compact `ConfigReport` reuse, and its grants are the existing `GrantReport` summary. `--json` is one deterministic diagnostic document; `--export` writes the same redacted document to a new owner-only local file and refuses an existing target, a symlink, a missing parent, or a source artifact.

Verification: `cargo test --test debug_session` passed 28 tests; the focused unit tests for the report and session liveness passed; `cargo test --test debug_config`, `debug_grants`, `debug_models`, `diagnostic_json`, and `exit_codes` passed; strict clippy passed; `just cc-check` reported 1,990 functions with zero over the allowed complexity; `just lint-imports` and dependency lint passed; and the final `just check` passed in 90.1s with 2,052 unit tests plus every integration and script suite. `panache format --check`, `panache lint`, and `just docs-check` passed for the four changed documents.

What remains: the `debugging-cake` skill update is a separate `cake-skills` task because this workspace forbids editing two project clones in one task. The new module is 2,536 production lines, which is informational under the existing module-size threshold and not a gate failure. No new ADR was needed; the command uses the existing diagnostic, telemetry, and grant contracts.

Lessons: the transcript and telemetry scans needed the same split between framing, classification, and payload construction to stay under the CC gate; extracting those boundaries also made the acceptance tests easier to target. Correlating by a telemetry-observed `task_id` first avoids overstating what start-time matching proves on resumed sessions.

## Context and Orientation

Cake is a Rust CLI with a persisted transcript at `~/.local/share/cake/sessions/{uuid}.jsonl` (or `{CAKE_DATA_DIR}/sessions/{uuid}.jsonl`) and an optional telemetry sidecar at `~/.cache/cake/session-telemetry/{uuid}.ndjson` (or `{CAKE_DATA_DIR}/session-telemetry/{uuid}.ndjson`). Logs are `cake.YYYY-MM-DD.log` files in the cache/data root. The transcript is the content surface and may contain prompts, assistant text, tool output, hook commands, and paths. Telemetry is bounded operational metadata. Logs may contain anything Cake logged.

`src/types/session.rs` defines the transcript records. `TaskStart` carries `task_id`, `timestamp`, `model`, and `model_config`; `TaskComplete` carries the outcome, duration, turn and tool counts, usage, and permission denials; `HookEvent` carries command/stdout/stderr and must not be copied into a report. `src/config/session_jsonl.rs` provides `SessionFramer`, the shared line framer that reports physical line numbers and a `partial_tail` flag.

`src/session_telemetry.rs` defines `SessionTelemetryRecord` and the writer. Records include `telemetry_init`, `api_attempt_in_flight`, `api_attempt`, `retry_scheduled`, `retry_wait`, `tool_call`, compensation events, gate records, and `session_summary`. Provider identifiers are already digests; provider text is already bounded; the endpoint is already sanitized; but the export must still omit those fields where they are not needed.

`src/config/session.rs` holds the session append lock. It uses `fs4::FileExt::try_lock` on the append handle for the lifetime of a run, so a non-blocking lock attempt on a read-only handle distinguishes an active writer from a released one without adding state.

`src/cli/debug/config_report.rs` builds the effective-configuration report from `LoadedSettings`, `ProjectTrustReport`, `HookFileSource`s, and `GrantReport`. `src/cli/debug/grants.rs` owns `build_report`, `summary`, `data`, and `flat_payload`. The session report embeds only the compact current context; the full reports keep their existing commands.

`src/cli/diagnostic.rs` owns the diagnostic document, status derivation, and check ids. `src/cli/sessions.rs` shows the existing read-only listing style and `SessionFramer` usage. `src/cli/replay.rs` is the precedent for read-only transcript failures and a typed error with an explicit exit code.

Terms used below: a *task* is one `task_start` through its terminal `task_complete` (or the absence of one); an *invocation* is one telemetry `invocation_id` group; a *partial tail* is an unterminated final line; a *redacted export* is the same diagnostic document, containing only counts, timings, states, and artifact paths, never prompts, tool output, hook text, provider payloads, credentials, or log bodies.

## Plan of Work

In `src/config/session.rs`, add a small `SessionLiveness` enum and `probe_session_liveness(path) -> SessionLiveness` that opens the file read-only, calls `FileExt::try_lock`, and maps `WouldBlock` to `Live`, success to `NotLive`, and any error to `Unknown`. It never retries and does not hold the lock after returning. In `src/cli/settings.rs`, extract `try_load_settings(options)` from `load_settings` so the session command can degrade when settings fail without printing a failure document; `load_settings` keeps its current behavior by wrapping the attempt with `report_failure`.

Add `src/cli/debug/session_report.rs`. It defines the local flags, parsed transcript and telemetry summaries, task and invocation rows, retry and slow-operation rows, artifact and log rows, a compact current-config context, the diagnostic document builder, the text renderer, and the export writer. Add `DebugSubcommand::Session` in `src/cli/debug.rs`, dispatch it, and add examples to the root and debug help. Add the check ids in `src/cli/diagnostic.rs` and export them from `src/cli/mod.rs`.

Expose only the compact reuse points from `src/cli/debug/config_report.rs`: make `ConfigReport` and `build` visible to the sibling module, add `session_context()` returning the safe current model, trust, tool-count, skill-count, and grant-summary fields, and add `session_checks()` returning settings warnings, the project-trust warning, and the unavailable-model warning. No second settings resolver or grant resolver is introduced.

The transcript scan validates the first record as `session_meta` with the current format version and matching session id, rejects a symlink or non-regular file, reads lines through `SessionFramer`, and classifies records without retaining content. It accumulates tasks, record counts, last activity, usage, and hook counts. A parse error on a partial tail is ignored and recorded; a parse error on a newline-terminated line is corruption. The telemetry scan does the same for `.ndjson`, groups by `invocation_id`, tolerates unknown record types, counts record kinds, extracts retry timing and operation durations, and never copies error text, gate evidence payloads, compensation details, prompts, or provider URLs.

Correlation runs after both scans. It prefers a telemetry-observed `task_id`; otherwise it pairs the nearest unmatched task start within 120 seconds. The report exposes the match basis. Task state derives from the terminal subtype, or from liveness when no terminal record exists. The top-level state is `corrupt` for an invalid transcript, otherwise `live`, `unknown_liveness`, `completed`, or `interrupted` in that order of precedence. `diagnosis_complete` is false when any required artifact is corrupt, unreadable, or ends in a partial tail.

The text renderer prints stable sections: Session, Liveness, Transcript, Telemetry, Last activity, Artifacts, Tasks, Invocations, Retries, Slow operations, Current configuration, Diagnostics, and Export. The JSON document uses `command: "debug session"`, a summary with state/liveness/task/invocation/retry/slow counts, and `data` with session identity, transcript, telemetry, last activity, tasks, invocations, retries, slow operations, artifacts, the compact current configuration (including its grant summary), and export metadata.

`--export <PATH>` renders the same document and writes it before stdout with `create_new`, `O_NOFOLLOW` where available, and mode `0600`. It refuses an existing path, a symlink, a non-file, a source session or telemetry path, or a missing parent. A successful export records the path in `data.export`; stdout still contains the normal text or JSON report. Errors are typed so invalid identity/path failures exit `3` and an operational export or incomplete diagnosis exits `1`.

Tests cover: a completed resumed session with two tasks and two invocations; an interrupted task without a terminal record; a live writer holding the append lock; unknown liveness mapping at the report level; malformed partial tails versus interior corruption; a missing sidecar; alternate `CAKE_DATA_DIR`; deterministic JSON; no source modification; export creation and refusal; and canaries in the session header, system prompt, prompt context, message, tool output, hook command/stdout, provider base URL, summary error, gate evidence, log body, and credential value. The canaries must not appear in stdout, the exported file, checks, or errors.

Update `docs/configuration.md` with the command and `--slow-ms`/`--export` behavior; `docs/integrations.md` with the document payload, state vocabulary, correlation basis, exit codes, and artifact layout; and `docs/security.md` with the read-only, no-upload, redaction, symlink, and liveness-probe rules. Add the command to the read-only inspection-command lists. The `debugging-cake` skill companion remains a separate cake-skills task because this workspace forbids editing two project clones in one task.

## Concrete Steps

All commands run from `/Users/travisennis/Projects/cake/cake`.

Focused tests while iterating:

```
cargo test --bin cake cli::debug::session_report
cargo test --test debug_session
cargo test --test diagnostic_json
```

Full gate before handoff:

```
just check
panache format --check docs/configuration.md docs/integrations.md docs/security.md
panache lint docs/configuration.md docs/integrations.md docs/security.md
just docs-check
```

Manual smoke test with an isolated data root:

```
CAKE_DATA_DIR=/tmp/cake-debug-session-data cake debug session <UUID>
CAKE_DATA_DIR=/tmp/cake-debug-session-data cake debug session <UUID> --json
CAKE_DATA_DIR=/tmp/cake-debug-session-data cake debug session <UUID> --export /tmp/session-diagnostic.json
```

## Validation and Acceptance

A reviewer can see the command working when a fixture session produces:

```
Session diagnostics
Session: 550e8400-e29b-41d4-a716-446655440000
State: completed
Liveness: not held
Transcript: /.../sessions/550e8400-e29b-41d4-a716-446655440000.jsonl (...)
Telemetry: present (...)
Tasks
  ...
```

and `--json` produces one document whose `command` is `debug session`, whose `summary.state` is `completed`, and whose `data.tasks` has one row per task start. A held lock produces `state: live`; an unterminated task without a lock produces `interrupted`; a missing sidecar produces `telemetry.state: missing` with a warning; an interior malformed line produces `status: error` and exit `1`; a final malformed partial line is tolerated and reported as a partial tail. An export contains the same bytes as stdout JSON, is mode `0600` on Unix, and contains none of the synthetic canaries. The source transcript and sidecar bytes are unchanged.

## Idempotence and Recovery

Reading is idempotent. The only write is `--export`, which refuses an existing target, so a retry after a successful export requires a new path or removing the prior export. A failed export leaves no partial file because the content is rendered before creation and the write is flushed and closed before stdout is emitted. Reverting the source change leaves all existing artifacts unchanged; the command only adds a read-only consumer.

## Artifacts and Notes

Pending. Record the focused test output and the `just check` tail here before task completion.

## Interfaces and Dependencies

New in `crate::cli::debug::session_report`: `pub(super) fn run_session(uuid: &str, json: bool, export: Option<&Path>, slow_ms: u64, data_dir: &DataDir, options: &CommandRunOptions<'_>) -> anyhow::Result<()>`, the report data types, and the typed `SessionDiagnosticError` exported through `crate::cli`. New in `crate::config::session`: `pub enum SessionLiveness` and `pub fn probe_session_liveness(path: &Path) -> SessionLiveness`. New in `crate::cli::settings`: `pub fn try_load_settings(options) -> anyhow::Result<SettingsLoad>`. `ConfigReport::session_context()` and `ConfigReport::session_checks()` are `pub(super)`. The command uses `SessionFramer`, `DiagnosticDocument`, `ConfigReport`, `GrantReport::summary`, `chrono`, and `serde_json`; it adds no dependency. It reads `fs4` only through the existing session module. The export writer uses the existing `owner_only` helpers and `O_NOFOLLOW`.

## Revision Notes

- 2026-11-10: initial plan.
