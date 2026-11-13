---
status: accepted
date: 2026-11-12
decision-makers: Travis Ennis
---

# Content-address session images in per-session blob directories

## Context and Problem Statement

Format-4 session JSONL embeds every image as base64 in `ImagePart.data_base64`. Those bytes are part of the conversation record, so every checkpoint that retains the exchange copies them again, and the eight-MiB serialized-checkpoint ceiling counts them. A single maximum-size image is roughly seven MB of base64, about eighty percent of the ceiling. Nothing verifies the bytes before they reach a provider request, so a corrupted or truncated base64 payload can be sent to the model.

Images are duplicative by nature. The same screenshot appears in the raw message, in every checkpoint retained tail that keeps it, and in a fork of the session. A checkpoint is a self-contained, sanitized projection: a summary plus a retained tail that replaces retired history. It must stay self-contained, because restore must not re-interpret lifecycle records from the parent log. Externalizing the image bytes keeps that invariant while removing them from the checkpoint ceiling.

This decision covers images only. Tool-output text stays inline: at current defaults Bash and MCP cap output at 50 KB, Read at 100 KB, and the maximum retained image dwarfs them. Durable externalization of large tool output belongs to the between-turn compaction work (034f) or a follow-up; the same reference shape can be added beside `FunctionCallOutputData.output` later.

The session format is a compatibility surface. Existing format-4 files must continue to load with their inline bytes. Provider wire shapes must not change: images still become data URLs at request construction. This decision therefore adds an optional reference beside the existing inline field rather than replacing the format.

## Decision Drivers

- Keep session format version 4 and load every existing session unchanged.
- Keep a session self-contained as a filesystem pair: `<uuid>.jsonl` plus `<uuid>.blobs/`, so backup, copy, and deletion operate on one unit.
- Store each image once per session: the raw record and every checkpoint retained tail share one blob; a fork shares the parent's blobs by hard link.
- Remove referenced image bytes from the serialized checkpoint size.
- Fail closed when a referenced image is missing or corrupt, before any provider request, with a diagnostic naming the session and digest.
- Avoid a global namespace, a cross-session lock, and a time-based garbage collector.
- Keep payload storage local, inspectable, owner-only, and consistent with the existing session-file safety rules.
- Keep stream-json and provider request shapes unchanged.
- Avoid a new database, compression layer, or remote service.

## Considered Options

- **Keep everything inline (status quo).** Simple and self-contained, but duplicates bytes into checkpoints, consumes the ceiling, and performs no integrity check.
- **Bump to a new session format that requires references.** Clean storage model, but forces a migration and makes every new session unreadable by the current binary generation. It is unnecessary because the record vocabulary already tolerates additive optional fields.
- **Add an optional digest reference beside the inline image field (chosen for the record shape).** Old records keep loading; new records can externalize images; readers that understand references verify and resolve them.
- **Store referenced images in a per-session blob directory beside the session file (chosen for storage).** Deduplicates within a session and across forks through hard links, needs no sweep or store lock, and keeps the session backup unit intact. It deliberately gives up deduplication between unrelated sessions.
- **Global content-addressed store with a shared/exclusive lock and orphan sweep (rejected).** Deduplicates across sessions, but adds a cross-session lock, a time-based sweep, a new sandbox grant, and a session that is no longer self-contained. fx rejected the same shape in favor of per-session blobs (D6), and pi's durable backend keeps payloads with their session and recovers unconfirmed payloads when the writer opens. Cross-session image reuse is rare enough that the machinery is not justified.
- **Head-pointer compaction instead of a self-contained checkpoint (rejected for this change).** Pi and fx store a summary plus the first kept entry and leave older records in the log. Cake's checkpoint is a sanitized, self-contained projection; reinterpreting raw lifecycle records on restore would change the format contract and validation surface. Keep the projection and externalize its images instead.
- **Store payloads in SQLite or another metadata database.** Adds a dependency and makes individual payloads harder to inspect, back up, and delete with ordinary filesystem tools.
- **Compress inline payloads.** Reduces some size but does not deduplicate, does not provide integrity, and adds CPU cost to every read and write.

## Decision Outcome

Chosen: an optional SHA-256 reference on images, backed by a content-addressed blob directory that lives beside the session file.

