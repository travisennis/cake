# Refactor Session Management to Append-Only Task Events

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This plan follows `.agents/PLANS.md` from the repository root. A future contributor should read that file if changing the plan format, but this document is otherwise self-contained and describes the required behavior, code locations, tests, and migration decisions needed to complete the refactor.

## Purpose / Big Picture

Cake session files should become append-only logs again. A session file should be easy to reason about: it starts once with durable session metadata, then each user task appends a start event, conversation records, and a completion event. Continuing, resuming, and forking should no longer rewrite or normalize the whole file.

After this change, a user can run `cake "first task"` and then `cake --continue "second task"` and inspect `~/.local/share/cake/sessions/{uuid}.jsonl`. The first line will be one `session_meta` record. Each invocation will append one `task_start` record and one `task_complete` record around the records produced by that task. `--output-format stream-json` will output live task events beginning with `task_start`; it will not output `session_meta`, and its output is allowed to be a subset of the persisted session file. `--resume` and `--fork` will accept only session UUIDs, not arbitrary file paths.

Cake is pre-release software with a single user. This refactor is a clean break: legacy v2 and old-v3 (`init`/`result`) session files will no longer load, and the persisted JSONL schema bumps to `format_version: 4`. Stream-json consumers must adapt to the new event names.

## Progress

- [x] (2026-05-03T15:44Z) Captured the initial user requirements and inspected the current session implementation in `src/clients/types.rs`, `src/clients/agent.rs`, `src/config/session.rs`, `src/config/data_dir.rs`, `src/main.rs`, and `docs/design-docs/session-management.md`.
- [x] (2026-05-03T15:44Z) Created this initial ExecPlan with concrete design decisions, affected files, milestones, and validation strategy.
- [x] (2026-05-03T18:30Z) Revised the plan after review: added separate `StreamRecord` type, removed all backward-compatibility scope, pinned `task_id` ownership, decided on advisory file locking, slimmed `task_start`, made directory-mismatch fatal for `--continue`, switched validation to mocked tests, and noted snapshot updates.
- [x] (2026-05-03T19:15Z) Second review pass: fork copies parent records into a new file, live-append from agent through a persist sink, switched lock crate to `fs4`, renamed `ResultSubtype` to `TaskCompleteSubtype`, standardized emitter method suffix to `_record`, added `#[serde(default)]` to `cake_version`, specified `format_version` mismatch error, and clarified that stream-callback type becomes `StreamRecord`.
- [x] (2026-05-03T22:05Z) Defined `SessionRecord` (persisted) and `StreamRecord` (streamed) schemas with `format_version: 4`.
- [x] (2026-05-03T22:05Z) Implemented append-only session creation and saving with advisory file lock via `fs4`.
- [x] (2026-05-03T22:05Z) Removed file-path resume/fork support from CLI behavior, code, and docs.
- [x] (2026-05-03T22:05Z) Updated stream-json output so it starts at `task_start` and never emits `session_meta`.
- [x] (2026-05-03T22:05Z) Updated session loading to read append-only multi-task logs and restore only conversation history for model context.
- [x] (2026-05-03T22:05Z) Deleted legacy v2 and old-v3 loading paths and their tests.
- [x] (2026-05-03T22:05Z) Added regression tests for append-only loading/appending, stream/persist fan-out, path rejection, exit classification, and file locking.
- [x] (2026-05-03T22:12Z) Checked snapshot-backed type tests with `cargo test types`; `cargo insta test` could not run because the `cargo-insta` subcommand is not installed.
- [x] (2026-05-03T22:05Z) Updated design docs, stream-json docs, CLI reference note, and changelog.
- [x] (2026-05-03T22:18Z) Ran `just ci` with localhost binding allowed for wiremock tests; all checks passed.

## Surprises & Discoveries

- Observation: The current code documents session files as no longer append-only and rewrites full files on every save.
  Evidence: `docs/design-docs/session-management.md` says "Session files are no longer append-only" and `src/config/session.rs::Session::save` creates a temporary file and renames it over the old session file.

- Observation: The current `SessionRecord` type is shared by persisted sessions and stream-json output, which conflicts with the new requirement that persisted session metadata should not be emitted to stream-json.
  Evidence: `src/clients/types.rs` describes `SessionRecord` as the schema for both `--output-format stream-json` and session history files.

- Observation: File-path resume/fork is implemented in `src/main.rs` by treating non-UUID values as paths, and in `src/config/data_dir.rs` via `load_session_from_path`. Only `src/main.rs::build_client_and_session` and the re-export in `src/config/mod.rs` use it; no tests reference it.
  Evidence: `rg -n load_session_from_path src tests` returns four hits, all in `src/main.rs`, `src/config/mod.rs`, and the function definition itself in `src/config/data_dir.rs`.

