## Externalize large tool outputs into per-session blob storage

This ExecPlan is a living document, maintained per docs/workflow/exec-plans.md. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be kept current as work proceeds.

## Purpose / Big Picture

After this change, Cake stores a large tool output once per session in the same content-addressed blob directory that already holds session images. The session JSONL and every compaction checkpoint that retains the exchange carry a SHA-256 `content_id` instead of a copy of the output text. A fork hard-links the parent's output blobs instead of copying them, and a missing or corrupt blob stops the run with a diagnostic naming the session and digest before any provider request.

A user can see the result by running a command whose output is at least 16 KiB and inspecting the session JSONL: the `function_call_output` record contains `content_id` and an empty `output`, and the text exists once under `~/.local/share/cake/sessions/<uuid>.blobs/<digest>`. A shorter output stays inline with no blob. Resuming the session sends the exact original text to the provider. Deleting the blob makes the next resume fail before any provider request. Replaying the session emits the inline text and no `content_id`, so stream consumers see the same shape as before.

The provider wire format does not change. Tool output is still text in the model request; hydration materializes it before request construction. Stream-json keeps its existing inline shape.

This is task 145a, the follow-up anticipated by ADR 061. It is a storage change only: the threshold does not change which output a tool returns, the existing truncation and spill behavior, or the checkpoint ceiling itself. It gives 034f one reference path for retained output text instead of a second copy.

## Progress

- [x] (2026-11-12) Measured existing session output sizes and selected the 16 KiB externalization threshold.
- [x] (2026-11-12) Drafted this ExecPlan and the ADR 061 amendment before changing code.
- [x] (2026-11-13) Milestone 1: add `FunctionCallOutputData.content_id`, extend the shared content-id traversal and stripping projection, and add output externalization and hydration to `BlobStore`.
- [x] (2026-11-13) Milestone 1: prove threshold boundaries, both-field verification, reference-only hydration, empty output, missing/corrupt blob rejection, and UTF-8 failure with unit tests.
- [x] (2026-11-13) Milestone 2: externalize output text in `SessionWriter::append_record` and in `SessionWriter::commit_checkpoint`/`cake sessions checkpoint`, and hydrate it on provider-bound loads and replay.
- [x] (2026-11-13) Milestone 2: prove the writer, checkpoint, replay, and legacy-inline paths with unit and integration tests.
- [x] (2026-11-13) Milestone 3: include output references in fork hard links, resume recovery, and `cake debug blobs`.
- [x] (2026-11-13) Milestone 3: prove a fork shares output inodes, recovery removes only unreferenced output blobs, and diagnostics report missing/corrupt output references.
- [ ] Milestone 4: keep the session-metrics tooling truthful by resolving externalized output text from the session blob directory.
- [ ] Milestone 5: update `docs/integrations.md`, `docs/security.md`, `docs/configuration.md`, `ARCHITECTURE.md`, the ADR amendment, and record snapshots.
- [ ] Milestone 5: run the task verification commands and the full gate.

## Surprises & Discoveries