### Record shape

`ImagePart` gains an optional lowercase 64-character `content_id` holding the SHA-256 of the image's raw bytes. The existing `data_base64` field remains in the schema so legacy records load unchanged. A record that carries a reference omits `data_base64`; a reader that sees both verifies that the inline bytes match the digest. New writers never persist both.

`content_id` is a persistence field. It rides on the shared `ImagePart` type, but provider conversion reads only the hydrated bytes, and stream-json and replay projections omit the reference. Newly persisted images are externalized; legacy inline images remain inline until the session is rewritten by a new append or a fork. An image with neither inline bytes nor a reference is rejected before persistence and on read, so an empty image cannot reach a provider. `FunctionCallOutputData.output` stays inline in this task; images attached to a function-call output use the same `ImagePart` field.

### Storage layout

Blobs live beside the session file under the durable sessions root:

```text
~/.local/share/cake/sessions/<session-uuid>.blobs/<digest>
{CAKE_DATA_DIR}/sessions/<session-uuid>.blobs/<digest>
```

`<digest>` is the 64-character lowercase hexadecimal SHA-256, with no shard directory: the namespace holds one session's images. `DataDir` owns `session_blob_dir(id)` and `session_blob_store(id)`. The sessions root is already granted read-only to sandboxed commands, so the blob directory inherits that access and needs no new configuration. The directory is created lazily; a session with no externalized image has no blob directory.

Every operation validates the blob directory itself with `symlink_metadata`; a symlink or non-directory is refused before any read, write, link, recovery, or diagnostic traversal. The directory is created `0700` and files `0600`, independent of the process umask. Final files are opened with no-follow semantics on Unix. A newly created blob directory is synced in its sessions parent so the directory entry survives a crash. Temporary files use the recognizable `.tmp-` prefix; no digest or unknown file matches it. Payload bytes are never logged.

### Publication: payload before marker

Writing a record follows the same ordering as pi's JSONL backend: the payload is durable before the record that references it.

1. If the blob directory is missing, create it owner-only and sync the sessions parent so the new directory entry is durable. Then write a temporary file named with the `.tmp-` prefix in the blob directory with `create_new`, owner-only mode, the payload bytes, then `sync_all`.
2. Publish it at `<digest>` with a no-replace hard link, sync the blob directory so the name is durable, then remove the temporary file. A crash before publication leaves only a temp file; a crash after publication leaves a complete blob.
3. If `<digest>` already exists, open it without following symlinks, require a regular file, hash the bytes, and compare. A mismatch is an error naming the session and digest; the file is never overwritten.
4. Only then append the session record. Ordinary records are flushed; a checkpoint is additionally synced, so the payload is always durable before the record that references it.

The session writer externalizes while it owns the locked append handle, before serialization. A crash between steps leaves an unreferenced blob, never a record that references a missing payload.

No separate store lock is needed. The existing per-session append lock serializes writers; two writers racing on one digest are resolved by the no-replace link and verification.

### Load and hydrate

`DataDir::load_session` and `load_latest_session` hydrate every image: validate the session's blob directory, resolve the reference, read the blob without following symlinks, verify SHA-256, and restore `data_base64`. Both reject a session whose header id differs from the UUID in its filename before deriving the blob directory. A missing, unreadable, non-regular, or mismatched blob is an explicit error naming the session and digest. An image with neither inline bytes nor a reference is also an error, so an empty image cannot reach a provider. No provider request is made and no empty or truncated payload is substituted. Legacy inline-only records load without a blob read.

Raw readers that need only identity, boundary, or model data keep their current behavior; the checkpoint verb's read of the existing session does not require image blobs. Replay hydrates before emitting and uses one shared stream projection that drops `content_id` from message images, function-call-output images, and checkpoint retained images. The same projection is available to any hydrated live path; live stream-json is serialized before externalization, so its existing inline shape is unchanged.

### Checkpoint behavior

