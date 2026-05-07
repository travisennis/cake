# Plan: Unify stream-json output and session file format

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `.agents/PLANS.md` from the repository root. It was migrated from the former `.agents/.plans/` location and classified as completed because the plan is closed and superseded by the append-only v4 session design in `.agents/exec-plans/completed/append-only-session-management.md`.

## Purpose / Big Picture

This historical plan proposed making `cake --output-format stream-json` and persisted session files share one v3 JSONL schema so redirected stream output could later be resumed. That is no longer the desired behavior. The current repository uses an append-only v4 persisted session log with `session_meta`, `task_start`, and `task_complete` records, while stream-json is a live task feed and deliberately omits `session_meta`.

The observable current behavior is documented in `docs/design-docs/session-management.md` and `docs/design-docs/streaming-json-output.md`. A user should now resume from session UUIDs in the session directory, not arbitrary redirected stream-json files.

## Progress

- [x] (2026-05-07 18:55Z) Confirmed the historical v3 schema goal conflicts with the completed append-only v4 session plan.
- [x] (2026-05-07 18:55Z) Confirmed the current code uses separate persisted and streamed record types rather than a unified schema.
- [x] (2026-05-07 18:55Z) Migrated this closed plan to `.agents/exec-plans/completed/schema-unification-plan.md` and added the required ExecPlan lifecycle sections.

## Surprises & Discoveries

- Observation: The later append-only session plan explicitly chose a different design from this plan.
  Evidence: `.agents/exec-plans/completed/append-only-session-management.md` says stream-json starts at `task_start`, never emits `session_meta`, and file-path resume/fork support was removed.

- Observation: The current type layer preserves the split between persisted and streamed records.
  Evidence: `src/clients/types.rs` defines both `SessionRecord` and `StreamRecord`.

## Decision Log

- Decision: Classify this plan as completed and superseded during the ExecPlan migration.
  Rationale: Leaving it active would contradict the current v4 session architecture and docs. The work is not pending; it was replaced by a newer completed design.
  Date/Author: 2026-05-07 / Codex

## Outcomes & Retrospective

This plan did not remain the final direction. The project moved to append-only v4 session persistence, separate stream-json records, UUID-only resume/fork, and a live stream that is intentionally not a complete resumable session file. Keep this document as historical context for the rejected unified-schema approach.

## Goal

Make `cake --output-format stream-json` output and session history files use the exact same JSONL schema. A user should be able to redirect stream-json output to a file and later resume from that file.

## Decisions

### 1. Reasoning roundtrip (Option A)

The stream-json `reasoning` record must include `encrypted_content` and `content` arrays so that reasoning conversations can be resumed. These fields use `#[serde(skip_serializing_if = "Option::is_none")]` so they only appear when present.

### 2. Result record schema

Keep the current stream-json field names and extend them. The unified `result` record will have:

```
{
  "type": "result",
  "subtype": "success" | "error_during_execution" | "error_max_turns",
  "success": boolean,           // kept for backward compat; inverse of is_error
  "duration_ms": number,
  "turn_count": number,         // kept for backward compat; maps to num_turns
  "num_turns": number,          // added; alias for turn_count
  "is_error": boolean,          // added
  "session_id": string,         // added
  "result": string,             // added on success
  "error": string,              // kept on error
  "usage": { ... },
  "permission_denials": string[] // added
}
```

`exit_code` is removed from the result record. The app can still emit an exit code to the shell, but it is not persisted.

Subtype mapping is:

- `success`: the run completed successfully and `result` contains the final assistant message text.
- `error_during_execution`: any non-max-turn failure after the run started.
- `error_max_turns`: reserved for a future max-turns limit. Do not add this subtype until an actual max-turns failure path exists.

`permission_denials` remains optional. Only populate it if the runtime already has a concrete list of denied permission requests. Do not invent or infer denials from generic tool failures.

### 3. Session loading

- `--continue` continues the most recent session for the current working directory (current behavior, unchanged).
- `--resume` is modified to accept either a session UUID or a file path. If the argument looks like a UUID, load from the sessions directory. Otherwise, treat it as a file path.
- `--fork` is modified to accept either a session UUID or a file path, matching `--resume`. `--fork` without an argument still forks the latest session for the current working directory.
- For file-based `--resume` and `--fork`, the loaded session keeps its original `Init.working_directory`. If the current process working directory does not match that stored directory, exit early with a clear warning/error about the mismatch instead of silently rebasing the session onto the new directory.