- A read-only scan of the live session store on 2026-11-12 found 2,597 session files and 175,118 `function_call_output` records holding 552.5 MB of inline text. The table in Artifacts and Notes shows how much text sits above each candidate threshold. A 16 KiB threshold captures 25.4% of output bytes and about 9.5% of raw JSONL bytes with 5,552 outputs, roughly two per session. A 32 KiB threshold captures only 6.5% of output bytes, and 8 KiB creates 3.4 times as many blobs for about twice the captured bytes.
- The same scan found 323 checkpoints retaining 4,385 output records and 7.9 MB of text. At 16 KiB, 47 retained outputs (1.34 MB) become references; at 32 KiB only 12 (0.47 MB) do. Checkpoints are currently nowhere near the eight-MiB ceiling, so the immediate value is deduplication between the raw record, the checkpoint, and a fork, not rescuing an oversized checkpoint.
- Output externalization invalidated the fixture in `flush_failure_with_an_oversized_record_leaves_no_malformed_line`: its 128 KiB retained tool output now serialized as a reference, so the write no longer exceeded `BufWriter`'s capacity. The fixture now retains a 128 KiB message, which stays inline and preserves the direct-write fault path.
- `Session::append_records` writes seed records directly and has no `BlobStore`, so it cannot externalize output text. The only seed path that can carry a tool output today is a fork, and `create_seeded_session_file` already calls `prepare_fork_seed_records` first. The plan keeps that ordering and adds a regression test rather than widening `Session::append_records`.
- The session-metrics Python loader reads `function_call_output.output` directly for failure classification and output sizes. Without a change it would treat every externalized output as empty and silently lose the failure taxonomy. The loader needs to resolve a `content_id` from the sibling `.blobs` directory.
- Milestone 2 needed no new persistence wiring: `append_record`, `commit_checkpoint`, `cake sessions checkpoint`, `DataDir::load_session`, and replay already route through the shared `BlobStore` helpers, so output text followed the image path. The work was proving it with a real-binary run, a writer unit test, a checkpoint-verb test, and a legacy-inline fixture. The real-binary suite sets `CAKE_GATE=off` because the default command-policy gate is not installed in the test environment; otherwise Bash fails closed with `script_evidence` before producing an output.
- Milestone 3 likewise needed no new production traversal: `prepare_fork_seed_records` and `collect_record_content_ids` already consume `SessionRecord::content_ids`, so output references followed images through fork, recovery, and diagnostics. The work was output-specific proof and payload wording. The `debug blobs` integration suite is `tests/debug_blobs.rs`, not `tests/debug_session.rs` as the original milestone criterion said; that criterion is corrected below.

## Decision Log

- Decision: externalize a `function_call_output` when its UTF-8 text length is at least 16 KiB (16,384 bytes); leave shorter text inline. Rationale: 16 KiB matches fx's `large_result_threshold_bytes`, captures a quarter of real output bytes without turning every ordinary command into a blob, and is about 4k tokens or one fifth of the 20k-token retained target. Date/Author: 2026-11-12, Trav.
- Decision: add an optional lowercase SHA-256 `content_id` beside `FunctionCallOutputData.output`, matching `ImagePart.content_id`. An externalized record keeps `"output": ""` and adds the reference, so the serialized field set stays stable for stream consumers and older readers that ignore unknown optional fields. A record carries either non-empty inline text or a reference, never both. Rationale: format version 4 and all existing record shapes stay intact. Date/Author: 2026-11-12, Trav.
- Decision: an empty output with no reference is a valid empty tool result, not an error. Only an image requires one of bytes or a reference. Rationale: tools such as Bash with no output legitimately return an empty string, and the record already permits it. Date/Author: 2026-11-12, Trav.
- Decision: externalize before measurement in `SessionWriter::commit_checkpoint` and in `cake sessions checkpoint`, exactly as images already do. The eight-MiB stdin cap for the checkpoint verb is unchanged; a payload at the cap can produce a smaller record, but a payload above the cap remains an input error. Rationale: the writer and verb already share one ordering, and changing the wire cap is not part of this task. Date/Author: 2026-11-12, Trav.
- Decision: keep stream-json and replay inline. Live stream records are built from `ConversationItem` before persistence, so they never see a reference. Replay hydrates and then clears the output `content_id` in the shared stream projection. Rationale: stream-json is a compatibility surface. Date/Author: 2026-11-12, Trav.
- Decision: forks hard-link every digest referenced by the seed records, images and output text alike, then externalize any eligible inline seed output into the child store. Rationale: this preserves the existing fork contract and keeps the parent and child independent after the link. Date/Author: 2026-11-12, Trav.
- Decision: reuse `content_ids`, recovery, and `debug blobs` for output references rather than adding a parallel traversal. Rationale: one shared traversal means a new payload-bearing record is visible to every consumer or to none, which is the rule ADR 061 set for images. Date/Author: 2026-11-12, Trav.
- Decision: do not add a settings key for the threshold in this task. The constant lives in `src/config/blob_store.rs`; 034f consumes one policy. Rationale: no user has asked to tune it, and a second policy surface would need precedence, documentation, and tests before there is evidence for it. Date/Author: 2026-11-12, Trav.
- Decision: teach `scripts/session-metrics/cakelib.py` to resolve an externalized output from the session's blob directory, and add a fixture test. Rationale: the metrics reports are a durable analysis surface; silently treating externalized failures as empty successes would corrupt them. Date/Author: 2026-11-12, Trav.
- Decision: keep the command name `cake debug blobs` and its check ids, but change its prose and help from "image blobs" to "session payload blobs". Rationale: the command already reports the whole blob directory, and renaming a CLI surface is unnecessary for this scope. Date/Author: 2026-11-12, Trav.