`SessionWriter::commit_checkpoint` externalizes every retained image before `to_record_bytes` and the eight-MiB check, while holding the writer mutex. An image with neither inline bytes nor a reference is a hard error at this point. The written record is exactly the record that was measured. `cake sessions checkpoint` externalizes retained images before its pre-validation call to `to_record_bytes`; the writer re-verifies under its lock. The stdin payload cap remains eight MiB, so an external compactor can externalize a checkpoint whose built record would exceed the ceiling only when the incoming payload itself fits the cap.

Tool-output text remains inline and still counts toward the ceiling. A pathological text-only retained tail can still exceed eight MiB; the existing rejection remains the backstop, and a later change can add the same reference beside the output text.

### Fork

A fork validates both blob roots, creates the child's directory as needed, and hard-links exactly the digests referenced by its seed records, after verifying each parent blob. It verifies each destination after linking, syncs the child blob directory so the links are durable, then externalizes any inline images in legacy seed records into the child directory and writes the child file under one lock. A missing or corrupt parent blob fails the fork before the child file exists. Any failure after the fork begins removes the incomplete child file and any blob directory this fork created; removing that directory only drops the new hard links, not the parent's.

A fork no longer copies image bytes, and deleting either session's blob directory later does not affect the other. Only digests present in the copied seed records are linked; blobs referenced solely by retired history stay behind.

### Recovery and diagnostics

On resume, after acquiring the locked append handle and repairing a torn tail, the writer scans the session's records, collects referenced digests with a shared traversal (message images, function-call-output images, checkpoint retained images), and removes regular 64-hex-named files in the session's blob directory that are not referenced, plus stale `.tmp-` files. It validates the blob directory with the shared no-follow check, never removes a referenced blob, and never touches the session file. If the reference scan fails, it deletes nothing. A created or forked session does not run this cleanup, and read-only loads never delete.

A referenced-but-missing blob is not recreated; hydration fails closed. `cake debug blobs [--json]` is read-only: it pairs each session file with its blob directory and reports blob count, bytes, referenced and unreferenced counts, missing or corrupt references with session ids and digests, and a blob directory left behind with no matching session file. It never deletes, never creates a directory, and never prints payload bytes. Global byte totals deduplicate hard-linked inodes so a fork is not counted twice.

`BlobStore` uses one shared directory validator for `put`, `read_verified`, `link_verified`, recovery, and diagnostics; `O_NOFOLLOW` alone protects only the final path component. There is no time-based sweep, no store-wide lock, and no destructive CLI in this task. `cake debug blobs` is read-only. If a future `sessions delete` or prune command lands, it must remove `<uuid>.blobs` together with the session file.

### Consequences

- Good: an image is stored once per session, shared by the raw record and every retained checkpoint tail; a fork shares the parent's bytes through hard links.
- Good: the serialized checkpoint counts a digest instead of an image, and a corrupt or missing payload fails before a provider request.
- Good: the session pair is an explicit backup unit, no new sandbox grant is needed, and there is no global collector to schedule or lock.
- Good: format version 4 and all provider wire shapes remain unchanged.
- Bad: there is no deduplication between unrelated sessions; an image reused across many sessions is stored once per session. This is the deliberate cost of dropping the global store.
- Bad: a session is now two filesystem entries. Copying or moving only the JSONL breaks its references until the blob directory is restored alongside it.
- Bad: a binary without this feature cannot load a reference-bearing session because the inline field is absent. That is an explicit forward-compatibility break, preferred over silently sending empty content. It follows the existing precedent that a checkpoint-bearing session needs a checkpoint-aware binary.
- Bad: an orphan blob from a crashed append persists until that session is next resumed, and read-only commands will report it until then.
- Bad: tool-output text still counts toward the checkpoint ceiling; image-only scope does not eliminate pathological text-only overflow.

## More Information

- Task 145, scoped to images; large tool-output externalization is deferred to 034f or a follow-up.
- ADR 047, core-native compaction, which owns the checkpoint ceiling and the between-turn compaction design.
- ADR 058, replace files through a staged atomic rename, for the write-once and atomic-publication posture.
- fx's session manager (D6) for per-session blobs, hard links on fork, and deletion with the session.
- pi's durable JSONL backend (spec §11.3) for payload-first publication, recovery of unconfirmed payloads at writer open, and read-only opens that never repair.
- `docs/integrations.md`, persisted sessions and checkpoint append contracts.
- `docs/security.md`, session and payload storage sensitivity.
- `ARCHITECTURE.md`, persistence and integration boundaries.
- Amendment (2026-11-12, task 145a): the same reference shape now externalizes large tool-output text; see below.