- Observation: Existing snapshot tests embed the JSON tags `init` and `result` and the v2/legacy-v3 fixture files; they will need refreshed snapshots and deletions when the schema changes.
  Evidence: `src/config/session.rs` contains `test_session_v2_backward_compat`, `test_session_v2_duplicate_timestamp_compat`, `test_session_result_record_stripped_on_load`, and `test_session_save_writes_v3` which all assert legacy behavior.

- Observation: The current code only enforces the working-directory match for path-based resume/fork (`ensure_session_directory_matches`); UUID-based `--resume` and `--fork` deliberately skip the check, and `--continue` is implicitly directory-scoped because `load_latest_session` filters by working directory.
  Evidence: `src/main.rs` calls `ensure_session_directory_matches` only inside the non-UUID branches of `--resume` and `--fork`.

- Observation: The first `cargo test session --quiet` after adding `fs4` failed inside the sandbox because Cargo needed crates.io index access for the new dependency. Rerunning with approved network access resolved dependencies and surfaced compile errors normally.
  Evidence: Cargo reported `Couldn't resolve host: index.crates.io` before escalation.

- Observation: Focused `cargo test agent --quiet` fails in the sandbox because existing `wiremock` tests cannot bind localhost ports there. The same test set passes when run with escalated permissions.
  Evidence: Wiremock reported `Failed to bind an OS port for a mock server: Operation not permitted`; full `cargo test --quiet` later passed with escalation.

- Observation: The `cargo-insta` CLI is not installed in this environment, so `cargo insta test` cannot run.
  Evidence: Cargo reported `error: no such command: insta`. The existing snapshot tests covered by `cargo test types --quiet` pass.

## Decision Log

- Decision: Treat persisted session JSONL as an append-only session log, not a canonical rewritten snapshot.
  Rationale: The user explicitly wants append-only session files and noted that rewriting `init` and `result` records undermines the simplicity of JSONL as a log format.
  Date/Author: 2026-05-03 / Codex

- Decision: Split persisted session metadata from stream-json output by introducing separate `SessionRecord` and `StreamRecord` types in `src/clients/types.rs`.
  Rationale: A separate `StreamRecord` type makes "never stream `session_meta`" a compile-time invariant rather than a runtime convention. `SessionRecord` keeps all variants that exist on disk; `StreamRecord` is a strict subset (`TaskStart`, `Message`, `FunctionCall`, `FunctionCallOutput`, `Reasoning`, `TaskComplete`). Conversation variants share their inner field structures via shared structs or by reusing `ConversationItem` so duplication stays minimal.
  Date/Author: 2026-05-03 / Codex (revised after review)

- Decision: Replace `result` with `task_complete`, and add `task_start` to mark each user task boundary.
  Rationale: A session can contain multiple user tasks. The old single trailing `result` concept fits a snapshot model, while `task_start` and `task_complete` fit an append-only log with repeated invocations.
  Date/Author: 2026-05-03 / Codex

- Decision: Remove support for `--resume <path>` and `--fork <path>`; only UUID-based restore is supported.
  Rationale: Redirected stream-json is no longer intended to be a complete resumable session file. Removing path loading simplifies session invariants and avoids ambiguous current-directory validation.
  Date/Author: 2026-05-03 / Codex

- Decision: Keep `--continue` directory-scoped via `load_latest_session`. Make a working-directory mismatch fatal for `--continue` rather than silently producing "no session found": when the user passes `--continue` and the latest session for any directory is the one they intend, but its `working_directory` no longer matches `current_dir`, return a clear error explaining the mismatch.
  Rationale: The user wants `--continue` to refuse, not silently succeed or silently start a new session. Existing UUID-based `--resume` and `--fork` deliberately skip the directory check; the user signaled the existing guards are sufficient there, so this refactor does not change UUID-resume/fork directory behavior.
  Date/Author: 2026-05-03 / Codex (revised after review)

- Decision: Make a clean break with no backward compatibility for v2 or old-v3 (`init`/`result`) session files.
  Rationale: Cake is pre-release with a single user. Maintaining migration code adds complexity that benefits no one. Bump `format_version` to `4`. Loading any file whose first record is not `session_meta` returns a clear error. Delete `load_format_v2`, the v2/legacy-v3 fixtures, and `test_session_result_record_stripped_on_load`.
  Date/Author: 2026-05-03 / Codex (added after review)

- Decision: Generate `task_id` (UUIDv4) once in `src/main.rs::run` at the start of each invocation. Thread it explicitly into `Agent` (stored on `Agent` for the lifetime of the run) and into the save layer. Conversation records (`Message`, `FunctionCall`, `FunctionCallOutput`, `Reasoning`) do **not** carry `task_id`; only `TaskStart` and `TaskComplete` do. Consumers correlate records to a task by position between matching `task_start`/`task_complete` pairs.
  Rationale: Single source of truth keeps the agent stateless about session identity beyond the current task and prevents drift. Bloating every conversation record with `task_id` is unnecessary because file order already correlates them.
  Date/Author: 2026-05-03 / Codex (added after review)