## Outcomes & Retrospective

Not started. The only delivered artifacts are this plan and the ADR 061 amendment. No production code, session record, or test has changed yet.

## Context and Orientation

Cake persists each invocation as a format-4 append-only JSONL file under `~/.local/share/cake/sessions/{uuid}.jsonl`, or `{CAKE_DATA_DIR}/sessions/{uuid}.jsonl`. The first record is `session_meta`; conversation records are `message`, `function_call`, `function_call_output`, and `reasoning`; audit records include `turn_usage`, `hook_event`, and `task_complete`. A `compaction` record is a checkpoint: one summary plus a retained tail of complete exchanges that replaces retired history in provider requests.

A **blob** is a file holding one payload's raw bytes once. Its **digest** or **content id** is the lowercase hexadecimal SHA-256 of those bytes. **Externalizing** a payload means writing it to the session's blob directory and replacing the inline field with a `content_id`. **Hydrating** means reading the blob, verifying the digest, and restoring the inline field in memory before the conversation is used. The blob directory is a sibling of the session file:

```text
~/.local/share/cake/sessions/{uuid}.jsonl
~/.local/share/cake/sessions/{uuid}.blobs/{digest}
```

`ImagePart` already carries `content_id`, and `BlobStore` already owns publication, verified reads, hard-link forks, append-time recovery, and diagnostics. `SessionRecord::image_parts` and `SessionRecord::content_ids` are the shared image traversals; `content_ids` is what recovery and `debug blobs` consume. `SessionWriter::append_record` externalizes a clone of every non-checkpoint record under its mutex, and `SessionWriter::commit_checkpoint` externalizes retained images before `CompactionData::to_record_bytes` measures the framed record. `DataDir::load_session` and `load_latest_session` hydrate before returning a provider-bound session. `cake sessions checkpoint` externalizes before its pre-validation.

`FunctionCallOutputData` currently has `call_id`, `output`, optional `replay`, optional `timestamp`, and an optional `images` array. The `output` string is always inline and always counts toward the serialized checkpoint ceiling. The default tool caps are Bash, MCP, and toolbox at 50,000 bytes and Read at 100,000 bytes, but a configured `"unlimited"` and a delegated plugin can return more. This task does not change any of those tools or their caps.

The checkpoint ceiling is `MAX_CHECKPOINT_RECORD_BYTES` in `src/types/compaction.rs`: eight MiB of serialized JSONL including the final newline. `CompactionData::to_record_bytes` serializes the record and enforces the ceiling while the session writer holds its append lock.

A fork copies conversation records and the latest checkpoint into a new session through `SessionPersistencePlan::Create { seed_records, blob_source }`. `create_seeded_session_file` calls `prepare_fork_seed_records` before the child file exists. `Session::append_records` then writes the prepared records directly.

The key files and their roles are:

- `src/types/session.rs`: `FunctionCallOutputData`, `SessionRecord`, `StreamRecord`, `content_ids`, and the content-id stripping projection.
- `src/types/compaction.rs`: `CompactionData`, `RetainedRecord`, and the checkpoint size ceiling.
- `src/config/blob_store.rs`: `ContentId`, `BlobStore`, publication, verified reads, externalization, hydration, fork linking, recovery, and diagnostics.
- `src/config/session.rs`: `Session::load`, `SessionWriter`, checkpoint commit, append-lock handling, and partial-tail repair.
- `src/config/data_dir.rs`: the durable sessions root, session paths, `session_blob_dir`, `session_blob_store`, and provider-bound loads.
- `src/cli/persistence.rs`: session creation, fork seed preparation, and append-time recovery.
- `src/cli/session_factory.rs`: resume and fork construction.
- `src/cli/checkpoint.rs`: `cake sessions checkpoint`, the external checkpoint append verb.
- `src/cli/replay.rs`: read-only replay and its stream projection.
- `src/cli/debug.rs`: the read-only `cake debug blobs` report.
- `src/clients/agent_observer.rs`: live stream serialization, which happens before persistence.
- `scripts/session-metrics/cakelib.py`: offline transcript reading and tool-call pairing.

