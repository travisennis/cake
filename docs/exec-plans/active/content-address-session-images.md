## Content-address session images in per-session blob directories

This ExecPlan is a living document, maintained per docs/workflow/exec-plans.md. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be kept current as work proceeds.

## Purpose / Big Picture

After this change, Cake stores each image once per session in a content-addressed blob directory beside the session file. Session JSONL and compaction checkpoints carry a SHA-256 `content_id` instead of a copy of the image bytes, so the raw record and every checkpoint that retains it share one file, and a fork hard-links the parent's blobs instead of copying them. The eight-MiB checkpoint ceiling counts references rather than image bytes. A payload is verified before it is used; a missing or corrupt blob stops the run with a diagnostic naming the session and digest instead of sending empty content to a provider.

A user can see the result by running a session that reads an image, then inspecting the session JSONL: the image record contains `content_id` and no base64 bytes, and the payload exists once under `~/.local/share/cake/sessions/<uuid>.blobs/<digest>`. Resuming works normally. Removing the blob makes the next resume fail before any provider request, with the session id and digest in the error. Forking a reference-bearing session creates a child blob directory whose entries are hard links to the parent's inodes; both sessions resolve the same bytes. Resuming a session with an orphaned blob from an interrupted append removes that blob; a read-only command reports orphans without deleting them.

The provider wire format does not change. Images are still materialized as data URLs at request construction. Stream-json output also keeps its existing inline shape.

Tool-output externalization is out of scope for this task. At current defaults Bash and MCP cap output at 50 KB and Read at 100 KB, while one maximum-size image is roughly seven MB of base64 and about eighty percent of the checkpoint ceiling. Images are the binding constraint; tool text stays inline until follow-up task 145a.

## Progress

- [x] Milestone 1: add `ContentId`, the per-session `BlobStore`, shared no-follow root validation, `.tmp-` naming, parent-directory sync, and `DataDir::session_blob_dir` / `session_blob_store`.
- [x] Milestone 1: cover digest paths, write-once publication, owner-only modes, no-follow behavior, temp cleanup, parent and destination directory sync paths, concurrent writers, and corrupt reuse with unit tests.
- [x] Milestone 2: add optional `ImagePart.content_id`, carry it through the conversions, externalize images in the session writer, and hydrate on load and replay.
- [x] Milestone 2: hard-link parent blobs into a fork and prove a missing blob fails before the provider request.
- [x] Milestone 2: prove an image round trip through a real CLI run with a mock provider.
- [ ] Milestone 3: externalize retained checkpoint images before the eight-MiB check and in `cake sessions checkpoint`.
- [ ] Milestone 3: add checkpoint, fork, replay, and legacy-inline regression tests.
- [ ] Milestone 4: add payload-first recovery at resume: scan the session, remove unreferenced blobs and stale temporary files, and never touch referenced blobs.
- [ ] Milestone 4: add the read-only `cake debug blobs` diagnostics command and its tests.
- [ ] Milestone 5: update `docs/integrations.md`, `docs/security.md`, `docs/configuration.md`, `ARCHITECTURE.md`, and the record snapshots.
- [ ] Milestone 5: run the task verification commands and the full gate.

## Surprises & Discoveries

- Milestone 1 (2026-11-12): macOS temp directories accept directory `fsync` after creating the store root and after a fork hard link; 15 store tests pass. A plain read open of a FIFO target could block before the post-open metadata check, so `read_verified` preflights `symlink_metadata` and opens with `O_NONBLOCK` on Unix. Linux execution remains for later milestone verification.

## Decision Log