- Decision: `session_meta` carries durable session-wide context (`format_version`, `session_id`, `timestamp`, `working_directory`, `model`, `tools`, optional `cake_version`). `task_start` carries only per-task context (`session_id`, `task_id`, `timestamp`). Working directory, model, and tools are not duplicated on `task_start`; if any of these change between tasks, the change is reflected only by writing a new `session_meta` (which is not done in this refactor; the value is fixed for the life of a session).
  Rationale: Avoids the "which record wins" ambiguity. A session is a single (working_dir, model) tuple; if the user changes directory or model materially, that is a new session, enforced by the `--continue` directory check.
  Date/Author: 2026-05-03 / Codex (added after review)

- Decision: Use advisory exclusive file locking around session-file appends via the `fs4` crate (the maintained successor to the abandoned `fs2`). Acquire `try_lock_exclusive` on the open session file at the start of an invocation; on failure return a clear error: `Another cake invocation is currently writing to session <id>. Wait for it to finish or run in a different directory.`
  Rationale: With append-only files, concurrent invocations on the same session file would interleave records past `PIPE_BUF` boundaries on long writes. Cost is one small dependency and one syscall per invocation. Lock is automatically released on file close. `fs2` is unmaintained and pulls in a stale `libc`; `fs4` provides the same API with active maintenance.
  Date/Author: 2026-05-03 / Codex (added after review)

- Decision: When `--continue`, `--resume`, and `--fork` are combined with `--output-format stream-json`, do not replay prior tasks to stdout. The stream begins with `task_start` for the *current* invocation, includes only the current task's records, and ends with `task_complete`.
  Rationale: Stream-json is a live progress feed for the current invocation, not a session-file dump. Prior task replay would surprise consumers and dilute the meaning of "stream."
  Date/Author: 2026-05-03 / Codex (added after review)

- Decision: Validation strategy splits along automation boundaries. Unit and integration tests use mock model clients (no network). The CLI command examples in this plan that invoke a real model are manual smoke tests for the implementer; CI relies solely on `just ci`. The directory-mismatch error and the path-rejection errors are validated as **unit tests** against the underlying helper functions, not via subprocess CLI tests, to avoid adding test-only mock-injection plumbing to `main.rs`.
  Rationale: Keeps CI fast, deterministic, and offline. The user explicitly rejected adding mock-client complexity to the CLI itself.
  Date/Author: 2026-05-03 / Codex (added after review; refined 2026-05-03 second pass)

- Decision: Fork semantics copy the parent's records into the new fork file. A `--fork` invocation creates a brand-new session UUID and a new file under `~/.local/share/cake/sessions/<new-uuid>.jsonl`, writes a fresh `session_meta` (with the new `session_id`, current `working_directory`, current `model`, current `tools`, current timestamp, current `cake_version`), then copies the parent's conversation records (`Message`, `FunctionCall`, `FunctionCallOutput`, `Reasoning`) into the new file in their original order. Parent `task_start` and `task_complete` boundary records are **not** copied; the fork begins its own task history. After the parent's records are seeded, the fork's first `task_start` for the current invocation is appended, conversation records are appended live, and `task_complete` closes it. Resuming the fork later by UUID will see the parent's history followed by all fork-local tasks, with no reference back to the parent.
  Rationale: Self-contained fork files mean a fork is fully resumable on its own without chasing parent links. Stripping parent task boundaries avoids confusing replay semantics where fork files contain start/complete pairs that never ran in this session.
  Date/Author: 2026-05-03 / Codex (added after second review pass)

- Decision: Conversation records are appended to the session file **live** as the agent emits them, not batched at task end. The orchestrator passes a "persist sink" (a closure or trait object that wraps the locked, append-mode file handle and writes one JSONL line per call) to the agent alongside the existing stream callback. The agent fans out each emitted record to both sinks: the persist sink lands it on disk, the stream callback lands it on stdout when `--output-format stream-json` is enabled. After the agent loop ends, the orchestrator emits `task_complete` through the same fan-out so it lands on disk and (if streaming) on stdout. The file handle is dropped at the very end, releasing the lock.
  Rationale: Live append matches the append-only crash-recovery promise in `Idempotence and Recovery`: a crash mid-task leaves the conversation records that were fully written on disk, not just `task_start`. This requires the agent to know about the persist sink, but it is the same fan-out pattern as the existing stream callback so the structural change is small.
  Date/Author: 2026-05-03 / Codex (added after second review pass)