The existing behavior that must be preserved is: session format version stays 4; an old session with inline output loads without a blob read; stream-json and provider wire shapes are unchanged; session files remain append-only; owner-only modes and no-follow path checks remain in force; checkpoint validation and the eight-MiB ceiling are unchanged; and no invocation reads or writes payload bytes over the network.

## Plan of Work

### Milestone 1: record shape and blob helpers

Add `content_id: Option<String>` to `FunctionCallOutputData` in `src/types/session.rs` with `#[serde(default, skip_serializing_if = "Option::is_none")]`. Keep `output: String` required, so every existing record and every stream record keeps its current field set and an externalized record serializes `"output": ""` beside the reference. Do not add `#[serde(default)]` to `output`: a missing inline field is a malformed record, and the checkpoint verb can externalize a retained output from the inline form. A reference-only retained output in a checkpoint payload is out of scope because the external compactor sends the text it already holds and Cake externalizes it before measurement.

Extend `SessionRecord::content_ids` so it returns image ids and the output `content_id` for a `FunctionCallOutput`, including a retained output inside a checkpoint. The simplest implementation collects `image_parts` ids plus the output ids into a `Vec<&str>` and returns its iterator; the call sites are recovery, diagnostics, and fork provisioning, so the allocation is irrelevant. Extend `SessionRecord::strip_image_content_ids` and `StreamRecord::strip_image_content_ids` to clear the output `content_id` as well, and rename both to `strip_content_ids` with docs that say "payload references" rather than "images". Update `RetainedConversationRecord`'s checkpoint helper the same way.

In `src/config/blob_store.rs`, add:

```text
pub const TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES: usize = 16 * 1024;
```

with a doc comment naming this plan and the measured rationale. Add `fn externalize_output(&self, data: &mut FunctionCallOutputData, context: &str)`. When `content_id` is present, parse it with `ContentId::parse`, verify the blob exists and matches, and require the inline `output` to be empty or to hash to that id. When no id is present and `output.len() >= TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES`, compute the digest with `ContentId::from_bytes(output.as_bytes())`, call `put`, clear `output`, and set `content_id`. When no id is present and the text is shorter, leave the struct unchanged. An existing id is never dropped, even if the text is below the threshold, because the record already depends on that blob.

Add `fn hydrate_output(&self, session_id: &str, data: &mut FunctionCallOutputData, context: &str)`. When `content_id` is present and `output` is empty, read the blob with `read_verified`, convert the bytes with `String::from_utf8`, and set `output`. When both are present, verify that the inline bytes hash to the id and keep the inline text. When neither is present, leave the empty output as a valid empty result. Errors name the session id, digest, and whether the blob is missing, unreadable, non-regular, corrupt, or not valid UTF-8; error text never includes payload bytes.

Route the new helpers through the existing shared paths. `externalize_record` handles `SessionRecord::FunctionCallOutput` by externalizing its images and then its output. `externalize_checkpoint` does the same for a retained `FunctionCallOutput`. `hydrate_record` and `hydrate_checkpoint` do the same on the read side. `prepare_fork_seed_records` must collect ids from `record.content_ids()` rather than `record.image_parts()`, so an output reference is linked before the seed record is externalized.

Milestone 1 is done when `cargo test config::blob_store` and `cargo test types::session` pass with tests for: 16,383 bytes stays inline, 16,384 bytes externalizes, an id-only record hydrates, both fields with matching bytes verifies, both fields with mismatched bytes fails, a missing and corrupt blob each fail with the session id and digest, a reference to non-UTF-8 bytes fails without printing them, an empty output with no id stays empty, and two identical 16 KiB outputs share one blob.