### 4. Model enforcement

A session must continue to use the model it began with. When loading a session, read the model from the `Init` record.

- Compare the session model against the fully resolved runtime model string, not the raw `--model` CLI argument. This avoids false mismatches when a settings key like `claude` resolves to a provider model like `anthropic/claude-3-sonnet`.
- If the session model is present and the user passes `--model` that resolves to a different model, error out with a clear message.
- If the session model is present and `--model` is not provided, use the session model.
- If the session model is absent (old sessions), fall back to the default model resolution path and allow `--model` overrides.

### 5. Timestamps

Every stream-json record includes an RFC3339 `timestamp` field, except `result`, which remains timestamp-free unless we explicitly decide to add it to the schema.

- `Init.timestamp` is the timestamp of the current saved snapshot, not immutable session creation time.
- `SessionRecord` variants that correspond to `ConversationItem` all carry `timestamp: Option<String>`.
- For migrated v2 records, preserve existing timestamps when present.

### 6. Exit code

`exit_code` is dropped from the `result` record entirely.

### 7. Save semantics

Session files are no longer append-only. On save, rewrite the full file so it contains exactly:

1. one `Init` record at the top
2. zero or more conversation records in the middle
3. one `Result` record at the end after a completed run

When resuming or forking from an existing file, do not keep old terminal `Result` records in the in-memory session state. The next save writes a single fresh terminal `Result`.

### 8. `Init.tools`

`Init.tools` is informational metadata only.

- Persist it in the schema.
- Parse it on load.
- Do not enforce tool-list equality on `--continue`, `--resume`, or `--fork`.
- On save, rewrite `Init.tools` to the current runtime tool list.
- If useful during implementation, log a debug or warn-level message when stored tools differ from the current runtime tool list, but do not fail.

## Unified JSONL schema

Every line in both stream-json output and session files is a `SessionRecord`:

```rust
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionRecord {
    /// Line 1 of every file. Replaces SessionHeader and emit_init_message.
    Init {
        format_version: u32,
        session_id: String,
        /// Timestamp of the current saved snapshot.
        timestamp: DateTime<Utc>,
        working_directory: PathBuf,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        tools: Vec<String>,
    },

    Message {
        role: Role,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
    },

    FunctionCall {
        id: String,
        call_id: String,
        name: String,
        arguments: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
    },

    FunctionCallOutput {
        call_id: String,
        output: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
    },

    Reasoning {
        id: String,
        summary: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<Vec<ReasoningContent>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
    },

    Result {
        subtype: ResultSubtype,
        success: bool,
        is_error: bool,
        duration_ms: u64,
        turn_count: u32,
        num_turns: u32,
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        usage: Usage,
        #[serde(skip_serializing_if = "Option::is_none")]
        permission_denials: Option<Vec<String>>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
pub enum ResultSubtype {
    Success,
    ErrorDuringExecution,
    ErrorMaxTurns,
}
```

## Architecture changes

### 1. `src/clients/types.rs`

- Add `SessionRecord` enum and `ResultSubtype` enum.
- Keep `ConversationItem` for API communication but add conversion methods:
  - `ConversationItem::to_session_record(&self) -> SessionRecord`
  - `SessionRecord::to_conversation_item(&self) -> Option<ConversationItem>`

### 2. `src/config/session.rs`

- Replace `SessionLine` and `SessionHeader` with `SessionRecord`.
- `Session` struct becomes:
  ```rust
  pub struct Session {
      pub id: String,
      pub working_dir: PathBuf,
      pub model: Option<String>,
      pub records: Vec<SessionRecord>,
  }
  ```
- `Session::save` rewrites the full file in v3 format and ensures there is at most one terminal `Result` record.
- `Session::load` detects old format (v2) vs new format (v3):
  - v2: first line contains `"type": "session_start"`. Use existing `load_format_v2` logic and convert `ConversationItem`s to `SessionRecord`s in memory.
  - v3: first line is `{"type":"init", ...}`. Parse directly as `SessionRecord::Init`, then parse remaining lines as `SessionRecord`.
- When loading v3, drop any trailing `Result` from the in-memory resumable history. That record is metadata about the prior completed run, not an input item for the next run.
- On save, always write v3.

### 3. `src/clients/agent.rs`