- Decision: follow ADR 061 and add an optional lowercase SHA-256 `content_id` to `ImagePart` only. Rationale: format version 4 stays intact, legacy sessions load unchanged, and the raw record and checkpoint retained tails can reference one stored copy. Date/Author: 2026-11-12, Trav.
- Decision: externalize every image; leave tool-output text inline in this task. Rationale: tool output is already bounded at the tool (50 KB Bash/MCP, 100 KB Read), while one maximum-size image is about eighty percent of the checkpoint ceiling. Tool-output references stay with 034f or a follow-up. Date/Author: 2026-11-12, Trav.
- Decision: store blobs per session at `<sessions dir>/<uuid>.blobs/<digest>`, with no shard directory and no global root. Rationale: within-session reuse plus hard links on fork cover the real duplication; a global store costs a cross-session lock, a sweep, a new grant, and a non-self-contained session. Date/Author: 2026-11-12, Trav.
- Decision: use payload-before-marker ordering: sync and publish the blob, then append the record. Recover unreferenced blobs and stale temporary files when the session is next opened for append; never run a time-based sweep. Rationale: pi's JSONL backend shows the ordering makes a crash leave an orphan rather than a record pointing at a missing payload, and a per-session scan makes recovery deterministic. Date/Author: 2026-11-12, Trav.
- Decision: an externalized record omits its inline field rather than writing an empty string. Rationale: a binary without this feature then fails to load the record instead of silently sending an empty image to a provider. Date/Author: 2026-11-12, Trav.
- Decision: a fork hard-links only the digests referenced by its seed records, after verifying each parent blob. Rationale: it avoids copying bytes and avoids carrying blobs referenced solely by retired history into the child. Date/Author: 2026-11-12, Trav.
- Decision: serialize stream-json before externalization and drop `content_id` from replay projections. Rationale: the live stream and replay remain unchanged compatibility surfaces, and the in-memory conversation must keep bytes for the next provider request. Date/Author: 2026-11-12, Trav.
- Decision: add a read-only `cake debug blobs` command for diagnostics. Rationale: health needs a reviewable surface, but no user-facing command should delete payloads in this task. Date/Author: 2026-11-12, Trav.
- Decision: task 145 is images-only; large tool-output text moves to child task 145a, and `034f` consumes that separately. `cake debug blobs` stays read-only; the no-CLI rule applies only to a destructive sweep. Rationale: images are the binding checkpoint-size constraint, and a destructive surface needs its own scope and tests. Date/Author: 2026-11-12, Trav.
- Decision: create the blob directory owner-only and sync its sessions parent before publishing the first blob; temp names use the `.tmp-` prefix; fork links sync the destination directory. Rationale: payload-first ordering is incomplete if a durable record can outlive a lost directory entry, and recovery must distinguish temps from digests. Date/Author: 2026-11-12, Trav.
- Decision: reject an image with neither inline bytes nor a content id during externalization and checkpoint commit, not only during hydration. Rationale: an unloadable image should stop the write before it becomes a durable record. Date/Author: 2026-11-12, Trav.
- Decision: use one stream projection that clears content ids from message images, function-call-output images, and checkpoint retained images. Rationale: replay and any hydrated live path must preserve the old stream shape, and a checkpoint retains images outside the message and output paths. Date/Author: 2026-11-12, Trav.
- Decision: validate that the session header id matches the filename UUID before deriving a blob directory. Rationale: path and header ids are both used in different load paths, and a mismatch would attach one session's references to another session's store. Date/Author: 2026-11-12, Trav.

## Outcomes & Retrospective

Milestone 1 is implemented. `src/config/blob_store.rs` has validated content ids, write-once publication, verified reads, hard-link forks, owner-only modes, no-follow root checks, `.tmp-` cleanup naming, and parent/destination directory syncs. `DataDir` exposes the sibling path and store without creating the directory.

Milestone 2 core is implemented. `ImagePart` carries an optional `content_id`; the live record keeps inline bytes while `SessionWriter` externalizes a clone under its mutex and fails terminally on a blob error. `DataDir` validates the filename UUID against the header and hydrates before returning a provider-bound session; replay hydrates before emitting and strips references from message, function-call-output, and checkpoint-retained images. The targeted store, session-record, data-directory, and replay tests pass, as does the full 2155-test unit suite; `just cc-check` is clean.

Fork provisioning is implemented. `SessionPersistencePlan::Create` carries `blob_source`; seed creation hard-links every referenced parent blob, externalizes legacy inline message and function-call-output images, and then writes the fork file by reference. A failure removes the incomplete child file and any blob directory the fork created, and a missing parent blob fails before the child file exists. `SessionRecord::image_parts` is the shared traversal for message, output, and checkpoint-retained images. The fork test asserts matching content ids, a shared inode, and a link count of at least two; the missing-blob test asserts no child file or blob directory is left behind.

Milestone 2 is complete. `tests/session_images.rs` drives the real binary against a mock image-capable Responses provider: the model calls `ReadImage`, the second provider request carries the image, and the session stores one digest-named blob with a `content_id` and no `data_base64`. A resume hydrates the image and reaches the provider without duplicating the blob. Removing the blob or replacing its bytes makes resume fail before any provider request, with the session id and digest in stderr.

## Context and Orientation