### Milestone 2: writer, checkpoint, and replay integration

`SessionWriter::append_record` already calls `externalize_record` inside the write critical section, so routing the new helper through that method is enough for live appends. Keep the clone semantics: the live `FunctionCallOutputData` keeps its text for the next provider request, and only the persisted record carries the reference. The existing terminal-writer failure on a blob error is the desired behavior; a record must never be appended with a digest whose payload was not published.

`SessionWriter::commit_checkpoint` already calls `externalize_checkpoint` before `to_record_bytes`, and `cake sessions checkpoint` already calls it before its pre-validation. Both now externalize retained output text above the threshold, so the eight-MiB measurement counts a digest instead of the text. Add a checkpoint test with one retained output above the cap in inline form and prove the committed record is under the ceiling and the hydrated retained output matches the original bytes. Keep the near-cap stdin test: an incoming payload at 8 MiB still parses, and externalization may shrink the committed record.

`DataDir::load_session` and `load_latest_session` already call `hydrate_session`, which now covers output text. `src/cli/replay.rs` already hydrates and then strips references; extend the strip to the output field. Live stream-json needs no change because `AgentObserver` serializes `StreamRecord` from a `ConversationItem` before the persistence callback ever externalizes the clone.

Milestone 2 is done when a real-binary test with a mock provider runs a command with a 20 KiB output, the persisted `function_call_output` has `"output": ""` and a 64-character lowercase `content_id`, exactly one blob exists and hashes to that id, a resume reaches the provider with the exact original text, and replay emits `"output": "<the 20 KiB text>"` with no `content_id`. A second run with a 15 KiB output must stay inline and create no blob. A legacy session written with inline output before this change must load, resume, and replay unchanged. Removing the blob or replacing its bytes must make resume fail with a nonzero exit before any provider request, naming the session and digest.

### Milestone 3: forks, recovery, and diagnostics

Fork preparation changes from image ids to all content ids. `prepare_fork_seed_records` parses every id from `record.content_ids()`, verifies and hard-links each parent blob into the child store, then calls `externalize_record` on each seed. An inline output at or above the threshold is published into the child; an inline output below the threshold stays inline; an existing reference is verified and kept. A missing or corrupt parent output blob fails before the child file exists, and the existing cleanup removes the incomplete child file and any directory the fork created.

Recovery needs no new traversal because `referenced_content_ids` now includes output references. Add tests that an unreferenced output blob is removed on resume while a referenced one is kept, and that a referenced-but-corrupt output blob is reported and never removed. Update the recovery log and doc comments from "image blobs" to "payload blobs".

`cake debug blobs` needs no new traversal either, because `collect_record_content_ids` uses `record.content_ids()`. Update its check text and human output from "image references" to "payload references" and its header from "Image blobs" to "Session payload blobs". Add a diagnostics test with an output reference that is missing and one that is corrupt, and one that is unreferenced, asserting the same `blobs.reference_missing`, `blobs.reference_corrupt`, and `blobs.unreferenced` ids and that no payload bytes appear in the report. Update `src/cli/debug.rs` help and `src/main.rs` examples only if they say "image" where they now mean payload.

Milestone 3 is done when the fork test proves the parent and child output blob share an inode, a missing parent output blob fails with no child file, recovery removes only the unreferenced output blob, and `cargo test --test debug_blobs` covers an output reference in the report.

### Milestone 4: keep session metrics truthful

Change `ToolCall` construction in `scripts/session-metrics/cakelib.py` so `pair_tool_calls` can resolve an externalized output. Pass the session's sibling blob directory from `Session.tool_calls`; when a `function_call_output` has a `content_id`, read `{blob_dir}/{content_id}`, verify the SHA-256 before use, decode UTF-8, and use that text for `output`, `ok`, and output-size calculations. If the blob is missing or corrupt, keep the tool call but mark it as an unknown/error output rather than an empty success; do not crash the report. Inline records keep their current path.