- Decision: Rename `ResultSubtype` to `TaskCompleteSubtype` (the variant names `Success`, `ErrorDuringExecution`, `ErrorMaxTurns` are kept). Standardize all emitter method suffixes on `_record`: the new methods are `emit_task_start_record` and `emit_task_complete_record`; no `_message` suffix variant is retained.
  Rationale: Consistent naming with the new variant names; `_record` is preferred over `_message` because `Message` is itself a record variant and the overload is confusing.
  Date/Author: 2026-05-03 / Codex (added after second review pass)

- Decision: Specify `format_version` mismatch handling explicitly. `Session::load` reads `session_meta.format_version` from the first record. If it is anything other than `4`, fail with `Unsupported session format_version: <n> (expected 4). Session file: <path>`. No migration logic is attempted.
  Rationale: Pre-release single-user software means we can be strict; explicit error is better than silent acceptance of unknown variants.
  Date/Author: 2026-05-03 / Codex (added after second review pass)

## Outcomes & Retrospective

- Implemented the v4 append-only session log model end to end. New sessions write one `session_meta`; each invocation emits `task_start`, live conversation records, and `task_complete`; continue/resume append to existing files; fork creates a new file and seeds only parent conversation records.
- Split persisted `SessionRecord` from streamed `StreamRecord`, so `session_meta` cannot be emitted to stream-json. Removed the agent's stream buffer and replaced it with live fan-out to persistence and optional stdout streaming.
- Removed path-based resume/fork loading and legacy v2/old-v3 load paths. `--resume` and `--fork` now reject non-UUID arguments with input errors.
- Validation: `cargo test types --quiet`, `cargo test config::session --quiet`, `cargo test --test exit_codes --quiet`, `cargo clippy --all-targets --all-features -- -D warnings`, escalated `cargo test --quiet`, and final escalated `just ci` all pass. `cargo insta test` could not run because `cargo-insta` is not installed; snapshot-backed tests still pass under normal cargo test.

## Context and Orientation

Cake is a Rust 2024 CLI application. The session system stores conversation history so users can continue, resume, or fork prior conversations. A "session" is a long-lived conversation identified by a UUID. A "task" is one CLI invocation inside a session: one user prompt, the model/tool loop that handles it, and a final success or error status for that invocation.

The current implementation uses `SessionRecord` in `src/clients/types.rs` as both the persisted JSONL record type and the stream-json stdout record type. Current variants include `Init`, `Message`, `FunctionCall`, `FunctionCallOutput`, `Reasoning`, and `Result`. `Init` is intended to be first in the file, and `Result` is intended to be the trailing completion metadata for the last run.

The current save path is snapshot-based. `src/main.rs::run` builds an `Agent` and a `Session`, calls `client.emit_init_message()`, calls `client.send(msg).await`, calls `client.emit_result_message(...)`, drains the agent stream into `session.records`, and then calls `DataDir::save_session`. `DataDir::save_session` calls `Session::save`, which rewrites the whole file through a temporary file. On restore, `Session::load` strips a trailing `Result` because that result is treated as previous-run metadata, not model context.

The desired implementation is log-based. A new session file is created with one `session_meta` record at the top. Each CLI invocation appends a `task_start` record, appends conversation records as the agent runs, and appends a `task_complete` record. Restored sessions keep all prior task boundary records in the file, but only conversation records are converted back into model conversation history.

`stream-json` is machine-readable stdout produced when the user passes `--output-format stream-json`. Under the new design, stream-json is a live task stream, not a session-file dump. It includes `task_start`, conversation records, and `task_complete` for the current invocation only. It does not include `session_meta`, and it does not replay prior tasks under `--continue`/`--resume`/`--fork`.

The main code locations are:

- `src/clients/types.rs`: defines `SessionRecord`, will additionally define `StreamRecord`, `ResultSubtype`, conversion to/from `ConversationItem`, and streaming JSON conversion.
- `src/clients/agent.rs`: holds the in-memory stream buffer and emits init/result records today; will hold a `task_id` and emit task-scoped stream records.
- `src/config/session.rs`: loads and saves session files; will gain append-only semantics and lose v2/legacy-v3 support.
- `src/config/data_dir.rs`: chooses session paths, loads latest sessions, loads by UUID, and currently exposes path-based loading; the latter will be removed.
- `src/main.rs`: parses CLI flags and orchestrates new, continue, resume, fork, send, stream, and save behavior; will generate `task_id` and reject path-based resume/fork.
- `docs/design-docs/session-management.md`: describes session behavior and must be rewritten to match this design.
- `docs/design-docs/streaming-json-output.md`: describes stream-json output and must be rewritten to remove the old "same schema as session history" claim.

## Plan of Work

Milestone 1 defines the new record schemas and bumps `format_version` to `4`. In `src/clients/types.rs`:

- Rename the persisted metadata variant from `Init` to `SessionMeta` using serde rename `session_meta`. Fields: `format_version: u32`, `session_id: String`, `timestamp: DateTime<Utc>`, `working_directory: PathBuf`, `model: Option<String>`, `tools: Vec<String>`, and an optional `cake_version: Option<String>` populated from `env!("CARGO_PKG_VERSION")` at write time.
- Add `TaskStart` with fields `session_id: String`, `task_id: String`, `timestamp: DateTime<Utc>`. No working_directory/model/tools.
- Add `TaskComplete` with fields equivalent to today's `Result` plus `task_id`: `subtype`, `success`, `is_error`, `duration_ms`, `turn_count`, `num_turns`, `session_id`, `task_id`, `result`, `error`, `usage`, `permission_denials`. Serde tag `task_complete`.
- Define `StreamRecord` as a separate enum with variants `TaskStart`, `Message`, `FunctionCall`, `FunctionCallOutput`, `Reasoning`, `TaskComplete`. Reuse the same inner field shapes by extracting shared structs or by holding a `ConversationItem` for the conversation variants. Provide `From<StreamRecord> for SessionRecord` so the agent can emit a record once and have both the stream callback and the persisted buffer consume it.
- Bump `CURRENT_FORMAT_VERSION` to `4` in `src/config/session.rs`.

Milestone 2 changes session files to append-only with a file lock. Replace `Session::save` with append-oriented operations:

- Add `Session::create_on_disk(path: &Path, meta: &SessionRecord) -> anyhow::Result<File>` that fails if the file already exists, writes one `session_meta` line, flushes, and returns the locked, append-mode handle.
- Add `Session::open_for_append(path: &Path) -> anyhow::Result<File>` that opens an existing session file in append mode and acquires `fs4::FileExt::try_lock_exclusive`. On lock failure, return the error message specified in the Decision Log.
- Add `Session::append_record(file: &mut File, record: &SessionRecord) -> anyhow::Result<()>` (singular, called once per record for live append) and `Session::append_records(file: &mut File, records: &[SessionRecord])` as a thin loop wrapper for batch use cases (e.g., seeding fork files). Each call writes one JSONL line and flushes. The lock is held for the lifetime of the file handle and released on drop.
- Remove `Session::save`'s temp-file-and-rename path entirely. If the existing `Session::save` becomes unused after the orchestrator switches to live-append, delete it; otherwise keep only what callers still need.
- Add `fs4` (current stable version) to `Cargo.toml`.

The orchestrator in `src/main.rs::run` opens the file once at the start of an invocation. For a brand-new session it calls `create_on_disk` with the freshly built `session_meta`. For continue/resume it calls `open_for_append` on the existing file. For fork it creates a new file with `create_on_disk(new_meta)`, then calls `append_records(parent_conversation_records)` to seed the parent history. In all cases it then writes `task_start` via `append_record`, hands the locked file handle to the agent as a "persist sink" (a `Box<dyn FnMut(&SessionRecord) -> anyhow::Result<()>>` that wraps `append_record`), runs the agent loop (each emitted record is appended live), then writes `task_complete` through the same sink, and finally drops the file handle to release the lock.

Milestone 3 separates task events from session metadata in the agent and stream-json path. In `src/clients/agent.rs`:

- Store the current `task_id: String` on the `Agent` struct, set when the agent is constructed for an invocation.
- Replace `emit_init_message` with `emit_task_start_record`. It emits a `StreamRecord::TaskStart` through the fan-out (persist sink + stream callback). The stream callback type changes from `Fn(SessionRecord)` to `Fn(StreamRecord)` so streaming `session_meta` is impossible at compile time.
- Replace `emit_result_message` with `emit_task_complete_record`, doing the same for `TaskComplete`.
- The agent does not know about `session_meta`. Session persistence code in `src/main.rs::run` constructs and writes it when a new session file is created or when forking.
- The agent gains a `persist_sink: Option<Box<dyn FnMut(&SessionRecord) -> anyhow::Result<()>>>` field (or equivalent) wired in from the orchestrator. Each emitted record fans out to both the persist sink (always) and the stream callback (only when `--output-format stream-json` is enabled). Conversion between `StreamRecord` and `SessionRecord` uses the `From<StreamRecord> for SessionRecord` impl.
- Delete `with_stream_records` entirely. With live-append, there is no in-memory stream buffer to seed and no end-of-task drain. Restored conversation history loads into `history` through `with_history` and prior persisted records on disk are not touched.

Milestone 4 removes path-based resume/fork. In `src/main.rs`:

- Audit `looks_like_uuid` in `src/config/data_dir.rs` before making it the sole gatekeeper. Confirm it accepts standard v4 UUIDs in any case (lowercase and uppercase) and only rejects values that are clearly not UUIDs. If the audit reveals the function is too strict, prefer `uuid::Uuid::parse_str` directly.
- Change `--resume` handling so the argument must parse as a UUID via `looks_like_uuid`. On non-UUID input, return `Invalid session reference '<value>': resume by file path is no longer supported. Provide a session UUID.`
- In `--fork`, keep `--fork` with no value for latest-session fork and `--fork <uuid>` for a specific session. Reject non-UUID non-empty values with the equivalent message.
- Make `--continue` fail loudly when `load_latest_session` returns `None` because the latest existing session has a different `working_directory` than `current_dir`. Implement this by adding a sibling `load_latest_session_any_directory` (or a richer return type) that distinguishes "no session at all" from "newest session is for another directory" and emit `Cannot continue: latest session was created in '<other_dir>' but current directory is '<current_dir>'. Run from the original directory or start a new session.`
- Remove `load_session_from_path` from `src/config/data_dir.rs` and its re-export in `src/config/mod.rs`.
- Remove `ensure_session_directory_matches` from `src/main.rs` after path loading is gone (no UUID branch needs it; UUID resume/fork keeps current behavior of skipping the directory check, per Decision Log).
- Run `rg -n load_session_from_path src tests` after the change to confirm no references remain.

Milestone 5 updates restore behavior for append-only multi-task logs. In `src/config/session.rs`:

- Rewrite `Session::load` to expect `session_meta` as the first record. Fail with a clear "Unsupported or legacy session file format" error if the first record is anything else. No v2 or legacy-v3 path.
- Read all subsequent records into `records`. Loading must tolerate a final partial record (incomplete final line) and a trailing `task_start` with no matching `task_complete`; the resulting in-memory session simply contains those records as-is.
- `Session::messages()` returns conversation items derived from `Message`, `FunctionCall`, `FunctionCallOutput`, `Reasoning` records only; `task_start` and `task_complete` are skipped by `to_conversation_item` as they do today for `Init`/`Result`.
- Update `load_latest_session` to read the first-line `session_meta` (only) for `working_directory` matching. It must not parse the entire file just for directory matching.
- Delete `load_format_v2`, `test_session_v2_backward_compat`, `test_session_v2_duplicate_timestamp_compat`, and `test_session_result_record_stripped_on_load`.

Milestone 6 updates tests. All new tests use mocks; none requires network access.

- `src/config/session.rs`: add tests proving (a) a new file starts with one `session_meta`; (b) appending a second task does not add another `session_meta` and does append a new `task_start`/`task_complete` pair; (c) loading a file with two complete tasks returns conversation history from both tasks while skipping `task_start`/`task_complete` for model context; (d) loading a file with a trailing `task_start` and no `task_complete` succeeds and returns the partial-task records; (e) loading a file whose first record is not `session_meta` returns a clear error; (f) loading a file whose `session_meta.format_version` is not `4` returns the explicit "Unsupported session format_version" error.
- `src/clients/agent.rs`: add tests proving the persist sink and stream callback both receive `task_start` first and `task_complete` last for a single invocation, and that the stream callback's `StreamRecord` enum has no `SessionMeta` variant (compile-time check; one test that constructs a `StreamRecord` for each variant suffices). Use stub sinks that capture emitted records.
- Fork-copy test: build an in-memory parent `Session` with a mix of `Message`, `FunctionCall`, `FunctionCallOutput`, `Reasoning`, `task_start`, and `task_complete` records, run the fork seeding routine into a new file, and assert the new file contains exactly: one `session_meta` (with the new session id), then the parent's conversation records in order, with no `task_start`/`task_complete` records copied from the parent.
- Unit tests against the helper functions used by `--resume`, `--fork`, and `--continue` (no subprocess CLI tests): assert non-UUID `--resume` and `--fork` arguments produce the new "no longer supported" error, and assert the directory-mismatch helper produces the new error message.
- File-lock test: open a session file, acquire the lock, then attempt to open it again from a second handle; the second `try_lock_exclusive` must fail, and the user-facing error message must be propagated.
- **Insta snapshots**: many existing snapshot tests in `src/clients/types.rs` and elsewhere encode `init`/`result` JSON tags. After implementation, run `cargo insta test` then `cargo insta review` and accept the renamed snapshots. Verify in review that no accepted snapshot still contains `"type":"init"` or `"type":"result"`.

Milestone 7 updates documentation.

- Rewrite `docs/design-docs/session-management.md` to describe append-only files, `session_meta`, per-task events, UUID-only resume/fork, the directory-mismatch behavior of `--continue`, the file-lock behavior, and the fact that stream-json is no longer a complete session file.
- Rewrite `docs/design-docs/streaming-json-output.md` so it specifies stream-json starts with `task_start`, ends with `task_complete`, includes live conversation records, never includes `session_meta`, does not replay prior tasks on `--continue`/`--resume`/`--fork`, and is not resumable by path.
- Update references in `docs/references/responses-api.md`, `docs/design-docs/cli.md`, and any other doc that still claims `init` or `result` are stream-json events.
- Update `CHANGELOG.md` with a "Breaking changes" entry: schema bumped to v4, legacy session files no longer load, `--resume <path>` and `--fork <path>` removed, stream-json record names changed.