Cake persists each invocation as a format-4 append-only JSONL file under `~/.local/share/cake/sessions/{uuid}.jsonl`, or `{CAKE_DATA_DIR}/sessions/{uuid}.jsonl`. The first record is `session_meta`; conversation records are `message`, `function_call`, `function_call_output`, and `reasoning`; audit records include `turn_usage`, `hook_event`, and `task_complete`. A `compaction` record is a checkpoint: one summary plus a retained tail of complete exchanges that replaces retired history in provider requests.

A **blob** is the file holding one image's raw decoded bytes once. Its **digest** or **content id** is the lowercase hexadecimal SHA-256 of those bytes. **Externalizing** an image means writing the payload to the session's blob directory and replacing `ImagePart.data_base64` with a `content_id`. **Hydrating** means reading the blob, verifying the digest, and restoring `data_base64` in memory before the conversation is used. The blob directory is a sibling of the session file:

```text
~/.local/share/cake/sessions/{uuid}.jsonl
~/.local/share/cake/sessions/{uuid}.blobs/{digest}
```

Under `CAKE_DATA_DIR`, both live under `{CAKE_DATA_DIR}/sessions/`. `DataDir::sandbox_read_dirs()` already returns the sessions root, so the blob directory needs no new grant. `sessions list` filters to the `.jsonl` extension, so a `.blobs` directory is ignored.

Today image bytes are serialized inline in `ImagePart.data_base64` on message and function-call-output records, and in the retained records inside a checkpoint. `PinReason::UnconsumedImage` deliberately retains image-bearing records, so checkpoint externalization is a real path, not a hypothetical one.

The checkpoint ceiling is `MAX_CHECKPOINT_RECORD_BYTES` in `src/types/compaction.rs`: eight MiB of serialized JSONL including the final newline. `CompactionData::to_record_bytes` serializes the record and enforces the ceiling while the session writer holds its append lock. Retained images count against it today.

A fork copies conversation records and the latest checkpoint into a new session through `SessionPersistencePlan::Create { seed_records }`; `create_seeded_session_file` writes them under one lock. The parent session is loaded through `DataDir::load_session` or `load_latest_session`, which is where hydration runs.

The key files and their roles are:

- `src/types/conversation.rs`: `ImagePart`, `ConversationItem`, and the image-to-data-URL helper.
- `src/types/session.rs`: `MessageData`, `FunctionCallOutputData`, `SessionRecord`, `StreamRecord`, and their conversions.
- `src/types/compaction.rs`: `CompactionData`, `RetainedRecord`, retained selection types, the projector, and the checkpoint size ceiling.
- `src/config/session.rs`: `Session::load`, `SessionWriter`, checkpoint commit, append-lock handling, and partial-tail repair.
- `src/config/data_dir.rs`: the durable sessions root, session paths, `sandbox_read_dirs`, and the new per-session blob path.
- `src/config/owner_only.rs`: owner-only modes and `O_NOFOLLOW` open options.
- `src/config/session_paths.rs`: parent and target path validation.
- `src/cli/persistence.rs`: creates a new session file and writes seed records before returning the locked append handle.
- `src/cli/session_factory.rs`: resume and fork construction. A fork copies conversation records and only the latest checkpoint, re-enveloped for the new session.
- `src/cli/checkpoint.rs`: `cake sessions checkpoint`, the external checkpoint append verb.
- `src/cli/replay.rs`: reads a session read-only and emits stream-json records.
- `src/clients/agent_observer.rs`: serializes stream-json first, then converts to a `SessionRecord` and calls the persistence callback.
- `src/clients/tools/read_image.rs`: produces `ImagePart` values as base64.
- `src/clients/responses.rs` and `src/clients/chat_completions.rs`: materialize images as data URLs.
- `src/main.rs`: `attach_persistence` creates the `SessionWriter`; the command enum wires `cake debug` subcommands.

The existing behavior that must be preserved is: session format version stays 4; an old session with inline payloads loads without a blob read; stream-json keeps its current record shapes; provider requests keep their current wire shapes; session files remain append-only; owner-only modes and no-follow path checks remain in force; and no invocation reads or writes payload bytes from the network.

## Plan of Work

### Milestone 1: the per-session blob store primitive

Create `src/config/blob_store.rs` and export it from `src/config/mod.rs`. `BlobStore` owns one session's blob directory and exposes a validated `ContentId` type plus the operations the rest of the plan needs. The module also owns the shared no-follow directory validator and the `.tmp-` temporary-file naming used by publication and recovery. The module must not depend on `cli` or `clients`; it may use `types` and the existing `config` helpers.