- Add `stream: Vec<SessionRecord>` to `Agent`.
- Change `emit_init_message`, `stream_item`, and `emit_result_message` from `&self` to `&mut self` so they can append to `self.stream`.
- Each method appends to `self.stream` first, then optionally fires the streaming callback.
- When restoring a session, seed the agent's stream state from the resumable session records so that saving after resume preserves the full prior conversation instead of only newly emitted records.
- Add `drain_stream(&mut self) -> Vec<SessionRecord>` which drains and returns the full stream including `Init` and `Result`.
- Remove `drain_history_without_system` or reimplement it as a filter over `drain_stream`.

### 4. `src/main.rs`

- In the send path, after the run completes:
  ```rust
  session.records = client.drain_stream();
  ```
- Update `--resume` argument parsing to accept either a UUID or a file path.
- Update `--fork` argument parsing to accept either a UUID or a file path.
- When loading a session for resume/continue, check the model:
  - If `--model` is not provided, use the session's model.
  - If `--model` is provided and differs from the session's resolved model, error out.
  - If the loaded session has no model, use the existing default model resolution behavior.
- Emit and store the `Result` record for both text mode and stream-json mode. Only stdout emission remains conditional on `--output-format stream-json`.
- Remove `exit_code` from the `emit_result_message` call.
- For file-based `--resume` and `--fork`, preserve the original session working directory from the loaded session metadata.
- If a file-based restore is attempted from a different current working directory, exit early with a warning/error that explains the mismatch and instructs the user to run the command from the original directory.

### 5. `src/clients/chat_completions.rs` and `src/clients/responses.rs`

- Ensure that when reasoning items are received, `encrypted_content` and `content` are preserved in `ConversationItem::Reasoning` so they propagate to `SessionRecord::Reasoning`.

## Backward compatibility

- Existing v2 session files continue to load via `load_format_v2`. They are automatically migrated to v3 on the next save.
- The `format_version` field in `Init` is set to `3`.
- Tests for v2 roundtrips are kept but marked as backward-compatibility tests.
- Existing CLI and docs that describe `--resume <UUID>` or `result.exit_code` must be updated alongside the implementation.

## Files to touch

| File | Changes |
|---|---|
| `src/clients/types.rs` | Add `SessionRecord`, `ResultSubtype`, conversion methods |
| `src/config/session.rs` | Replace `SessionLine`/`SessionHeader` with `SessionRecord`; dual-format `load`; simplified `save` |
| `src/clients/agent.rs` | Add `stream` buffer; change emit methods to `&mut self`; add `drain_stream` |
| `src/main.rs` | Use `drain_stream`; update `--resume` and `--fork`; model enforcement; drop `exit_code` |
| `src/config/data_dir.rs` | Add helper(s) for loading from file path and update UUID-only assumptions |
| `README.md` and `docs/design-docs/*.md` | Update `--resume`/`--fork` docs and remove `exit_code` from result examples |
| `tests/` | Update session roundtrip tests; add v2 -> v3 migration test; add stream-json file resume test |

## Acceptance criteria

- [ ] `cake --output-format stream-json` produces a valid v3 JSONL stream.
- [ ] Redirecting stream-json to a file and running `cake --resume <file>` reconstructs the conversation.
- [ ] Reasoning sessions roundtrip correctly (encrypted_content preserved).
- [ ] `--continue` still loads the most recent session by working directory.
- [ ] `--resume <uuid>` loads from the sessions directory.
- [ ] `--resume <path>` loads from an arbitrary file path.
- [ ] `--fork <uuid>` loads from the sessions directory.
- [ ] `--fork <path>` loads from an arbitrary file path.
- [ ] File-based `--resume` and `--fork` preserve the original `working_directory` and exit early with a clear warning/error if invoked from a different directory.
- [ ] Changing `--model` mid-session is rejected with a clear error.
- [ ] Old sessions without a stored model fall back to default model resolution.
- [ ] All existing v2 session files load and save as v3 on next write.
- [ ] Saving after `--resume` or `--fork` preserves prior conversation records and rewrites the file with exactly one terminal `Result`.
- [ ] `exit_code` no longer appears in `result` records.

## Revision Notes

- 2026-05-07 / Codex: Migrated this historical plan into the new completed ExecPlan directory, marked it superseded by the completed append-only v4 session plan, and added lifecycle sections required by `.agents/PLANS.md`.