## Concrete Steps

Start from the repository root:

    cd /Users/travisennis/Projects/cake

Run the current test suite before editing to establish a baseline:

    just ci

Expect the command to complete successfully before the refactor. If it fails because of unrelated current work, record the failure in `Surprises & Discoveries` with the exact failing command and a short excerpt. Do not hide a pre-existing failure.

Inspect the core files before each milestone with anchored patterns:

    rg -n '\b(Init|Result)\b|"type":"(init|result|session_start)"|load_session_from_path|with_stream_records|emit_(init|result)_message' src tests docs

Implement the milestones in order. After each milestone, run a focused test command before moving on:

    cargo test session
    cargo test agent
    cargo test exit_codes

After Milestone 6, refresh snapshots:

    cargo insta test
    cargo insta review

At the end, run the full project check:

    just ci

Update this `Progress` section after every completed milestone and add any design changes to `Decision Log`.

## Validation and Acceptance

Acceptance is automated via `just ci` plus the unit and integration tests defined in Milestone 6. The CLI invocations below are **manual smoke tests** the implementer runs with a real model after CI passes. They are not gating for completion.

For a new session, run cake with a temporary data directory:

    CAKE_DATA_DIR=/tmp/cake-session-test cake --no-color "Say hello"

Inspect the single session file under `/tmp/cake-session-test/sessions`. Its first line must have `"type":"session_meta"`. It must contain exactly one `session_meta`. It must contain a `task_start` before the user message and a `task_complete` after the conversation records.

For a continued session, run:

    CAKE_DATA_DIR=/tmp/cake-session-test cake --continue "Say hello again"

Inspect the same session file. It must still contain exactly one `session_meta`. It must now contain two `task_start` records and two `task_complete` records. The file should have grown by appending lines; the first task's lines should remain in their original order and should not be duplicated.

For stream-json under `--continue`, run:

    CAKE_DATA_DIR=/tmp/cake-session-test cake --continue --output-format stream-json "Say hello once more" > /tmp/cake-stream.jsonl

Inspect `/tmp/cake-stream.jsonl`. The first streamed line must have `"type":"task_start"`. No line may have `"type":"session_meta"`. The final streamed line must have `"type":"task_complete"`. The file must contain only the current task's records, not any prior task's records.

For removed path loading, run:

    cake --resume /tmp/cake-stream.jsonl "continue"

The command must fail with a clear message that file-path resume is no longer supported and session UUIDs are required. Similarly:

    cake --fork /tmp/cake-stream.jsonl "branch"

must fail with a clear message that file-path fork is no longer supported.

For the directory-mismatch case, from a *different* directory:

    cd /tmp && CAKE_DATA_DIR=/tmp/cake-session-test cake --continue "should fail"

The command must fail with the directory-mismatch error.

For the concurrent-write lock case:

    CAKE_DATA_DIR=/tmp/cake-session-test cake --continue "long task" &
    CAKE_DATA_DIR=/tmp/cake-session-test cake --continue "second task"

The second invocation must fail immediately with the file-lock error message.

The full automated validation command is:

    just ci

It must pass before the task is complete.

## Idempotence and Recovery

The implementation should be safe to retry. Unit and integration tests create temporary session directories and do not depend on the user's real `~/.local/share/cake/sessions`. Manual validation uses `CAKE_DATA_DIR=/tmp/cake-session-test` so repeated runs do not modify real sessions.

Because the new session save model is append-only, a crash may leave a file with a `task_start` but no matching `task_complete`. Loading tolerates this and restores the records that were fully written. A future cleanup command can diagnose incomplete tasks; this refactor does not need to repair them.

Legacy v2 and old-v3 session files will not be migrated. After this refactor they are unreadable. Per the Decision Log, this is acceptable for pre-release software with a single user. The implementer should delete or archive their personal session directory before running the new build for the first time:

    mv ~/.local/share/cake/sessions ~/.local/share/cake/sessions.legacy

## Artifacts and Notes

The current bug-driving context is that restored sessions seed previous conversation records into `Agent.stream`, then append a fresh `Init`, and `Session::save` may synthesize another empty-tools `Init`. This plan does not patch that one bug directly; it replaces the model that caused it. In the new model, prior session records are not seeded into the current task stream, and session metadata is not emitted by the agent.

Relevant current excerpts:

    src/clients/agent.rs::with_stream_records filters out Init and Result but seeds prior conversation records into Agent.stream.
    src/clients/agent.rs::emit_init_message appends an Init record and streams it to stdout in stream-json mode.
    src/config/session.rs::Session::save rewrites the entire file and synthesizes an Init if the first record is not Init.
    src/config/session.rs::Session::load strips a trailing Result from loaded history.