`ContentId` wraps a 64-character lowercase hexadecimal string. Parsing rejects any other length, uppercase characters, or non-hex characters. `from_bytes` computes SHA-256 with the already-present `sha2` dependency and `hex` encoding. Derive `Ord` and `Eq` so referenced digests fit a `BTreeSet`.

`BlobStore::put(bytes)` computes the id. Before using the root, the shared validator uses `symlink_metadata` to require either a missing path or a real directory; a symlink or non-directory fails. If the final `<root>/<digest>` exists, open it without following symlinks, require a regular file, hash the bytes, and compare; a mismatch is an error naming the digest, never an overwrite. Otherwise create the root with `0700` using the existing owner-only helper, sync the sessions parent when the root was newly created, write a temporary file named with the `.tmp-` prefix and `0600` using `create_new`, call `sync_all`, publish the final path with `std::fs::hard_link(temp, final)` so an existing digest is not replaced, sync the directory on Unix so the directory entry is durable, and remove the temporary file. If the link fails with `AlreadyExists`, verify the winner and discard the temporary file. Remove the temporary file on every error path. Never log the bytes.

`BlobStore::read_verified(&ContentId)` validates the root with the shared no-follow check, opens the final path without following symlinks, rejects a non-regular file, reads the bytes, hashes them, and returns the bytes only when the digest matches. Errors distinguish a missing or symlinked root, missing blob, unreadable blob, non-regular target, and digest mismatch, and always include the digest.

`BlobStore::link_verified(&self, source: &BlobStore, id: &ContentId)` validates both roots, verifies `source`'s blob, creates a hard link at the destination if absent, syncs the destination directory, and verifies the destination afterwards. It is the fork primitive. If the destination already exists, verify it instead of replacing it. A destination root created by this call is synced in its parent.

Do not add a store lock file, a shared/exclusive guard, a sweep, or a shard directory. The per-session append lock is the only serialization the store needs.

Temporary files use the literal `.tmp-` prefix followed by the process id, a timestamp, and a process-local counter. A 64-character lowercase hex name is always a digest; recovery may remove only `.tmp-*` names or unreferenced 64-hex names. No other name is touched.

`DataDir` gains `session_blob_dir(id) -> PathBuf` (the sibling `<uuid>.blobs` path) and `session_blob_store(id) -> BlobStore`. It gains no field and no new root. Do not create the blob directory during ordinary startup; create it lazily on the first write that needs it, so a run that never stores an image does not add a directory. Do not change `sandbox_read_dirs`.

Milestone 1 is done when `cargo test config::blob_store` passes, including digest and path tests, two writes of the same bytes yielding one file, two threads racing on one digest yielding one valid file, a pre-existing mismatched file failing without being overwritten, symlink and non-regular roots and targets failing, no `.tmp-` file surviving a successful or failed write, a hard-link fork producing the same inode with two links, and Unix mode checks for `0700` directories and `0600` files. Add `data_dir` tests for the sibling path and lazy creation. The test suite must use temporary directories only.

### Milestone 2: images end to end

Add `content_id: Option<String>` to `ImagePart` with `#[serde(default, skip_serializing_if = "Option::is_none")]`. Add `#[serde(default, skip_serializing_if = "String::is_empty")]` to `data_base64`, because a reference-bearing record omits it. Do not change any other field name, ordering, or type. Do not add a `content_id` field to `FunctionCallOutputData`; its images are already `ImagePart` values, and its output text stays inline.

Update every conversion between `SessionRecord`, `StreamRecord`, `RetainedRecord`, and `ConversationItem` so the field survives session-record round trips; the replay projection below is the deliberate exception and clears it. The compiler finds the literal constructions in tests; add `content_id: None` where the test is about legacy inline behavior. Add one snapshot per new shape: a message with an image reference, a function-call output whose images use references, and a compaction record whose retained tail uses references. Existing snapshots must not change.

Add `BlobStore::externalize_record(&SessionRecord) -> anyhow::Result<SessionRecord>`. It walks every image on a message and on a function-call output, decodes `data_base64` when present, computes the digest, writes the blob when the record has no id, verifies the digest when an id is present, then returns the image with `data_base64` removed and `content_id` set. An image with neither an inline payload nor an id is a hard error naming the record context; it is never returned unchanged. The method is idempotent: a second call verifies the existing reference and does not write a second blob. Do not externalize `FunctionCallOutputData.output` in this task.