Add a fixture test under `scripts/session-metrics/tests/` that pairs an externalized failing output and asserts the failure is still classified, plus one that externalized output bytes count toward the output size. Run `just session-metrics-check`.

### Milestone 5: documentation, snapshots, and verification

Append an "Amendment: externalize large tool-output text" section to `docs/adr/061-content-address-session-images-in-per-session-blob-directories.md`. State the record shape, the 16 KiB threshold, the publication-before-marker ordering, hydration and fail-closed behavior, checkpoint and fork behavior, stream-json preservation, the unchanged stdin cap, and the consequences. The amendment extends the image-only decision; it does not replace it.

Update `docs/integrations.md`: the `function_call_output` record shape, the checkpoint retained-tail paragraph, the blob-layout and recovery sections, the replay paragraph, the `debug blobs` diagnostic ids, and the checkpoint payload paragraph. Update `docs/security.md` to say payloads rather than images and to keep the same sensitivity, owner-only, no-follow, digest-verification, and no-log rules. Update `docs/configuration.md`'s blob diagnostics section and the data-root layout, `ARCHITECTURE.md`'s persistence boundary, and the one sentence in `docs/exec-plans/active/context-compaction.md` that refers to 145a's threshold so it names 16 KiB.

Add record snapshots for `function_call_output` with an output reference and for a compaction retained tail with an output reference. Existing snapshots must remain byte-identical because `content_id` is skipped when absent and `output` remains present. Add or update the replay fixture proving the output reference is stripped.

Run the full task verification: `cargo test session`, `cargo test checkpoint`, `cargo test replay`, `cargo test --test session_images`, `cargo test --test debug_session`, `just session-metrics-check`, and `just docs-check`. Then run `just check`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `git diff --check`. No paid model calls are authorized.

## Concrete Steps

All commands run from `/Users/travisennis/Projects/cake/cake` unless stated otherwise.

Start with the focused unit suites while implementing:

```text
cargo test config::blob_store
cargo test types::session
cargo test config::session
cargo test checkpoint
cargo test replay
```

Expect each to end with `test result: ok` and no ignored failures. The blob-store suite should name the new threshold, hydration, and fork tests. The session-record suite should show the new snapshots as accepted and every old snapshot unchanged.

Run the real-binary suites next:

```text
cargo test --test session_images
cargo test --test debug_session
just session-metrics-check
just docs-check
```

Expect the session-images suite to remain green, the debug-session suite to include the output-reference report, and both `just` recipes to exit 0. If `insta` reports a changed snapshot, inspect the diff before accepting it; only the new reference-bearing shapes may change.

Finish with the full gate:

```text
just check
cargo fmt --check
cargo clippy --all-targets -- -D warnings
git diff --check
```

`just check` runs the unit suite, integration suites, complexity gate, structural checks, and script checks. Record the exact counts it prints in the task's Acceptance Notes. A skipped platform check, such as a Linux-only path on macOS, belongs in the final handoff.

## Validation and Acceptance

Acceptance is behavior a person can observe:

- A Bash call whose output is 20 KiB persists a `function_call_output` record with `"output": ""` and a 64-character lowercase `content_id`. Exactly one file exists at `<sessions dir>/<uuid>.blobs/<digest>`, its SHA-256 is the filename, and its bytes are the tool's output.
- A Bash call whose output is 15 KiB persists the text inline, creates no blob directory entry for it, and resumes and replays unchanged.
- Resuming either session sends the exact original text to the provider. The provider request and stream-json shapes are otherwise identical to before the change.
- Replay hydrates the output, prints the inline text, and omits `content_id`.
- A checkpoint that retains a 20 KiB output stores a digest and stays below the eight-MiB ceiling. Hydrating the checkpoint restores the exact retained text.
- A fork of a session with an externalized output creates a child blob directory whose output blob is a hard link to the parent's inode. Deleting one session's blob directory does not affect the other.
- Removing the output blob makes the next provider-bound load fail before any provider request, with a nonzero exit and an error naming the session and digest. Replacing the blob with bytes that hash differently fails the same way.
- An output reference to non-UTF-8 bytes fails closed with the session and digest, and no payload bytes appear in any error or report.
- `cake debug blobs` reports a missing output reference, a corrupt output reference, and an unreferenced output blob with the existing check ids and without printing payload bytes.
- Resuming a session for append removes an unreferenced output blob left by an interrupted write while keeping every referenced output blob.
- A legacy session with inline output loads, resumes, and replays exactly as before.