These excerpts should disappear or be meaningfully rewritten by the end of the refactor.

## Interfaces and Dependencies

This refactor adds one dependency: `fs4` (advisory file locking; the maintained successor to `fs2`). No other new crates.

At the end of the refactor, `src/clients/types.rs` should expose two enums equivalent to:

    pub enum SessionRecord {
        SessionMeta {
            format_version: u32,
            session_id: String,
            timestamp: DateTime<Utc>,
            working_directory: PathBuf,
            model: Option<String>,
            tools: Vec<String>,
            #[serde(default)]
            cake_version: Option<String>,
        },
        TaskStart {
            session_id: String,
            task_id: String,
            timestamp: DateTime<Utc>,
        },
        Message { ... },
        FunctionCall { ... },
        FunctionCallOutput { ... },
        Reasoning { ... },
        TaskComplete {
            subtype: TaskCompleteSubtype,
            success: bool,
            is_error: bool,
            duration_ms: u64,
            turn_count: u32,
            num_turns: u32,
            session_id: String,
            task_id: String,
            result: Option<String>,
            error: Option<String>,
            usage: Usage,
            permission_denials: Option<Vec<String>>,
        },
    }

    pub enum StreamRecord {
        TaskStart { session_id: String, task_id: String, timestamp: DateTime<Utc> },
        Message { ... },
        FunctionCall { ... },
        FunctionCallOutput { ... },
        Reasoning { ... },
        TaskComplete { /* same fields as SessionRecord::TaskComplete */ },
    }

    impl From<StreamRecord> for SessionRecord { /* trivial mapping */ }

The exact Rust fields for the conversation variants remain compatible with current `ConversationItem` conversions; the recommended implementation extracts the conversation variants into a shared struct or has both enums hold a `ConversationItem` directly to avoid duplicated field lists.

At the end of the refactor, `src/clients/agent.rs` should not know how to create `session_meta`. It should hold a `task_id` and emit `StreamRecord::TaskStart` and `StreamRecord::TaskComplete` for the current invocation, plus conversation records as the agent loop runs.

At the end of the refactor, `src/config/data_dir.rs` should provide UUID-based session loading and append-based saving with advisory file locking. It should not expose `load_session_from_path`. The re-export list in `src/config/mod.rs` should match.

Plan revision note, 2026-05-03 (initial): Initial plan created from the user's requirements to restore append-only session files, split session files from stream-json output, rename `init`/`result` to `session_meta`/`task_complete`, add `task_start`, and remove file-path resume/fork.

Plan revision note, 2026-05-03 (first post-review): Added separate `StreamRecord` type. Removed all backward-compatibility scope (v2 and legacy-v3 readers, fixtures, and tests are deleted; `format_version` bumps to 4). Pinned `task_id` ownership to `src/main.rs::run` and excluded it from conversation records. Adopted advisory file locking. Slimmed `task_start` to per-task fields only; `working_directory`/`model`/`tools` live solely on `session_meta`. Made `--continue` directory-mismatch fatal with a clear error. Switched validation to mocked unit/integration tests with the CLI examples relegated to manual smoke tests. Added explicit `cargo insta review` step in Milestone 6. Specified that stream-json under `--continue`/`--resume`/`--fork` does not replay prior tasks. Added `cake_version` to `session_meta` for forensics. Anchored the audit `rg` pattern to avoid noise.

Plan revision note, 2026-05-03 (second post-review): Switched lock crate from `fs2` to `fs4` (active fork). Pinned fork semantics: a fork creates a new file with a fresh `session_meta` and copies the parent's conversation records (excluding `task_start`/`task_complete` boundaries) before any new task records. Adopted live-append from agent through a "persist sink" closure, so conversation records land on disk as they are produced; the agent gains a persist-sink field and `with_stream_records` is deleted entirely (no end-of-task drain). Renamed `ResultSubtype` to `TaskCompleteSubtype` and standardized emitter method suffix on `_record`. Added `#[serde(default)]` to `cake_version`. Added explicit `format_version` mismatch error and a test for it. Added `looks_like_uuid` strictness audit step in Milestone 4. Restricted CLI-related tests to unit tests against helpers (no subprocess CLI tests), per the user's preference to avoid test-only mock-injection in `main.rs`. Added a fork-copy unit test to Milestone 6. Clarified that the stream-callback type changes from `Fn(SessionRecord)` to `Fn(StreamRecord)`, making `session_meta` streaming impossible at compile time.

Plan revision note, 2026-05-07 (migration): Moved this completed ExecPlan from `.agents/.plans/` to `.agents/exec-plans/completed/` during the ExecPlan directory migration.