Change `SessionWriter::new` to take `Arc<BlobStore>` as well as the file. `SessionWriter::append_record` clones the record, externalizes the clone, serializes the clone, and appends it while the writer mutex is held. The writer already owns the locked append handle for its lifetime, so the blob publication and the record append cannot interleave with another writer on the same session. A blob error becomes the same terminal writer failure as a serialization error, so the agent loop stops instead of appending a record that points at a payload it could not store. Stream-json is serialized by `AgentObserver` before the persistence callback, so it keeps the inline shape.

Add one shared content-id-stripping projection over `SessionRecord` and `StreamRecord`. It clears `content_id` on message images, function-call-output images, and checkpoint retained images, and is used by replay and any hydrated path that serializes a stream record. Do not rely on the message and output walk alone; `StreamRecord::Compaction` carries retained images.

Hydrate on the read side. Add `BlobStore::hydrate_session(&self, session_id: &str, &mut [SessionRecord]) -> anyhow::Result<()>` that validates the root, walks every message image, every function-call-output image, and every retained image inside a checkpoint, resolves each reference with `read_verified`, and restores `data_base64`. If both an inline value and an id are present, verify that the inline bytes hash to the id and keep the inline value. An image with neither inline bytes nor an id is an error, so an empty image cannot reach a provider. Errors name the session id, the digest, and whether the root, blob, or bytes are missing, unreadable, non-regular, or mismatched.

Wire hydration into `DataDir::load_session` and `DataDir::load_latest_session`. Both use the UUID from the session filename to derive the blob directory and reject a loaded session whose header `session_id` differs from that UUID. Leave `Session::load` and `Session::load_from_append_handle` as raw readers, because the checkpoint verb uses the latter only for identity, boundary, and model resolution and must not require image blobs. Document on both raw readers that callers building provider input must hydrate first; today every provider-bound load goes through `DataDir`. In `src/cli/replay.rs`, hydrate the parsed records with the data directory's store before emitting them, and use the shared projection to clear `content_id` from emitted stream records, including checkpoint retained images. A resolution failure is a replay `corrupt` error with exit 1 and the session id and digest in the message.

Update `src/cli/persistence.rs` and `src/cli/session_factory.rs` for forks. Extend `SessionPersistencePlan::Create` with `blob_source: Option<Uuid>`; `forked_client_and_session` sets it to `restored.id`, and a plain create sets `None`. Before `create_seeded_session_file` writes, compute the digests referenced by the seed records, hard-link each one from the parent store with `link_verified`, and then externalize any images that remain inline in legacy seed records into the child store. A missing or corrupt parent blob fails before the child file exists. A failure at any point after the fork begins removes the incomplete child file and any blob directory this fork created; removing that directory only drops the new hard links. Pass the child store to `SessionWriter::new` after the seed write.

Milestone 2 is done when an offline CLI test with a mock provider reads an image, writes a session whose image record has a content id and no `data_base64`, stores exactly one blob under `<sessions dir>/<uuid>.blobs/`, resumes the session successfully, and fails before any provider request when the blob is removed or corrupted. The test uses `tests/support::TestEnv`, `wiremock`, and a temporary `CAKE_DATA_DIR`; it makes no paid model call.

### Milestone 3: checkpoints

Add `BlobStore::externalize_checkpoint(&self, &CompactionData) -> anyhow::Result<CompactionData>` that returns a copy whose retained message images and retained function-call-output images are externalized. An image with neither inline bytes nor an id is a hard error. Leave retained output text inline. Hydration from Milestone 2 already covers retained images.

Change `SessionWriter::commit_checkpoint` to validate the structure, clone the checkpoint, externalize the clone, and then call `to_record_bytes` inside the existing critical section. The record that is written is exactly the record that was measured, and an externalization error terminally fails the writer. `CompactionData::to_record_bytes` keeps its current signature and ceiling check; callers are responsible for externalizing first.

In `src/cli/checkpoint.rs`, externalize the parsed checkpoint before the pre-validation call to `to_record_bytes`, so a payload whose built record would exceed eight MiB is measured with references rather than inline bytes. The writer re-externalizes and re-verifies under its lock. Preserve the existing ordering: lock, read, derive boundary, build, externalize, validate, commit. The stdin cap stays at eight MiB; a payload that does not fit is still rejected before parsing.

Milestone 3 tests must cover:

- a checkpoint that retains two maximum-size referenced images serializes under eight MiB, while the same checkpoint inline exceeds eight MiB and is rejected;
- a checkpoint supplied through the stdin verb with a payload just under the eight-MiB cap but a built record above the ceiling is accepted after externalization;
- a fork of a reference-bearing session links the referenced blobs and does not create a second copy;
- replay of a reference-bearing session emits inline image bytes and no `content_id`;
- a legacy session with inline images and no ids loads, resumes, and is rewritten with references only for newly appended records without changing existing message content.