## Idempotence and Recovery

Externalization is idempotent. A record with a `content_id` verifies the existing blob and does not write a second copy. A re-append of a hydrated record verifies the inline text against the id, then re-externalizes it. Re-running a test or a command twice never creates duplicate blobs or changes a committed session record.

Changing the threshold later does not rewrite existing records. Old inline output below the new threshold stays inline; old references stay references because the existing id is always honored. A fork externalizes only what its seed records carry.

A crash between blob publication and the record append leaves an unreferenced blob. The next append open runs recovery after the lock and torn-tail repair and removes it. A crash before publication leaves a `.tmp-` file, which the same recovery removes. A missing or corrupt referenced blob is never recreated and never substituted with empty text; the load fails closed.

There is no destructive CLI command, no time-based sweep, and no new store lock. Recovery is scoped to one session's blob directory. If a future `sessions delete` or prune command lands, it must remove `<uuid>.blobs` with the session file, because this task makes the pair the backup unit for both images and large output text.

## Artifacts and Notes

Threshold measurement, read-only from the live store on 2026-11-12 (2,597 sessions, 175,118 live output records, 552.5 MB of inline output, 323 checkpoints with 4,385 retained outputs and 7.9 MB):

```text
threshold   live outputs   live bytes   % of raw JSONL   checkpoint retained
>=  8 KiB         18,775     287.3 MB            19.5%          155 / 2.51 MB
>= 16 KiB          5,552     140.5 MB             9.5%           47 / 1.34 MB
>= 24 KiB          2,490      80.3 MB             5.5%           33 / 1.06 MB
>= 32 KiB            846      35.8 MB             2.4%           12 / 0.47 MB
>= 48 KiB            118       7.6 MB             0.5%            1 / 0.05 MB
```

The chosen 16 KiB boundary is tested at 16,383 bytes (inline) and 16,384 bytes (externalized).

Expected externalized record shape, with the digest shortened here:

```json
{"type":"function_call_output","call_id":"call-1","output":"","content_id":"<64 lowercase hex>","timestamp":"2026-11-12T20:00:00Z"}
```

Expected replay shape, with the original text inline and no reference:

```json
{"type":"function_call_output","call_id":"call-1","output":"<original text>"}
```

Expected failure text when a referenced output blob is missing (the exact wording may vary, but the session id and digest must both appear):

```text
session <uuid>: failed to hydrate record 7 function call output content id '<digest>': blob store directory ... does not exist
```

## Interfaces and Dependencies

By the end, these names must exist and have the stated purpose:

- `FunctionCallOutputData.content_id: Option<String>` in `src/types/session.rs`, the optional lowercase SHA-256 reference beside the required `output` string.
- `TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES: usize` in `src/config/blob_store.rs`, the 16 KiB boundary.
- `BlobStore::externalize_record`, `BlobStore::externalize_checkpoint`, and `BlobStore::hydrate_session` in `src/config/blob_store.rs`, extended to output text without changing their signatures.
- `BlobStore::prepare_fork_seed_records`, collecting all `SessionRecord::content_ids` so output blobs are linked.
- `SessionRecord::content_ids` and `SessionRecord::strip_content_ids` in `src/types/session.rs`, and the matching `StreamRecord`/`CompactionData` helpers, covering images and output text.
- `DataDir::load_session` and `DataDir::load_latest_session`, unchanged signatures, now hydrating output text through the shared store.
- `scripts/session-metrics/cakelib.py`'s tool-call pairing, resolving a `content_id` from the session's sibling blob directory.

No new crate dependency is needed: `sha2` and `hex` are already used by `BlobStore`. No provider, sandbox, CLI exit-code, hook, toolbox, or MCP protocol changes. No session-format version bump.