## Amendment: externalize large tool-output text (2026-11-12)

The original decision covered images only and explicitly deferred tool-output text. Task 145a extends it without changing the image contract: an optional lowercase SHA-256 `content_id` may now sit beside `FunctionCallOutputData.output` in the same per-session blob directory, under the same publication, hydration, fork, recovery, and diagnostic rules.

A read-only scan of the live store on 2026-11-12 found 2,597 sessions and 175,118 `function_call_output` records holding 552.5 MB of inline text. A 16 KiB boundary captures 25.4% of those output bytes and about 9.5% of raw JSONL bytes in 5,552 outputs, roughly two per session. A 32 KiB boundary captures 6.5% of output bytes, while 8 KiB captures 52% but creates 3.4 times as many blobs. fx uses the same 16 KiB `large_result_threshold_bytes` for its tool-result store, and 16 KiB is about 4k tokens, one fifth of Cake's 20k-token retained target.

### Decision

Externalize a `function_call_output` when its UTF-8 text length is at least 16 KiB (16,384 bytes). Shorter text stays inline. The threshold is a constant, `TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES` in `src/config/blob_store.rs`, not a settings key in this task.

The record shape is an optional lowercase 64-character `content_id` beside the existing `output` field. An externalized record serializes `"output": ""` and the reference, so the field set stays stable for stream consumers and older readers that ignore unknown optional fields. A record carries either non-empty inline text or a reference, never both. An empty output with no reference is a valid empty tool result, because tools such as Bash can legitimately return no text. New writers never persist both forms.

Publication is payload-before-marker, exactly as for images: write and verify the blob, then append the record. Hydration reads the blob, verifies the digest, and restores the text before request construction. A record with both an inline value and an id must have inline bytes that hash to the id. A missing, unreadable, non-regular, mismatched, or non-UTF-8 blob fails the load before any provider request with the session id and digest; Cake never substitutes empty text. Stream-json and replay keep their pre-blob shape: live stream records are serialized before persistence, and replay hydrates then omits `content_id`.

`SessionWriter::append_record` externalizes ordinary records under its lock. `SessionWriter::commit_checkpoint` and `cake sessions checkpoint` externalize retained output text before the eight-MiB measurement, so a checkpoint counts a digest rather than the text. The checkpoint verb's eight-MiB stdin cap is unchanged: a payload at the cap can produce a smaller record, but a payload above the cap is still rejected.

A fork hard-links every digest its seed records reference, images and outputs alike, then externalizes any eligible inline output into the child store. Resume-time recovery and `cake debug blobs` use the existing shared content-id traversal, so an unreferenced output blob is removed like an unreferenced image blob and a missing or corrupt output reference is reported the same way.

### Consequences

- Good: the raw record, every checkpoint retained tail, and a fork share one copy of a large output instead of duplicating its text.
- Good: a checkpoint that retains a large output stays under the eight-MiB ceiling without weakening the self-contained projection.
- Good: a corrupt or missing output payload fails before a provider request rather than sending empty text.
- Good: session format version 4, provider wire shapes, and stream-json remain unchanged; legacy inline records load without a blob read.
- Bad: a session with large outputs is now two filesystem entries, and copying only the JSONL breaks its references until the blob directory is restored.
- Bad: a binary without this feature cannot load a reference-bearing `function_call_output`, because the text is absent. That is the same explicit forward-compatibility break the image decision accepted.
- Bad: outputs below 16 KiB still count toward the checkpoint ceiling. The threshold trades fewer blob files for less deduplication of medium outputs.
- Bad: every provider-bound load reads and hashes each referenced output. The cost is bounded by the number of large outputs in the session and is small relative to the provider request.

### More Information

- Task 145a and `docs/exec-plans/active/externalize-large-tool-outputs.md` for the delivery plan, measured threshold evidence, and verification.
- The original image decision above remains in force; this amendment only widens the payload kinds that can use the same store.
- Cake task 034f consumes the reference path for retained output text; it does not depend on the threshold value.