### Milestone 4: recovery and read-only diagnostics

Add one shared traversal on `SessionRecord` that visits every content id: message images, function-call-output images, and the retained records inside a checkpoint. Recovery and diagnostics must share it so a future payload-bearing record cannot be missed by one and forgotten by the other.

Implement `BlobStore::recover_unreferenced(session_id, referenced: &BTreeSet<ContentId>) -> RecoveryReport`. If the blob directory does not exist, return an empty report without creating it. Validate the root with the shared no-follow check; a symlink or non-directory fails without deleting anything. Otherwise enumerate the directory without following symlinks; remove a regular file whose name is a 64-character lowercase hex digest not in `referenced`, and remove a regular file whose name starts with `.tmp-`. Ignore symlinks, directories, unknown names, and every referenced blob. If the reference set could not be collected completely, delete nothing. Never touch the session file, and never log payload bytes. Removal is best-effort: report an error but do not fail the resume, because this is garbage collection, not correctness.

Wire recovery into the `SessionPersistencePlan::Append` arm of `execute_persistence_plan`: after `open_session_for_append` has acquired the lock and repaired a torn tail, load the session through `Session::load_from_append_handle`, collect the referenced digests, and call `recover_unreferenced`. Skip the scan entirely when the blob directory does not exist, so a legacy session pays nothing. Do not run recovery for a create or fork.

Add `DataDir::blob_diagnostics(&self) -> anyhow::Result<BlobDiagnostics>` for a read-only report, with the report types in `blob_store.rs`. It pairs each session file with its blob directory by replacing the `jsonl` extension with `blobs`, so a malformed filename still gets a paired directory, parses each session with the tolerant JSONL framer, collects referenced digests, and compares them with the regular 64-hex files in that directory. A `.blobs` directory with no matching session file is reported as an orphaned session store. Validate every paired root with the shared no-follow check; a symlink or non-directory is a diagnostic, never a traversal. Report per session: blob count, logical bytes stored, referenced bytes, unreferenced count, and missing or corrupt references with session ids and digests. Global byte totals deduplicate hard-linked inodes by device and inode, so a fork is not counted twice. A malformed or unreadable session becomes a diagnostic entry, not a failed report.
Payload bytes are never included.

Wire a read-only `cake debug blobs [--json]` command through `src/cli/debug.rs` and `src/main.rs`. Text output is a short per-session summary plus totals. JSON adds a `data.blobs` object with the counts and a `checks` array with one diagnostic per problem. The command never deletes, never creates a directory, never follows a symlink, never prints payload bytes, and never starts a provider request. No destructive CLI exists in this task.

Milestone 4 tests must cover: an unreferenced blob from an interrupted append is removed at the next resume; a referenced blob survives; a stale temporary file is removed; a read-only load leaves an orphan untouched; a missing referenced blob is reported and not recreated; a corrupt referenced blob is reported and not deleted; a symlink in the blob directory is ignored; a blob directory with no session file is reported and left in place; a session with no blob directory reports zero; and the diagnostic JSON is stable and payload-free.

### Milestone 5: documentation, snapshots, and gates

Update `docs/integrations.md` under "Persisted sessions" and "Record semantics": describe the per-session blob directory, the `content_id` field, the omission of `data_base64` on externalized images, hydration and fail-closed behavior, checkpoint-size accounting, fork hard links, resume-time recovery, replay behavior, and the `cake debug blobs` report. State that the session format version remains 4, that a session is now a JSONL file plus an adjacent blob directory, and that a reference-bearing session requires a binary with this support. State that tool-output externalization is deferred.

Update `docs/security.md` in "Sessions, logs, and telemetry": blobs have the same sensitivity as transcripts, are owner-only, reject symlinks and non-regular files, are never logged, and live under the sessions root that is already granted read-only to sandboxed commands.

Update `docs/configuration.md` where `CAKE_DATA_DIR` and the default data roots are described, adding the `<uuid>.blobs/` sibling layout. Update `ARCHITECTURE.md` in the persistence boundary and invariants: image payloads are content-addressed per session and verified, provider wire shapes are unchanged, and a copied session must carry its blob directory.

Add the new record snapshots and regenerate any existing snapshot that changes only because a new optional field is present. Existing snapshots must not change when the field is absent. Run `just session-metrics-check` after inspecting the Python consumers under `scripts/session-metrics/`; they must ignore the new field and the new debug surface. Confirm `docs/adr/index.md` remains generated and gitignored.

## Concrete Steps

All commands run from `/Users/travisennis/Projects/cake/cake` unless stated otherwise. The work stays on `master` because it is one task; if the change needs isolation, use `just branch feat/content-address-images` first.

Milestone 1:

```
cargo test config::blob_store
cargo test config::data_dir
cargo fmt --check
```

Expected: the new unit tests pass, the existing data-directory tests still pass, and formatting is clean. The tests create temporary roots; nothing is written under the real home directory.

Milestone 2:

```
cargo test conversation
cargo test session
cargo test replay
cargo test --test session_images
cargo fmt --check
```

Expected: the new `tests/session_images.rs` test passes. Its assertions include that a session line contains `"content_id"` and does not contain the base64 fixture, that exactly one file exists under `<temp data dir>/sessions/<uuid>.blobs/`, that resume succeeds, and that removing the blob makes the next resume exit nonzero with the session id and digest in stderr while the mock provider's request count stays unchanged. A fork assertion compares the parent and child blob metadata and confirms the same inode and a link count of at least two.

Milestone 3:

```
cargo test checkpoint
cargo test compaction
cargo test --test session_images
cargo test --test session_modes
cargo test --test resume_repair
cargo fmt --check
```

Expected: all pass. Add a test that constructs a checkpoint whose inline image payload exceeds eight MiB and whose externalized form is accepted, and print the measured byte counts with `--nocapture` so the acceptance evidence is visible.

Milestone 4:

```
cargo test config::blob_store
cargo test --test debug_session
cargo test --test diagnostic_json
cargo run -- debug blobs --json
```

Expected: the recovery and diagnostics tests pass, and the debug command returns a JSON document whose `data.blobs` object contains counts only. To exercise it without touching real data, set a temporary root:

```
CAKE_DATA_DIR=/tmp/cake-blob-check cargo run -- debug blobs --json
```

Milestone 5:

```
git add docs/adr/061-content-address-session-images-in-per-session-blob-directories.md \
  docs/exec-plans/active/content-address-session-images.md \
  docs/integrations.md docs/security.md docs/configuration.md ARCHITECTURE.md
just docs-check
just session-metrics-check
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
just check
git diff --check
```

Expected: every command exits 0. `just docs-check` must see the new Markdown as tracked, so stage it first. `just check` is the final gate and includes the repository's standard test, lint, dependency, and documentation checks. If a gate cannot run because of an environment prerequisite, record the exact reason and the narrower checks that did run; do not describe it as passing.

## Validation and Acceptance

The change is accepted when all of the following are observable:

1. **Storage once per session.** Run `cargo test --test session_images -- --nocapture`. The test prints the temporary session path and blob path. The JSONL image record contains `content_id` and no `data_base64`; the blob file exists once under `<sessions dir>/<uuid>.blobs/<digest>`. A checkpoint that retains the same image references the same file, not a second copy.

2. **Fork and resume.** Fork a reference-bearing session and inspect the child JSONL. The child conversation records contain the same `content_id` and no inline bytes. The child blob directory contains hard links to the parent's inodes; the blob count and logical bytes are unchanged. Resuming either session reaches the mock provider with the image present.

3. **Fail closed.** Remove a referenced blob, then resume. Cake exits nonzero before the mock provider receives a request, and stderr contains the session id and digest. Corrupt the blob with different bytes; the same happens with a digest-mismatch diagnostic. No empty or truncated payload is sent.

4. **Legacy compatibility.** Load a fixture with inline images and no content ids. It loads and resumes exactly as before. Newly appended image records externalize, while existing inline records stay readable.

5. **Checkpoint ceiling.** A checkpoint whose retained images (two maximum-size images, for example) are inline would exceed eight MiB and be rejected; the same checkpoint after externalization commits, and `cake replay` shows the retained images. The serialized checkpoint record is below `MAX_CHECKPOINT_RECORD_BYTES`. A stdin checkpoint just under the eight-MiB payload cap whose built record would exceed the ceiling is accepted after externalization.

6. **Recovery safety.** Simulate an interrupted append by writing a blob with no referencing record. The next resume removes that blob and any stale temporary file. A referenced blob survives. A read-only load leaves the orphan untouched, and `cake debug blobs` reports it.

7. **Diagnostics.** `cake debug blobs --json` reports per-session blob count, bytes, referenced and unreferenced counts, and missing or corrupt references without payload bytes or provider activity. Global totals do not double-count hard-linked fork blobs.

8. **Documentation and snapshots.** `just docs-check`, `just session-metrics-check`, and the snapshot tests pass, and the updated docs match the implemented storage, compatibility, and failure behavior.

No paid model calls are required or permitted for this task.

## Idempotence and Recovery

All tests use temporary `CAKE_DATA_DIR` roots and can be rerun safely. A failed blob write leaves at most a temporary file in the session's blob directory; the next resume removes it, and the next write uses a new temporary name. A failed append after a successful blob write leaves an unreferenced blob, which the next resume removes after the torn-tail repair. A failed fork seed write removes the incomplete child file and its blob directory; the parent's blobs and links are unaffected.

Reusing an existing digest never overwrites it. If the existing file does not match the digest, the write fails and the operator must inspect or remove the corrupt file; the code must not guess which copy is correct. Removing a corrupt or missing referenced blob makes the next load fail closed; restore the file from the session backup pair (`<uuid>.jsonl` plus `<uuid>.blobs/`). The implementation never regenerates a missing payload from another record, because the raw record and the checkpoint are the same reference, not redundant copies.

The implementation does not rewrite legacy session files. A legacy session is externalized only when a new image record is appended or the session is forked. A session backup or move must carry both the JSONL file and its blob directory; copying only the JSONL leaves references unresolvable.

## Artifacts and Notes

Expected externalized image record shape:

```
{"type":"message","role":"user","content":"look","images":[{"media_type":"image/png","content_id":"<64 lowercase hex>"}]}
```

Expected blob path:

```
<sessions dir>/<session-uuid>.blobs/<64-hex-digest>
```

Expected missing-blob diagnostic:

```
session 550e8400-e29b-41d4-a716-446655440000: referenced image <digest> is missing at <path>; refusing to build a provider request
```

Expected checkpoint-size evidence from the acceptance test:

```
inline checkpoint: N bytes (rejected above 8388608)
externalized checkpoint: M bytes (accepted)
```

Expected fork evidence: the parent and child blob files share an inode, the child link count is at least two, and no image bytes are rewritten. The exact wording of diagnostics may change, but every diagnostic must contain the session id and digest and must never contain payload bytes.

## Interfaces and Dependencies

By the end, these interfaces must exist at stable paths:

- `crate::config::blob_store::ContentId` with `from_bytes(&[u8]) -> Self`, `parse(&str) -> anyhow::Result<Self>`, and `as_str(&self) -> &str`.
- `crate::config::blob_store::BlobStore` with `new(PathBuf) -> Self`, `root(&self) -> &Path`, `put(&self, &[u8]) -> anyhow::Result<ContentId>`, `read_verified(&self, &ContentId) -> anyhow::Result<Vec<u8>>`, `link_verified(&self, &BlobStore, &ContentId) -> anyhow::Result<()>`, `externalize_record(&self, &SessionRecord) -> anyhow::Result<SessionRecord>`, `externalize_checkpoint(&self, &CompactionData) -> anyhow::Result<CompactionData>`, `hydrate_session(&self, &str, &mut [SessionRecord]) -> anyhow::Result<()>`, and `recover_unreferenced(&self, &str, &BTreeSet<ContentId>) -> anyhow::Result<RecoveryReport>`.
- A shared content-id-stripping projection over `SessionRecord` and `StreamRecord` that covers message images, function-call-output images, and checkpoint retained images.
- `crate::config::DataDir::session_blob_dir(&self, Uuid) -> PathBuf`, `session_blob_store(&self, Uuid) -> BlobStore`, and `blob_diagnostics(&self) -> anyhow::Result<BlobDiagnostics>`.
- `crate::config::SessionWriter::new(File, Arc<BlobStore>) -> Self` and the existing `append_record` and `commit_checkpoint` methods, with image externalization performed inside their write critical sections.
- `crate::types::conversation::ImagePart.content_id: Option<String>`; `FunctionCallOutputData` gains no top-level content id in this task.
- `crate::cli::SessionPersistencePlan::Create { seed_records, blob_source: Option<Uuid> }`.
- `crate::cli::debug` gains a read-only `blobs` command whose JSON document is a `data.blobs` object plus diagnostic `checks`.

The implementation uses only crates already declared in `Cargo.toml`: `sha2` for SHA-256, `hex` for the digest text, `base64` for image encoding, `fs4` for the existing session-file advisory lock, and `anyhow` for errors. No new database, compression, remote storage, or cross-session lock dependency is introduced. The dependency direction stays `cli -> clients/config/prompts/types`; `types` does not import `config` or `cli`, and the blob-store traversal of record types lives in `config`.
