//! Content-addressed payload storage for one persisted session.
//!
//! Each session owns a sibling `<uuid>.blobs` directory. A blob's name is the
//! lowercase SHA-256 digest of its decoded bytes. Publication is write-once:
//! callers upload a temporary file, hard-link it into place without replacing
//! an existing digest, and verify the winner before a session record refers to
//! it.
//!
//! The store deliberately has no cross-session lock or sweep. The session's
//! append lock serializes writers, and recovery is scoped to one session.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use base64::Engine as _;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::config::owner_only;
use crate::config::session_jsonl::{FramedSessionLine, SessionFramer};
use crate::types::ImagePart;
use crate::types::compaction::{CompactionData, RetainedConversationRecord};
use crate::types::session::{FunctionCallOutputData, SessionRecord};

/// Prefix reserved for temporary files inside a session blob directory.
///
/// A digest is always 64 lowercase hexadecimal characters, so recovery can
/// remove `.tmp-*` files without risking a published blob or an unknown file.
const TEMP_PREFIX: &str = ".tmp-";

/// Tool outputs at or above this size move to the session blob store and are
/// replaced by a bounded summary in live provider requests.
///
/// A 2,597-session scan put 5,552 outputs at or above 16 KiB (140.5 MB, 25.4%
/// of all output bytes); an 8 KiB cutoff would move 52%. The point matches
/// `fx`'s `large_result_threshold_bytes`.
pub const TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES: usize = 16 * 1024;

/// Hard ceiling for one tool result retained in the live conversation and
/// session blob.
///
/// A producer may return at most [`TOOL_OUTPUT_PRODUCER_LIMIT_BYTES`] so its
/// footer or truncation marker still fits under this ceiling. The conversation
/// boundary replaces anything above it with a head/tail preview that says the
/// result was not retained.
pub const TOOL_OUTPUT_RETENTION_CEILING_BYTES: usize = 2 * 1024 * 1024;

/// Per-producer output cap used by Bash, `BashSession`, MCP, and toolbox tools.
///
/// The reserve covers metadata footers and truncation markers so the final
/// model-visible string stays below the hard retention ceiling.
pub const TOOL_OUTPUT_PRODUCER_LIMIT_BYTES: usize = TOOL_OUTPUT_RETENTION_CEILING_BYTES - 8 * 1024;

/// Total full-text bytes one session's canonical history may retain across
/// oversized tool results.
///
/// Results above this budget are replaced by a bounded preview with an explicit
/// not-retained marker before they enter canonical history, so the budget
/// bounds live memory rather than only the retrieval index.
pub const TOOL_OUTPUT_SESSION_BUDGET_BYTES: usize = 64 * 1024 * 1024;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A validated lowercase SHA-256 digest.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContentId(String);

impl ContentId {
    /// Compute the content id of `bytes`.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(hex::encode(Sha256::digest(bytes)))
    }

    /// Parse a 64-character lowercase hexadecimal digest.
    ///
    /// Uppercase characters, other lengths, and non-hex characters are
    /// rejected.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is not a lowercase SHA-256 digest.
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        if value.len() != 64 {
            bail!(
                "content id must be exactly 64 lowercase hexadecimal characters, got {} characters",
                value.len()
            );
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            bail!("content id must contain only lowercase hexadecimal characters");
        }
        Ok(Self(value.to_owned()))
    }

    /// Return the digest as lowercase hexadecimal text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Return whether `bytes` hash to this id.
    #[must_use]
    pub fn matches(&self, bytes: &[u8]) -> bool {
        Self::from_bytes(bytes) == *self
    }
}

impl std::fmt::Display for ContentId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for ContentId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// The content-addressed payload store for one session.
#[derive(Clone, Debug)]
pub struct BlobStore {
    root: PathBuf,
}

/// Whether the store root already exists or must be created lazily.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RootState {
    Existing,
    Missing,
}

/// What one append-time recovery pass removed.
#[derive(Debug, Default)]
pub struct RecoveryReport {
    /// Published blobs that no record in the session references.
    pub removed_blobs: Vec<ContentId>,
    /// Stale `.tmp-*` files left by interrupted publication.
    pub removed_temporary_files: Vec<PathBuf>,
    /// Best-effort removal failures. These never fail the resume.
    pub errors: Vec<String>,
}

/// One missing referenced blob.
#[derive(Debug, Serialize)]
pub struct MissingBlobReference {
    /// Referenced content id.
    pub digest: String,
    /// Expected blob path.
    pub path: PathBuf,
}

/// One referenced blob whose bytes do not verify.
#[derive(Debug, Serialize)]
pub struct CorruptBlobReference {
    /// Referenced content id.
    pub digest: String,
    /// Blob path that failed verification.
    pub path: PathBuf,
    /// Why verification failed. Never contains payload bytes.
    pub reason: String,
}

/// Read-only health for one session and its blob directory.
#[derive(Debug, Serialize)]
pub struct SessionBlobDiagnostics {
    /// Session identity derived from the session filename.
    pub session_id: String,
    /// Session JSONL path paired with this report.
    pub session_file: PathBuf,
    /// Sibling blob directory. It may be absent.
    pub blob_dir: PathBuf,
    /// Number of regular digest-named files in the blob directory.
    pub blob_count: usize,
    /// Logical bytes across those files, before global inode deduplication.
    pub stored_bytes: u64,
    /// Distinct content ids referenced by the session records.
    pub referenced_count: usize,
    /// Bytes in referenced blob files that exist, verified or not.
    pub referenced_bytes: u64,
    /// Regular digest-named files that no session record references.
    pub unreferenced_count: usize,
    /// Logical bytes in unreferenced files.
    pub unreferenced_bytes: u64,
    /// Stale `.tmp-*` files left behind.
    pub temporary_file_count: usize,
    /// Paths of stale temporary files.
    pub temporary_files: Vec<PathBuf>,
    /// Referenced digests with no regular digest-named file.
    pub missing_references: Vec<MissingBlobReference>,
    /// Referenced files whose bytes fail verification.
    pub corrupt_references: Vec<CorruptBlobReference>,
    /// A session read or parse problem. The report still covers what was read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_error: Option<String>,
    /// A blob-directory validation or read problem. No traversal was done.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory_error: Option<String>,
    /// False when an unread reference or malformed record makes the set
    /// incomplete. Recovery deletes nothing in that case.
    pub references_complete: bool,
}

/// One `.blobs` directory with no matching session file.
#[derive(Debug, Serialize)]
pub struct OrphanedBlobStoreDiagnostics {
    /// The unpaired blob directory.
    pub blob_dir: PathBuf,
    /// Number of regular digest-named files.
    pub blob_count: usize,
    /// Logical bytes across those files.
    pub stored_bytes: u64,
    /// Stale `.tmp-*` files left behind.
    pub temporary_file_count: usize,
    /// Paths of stale temporary files.
    pub temporary_files: Vec<PathBuf>,
    /// A directory validation or read problem. No traversal was done.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory_error: Option<String>,
}

/// Aggregate counts for every blob directory in a data directory.
#[derive(Debug, Serialize)]
pub struct BlobDiagnosticsTotals {
    /// Paired session reports.
    pub session_count: usize,
    /// Logical blob entries across paired sessions, before inode deduplication.
    pub blob_count: usize,
    /// Distinct blob files across paired sessions after inode deduplication.
    pub unique_blob_count: usize,
    /// Distinct stored bytes across paired sessions after inode deduplication.
    pub stored_bytes: u64,
    /// Distinct content ids referenced across paired sessions.
    pub referenced_count: usize,
    /// Distinct referenced bytes after inode deduplication.
    pub referenced_bytes: u64,
    /// Unreferenced blob entries across paired sessions.
    pub unreferenced_count: usize,
    /// Logical unreferenced bytes across paired sessions.
    pub unreferenced_bytes: u64,
    /// Stale temporary files across paired sessions.
    pub temporary_file_count: usize,
    /// Missing referenced digests across paired sessions.
    pub missing_reference_count: usize,
    /// Corrupt referenced digests across paired sessions.
    pub corrupt_reference_count: usize,
    /// Blob directories with no matching session file.
    pub orphaned_store_count: usize,
    /// Blob entries in orphaned stores.
    pub orphaned_store_blob_count: usize,
    /// Distinct orphaned-store bytes after inode deduplication.
    pub orphaned_store_bytes: u64,
}

/// Read-only report for every session blob directory under one data root.
#[derive(Debug, Serialize)]
pub struct BlobDiagnostics {
    /// One report per session file, sorted by filename stem.
    pub sessions: Vec<SessionBlobDiagnostics>,
    /// Blob directories with no session file, sorted by filename stem.
    pub orphaned_stores: Vec<OrphanedBlobStoreDiagnostics>,
    /// Aggregate counts. Byte totals deduplicate hard-linked files.
    pub totals: BlobDiagnosticsTotals,
}

impl BlobStore {
    /// Create a store rooted at `root`.
    ///
    /// Construction does not touch the filesystem. The directory is created
    /// lazily by the first write.
    #[must_use]
    pub const fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Store `bytes` and return their content id.
    ///
    /// The blob is published under its digest without replacing an existing
    /// file. If that digest already exists, its bytes are verified instead.
    ///
    /// # Errors
    ///
    /// Returns an error when the root or a target is unsafe, the data cannot be
    /// written, or an existing digest contains different bytes.
    pub fn put(&self, bytes: &[u8]) -> anyhow::Result<ContentId> {
        let id = ContentId::from_bytes(bytes);
        let root = self.ensure_root_for_write()?;
        let final_path = root.join(id.as_str());
        if self.existing_blob_verified(&final_path, &id)? {
            return Ok(id);
        }

        let temp_path = write_temp_blob(&root, bytes)?;
        let publish_result = self.publish_temp_blob(&root, &temp_path, &final_path, &id);
        let cleanup_result = fs::remove_file(&temp_path);
        publish_result?;
        cleanup_result.with_context(|| {
            format!(
                "published blob '{}' but failed to remove temporary file '{}'",
                id,
                temp_path.display()
            )
        })?;

        Ok(id)
    }

    fn existing_blob_verified(&self, final_path: &Path, id: &ContentId) -> anyhow::Result<bool> {
        match fs::symlink_metadata(final_path) {
            Ok(_) => {
                self.read_verified(id)?;
                Ok(true)
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(source)
                .with_context(|| format!("failed to inspect blob '{}'", final_path.display())),
        }
    }

    fn publish_temp_blob(
        &self,
        root: &Path,
        temp_path: &Path,
        final_path: &Path,
        id: &ContentId,
    ) -> anyhow::Result<()> {
        match fs::hard_link(temp_path, final_path) {
            Ok(()) => sync_dir(root).with_context(|| {
                format!(
                    "failed to sync blob directory '{}' after publishing '{}'",
                    root.display(),
                    id
                )
            }),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                self.read_verified(id).map(|_| ())
            },
            Err(source) => Err(source).with_context(|| {
                format!(
                    "failed to publish blob '{}' at '{}'",
                    id,
                    final_path.display()
                )
            }),
        }
    }

    /// Read a blob and verify its digest.
    ///
    /// The store root and final component are inspected without following
    /// symlinks. Missing, non-regular, unreadable, and mismatched blobs are
    /// distinct errors that always name the digest.
    ///
    /// # Errors
    ///
    /// Returns an error when the root or blob is missing or unsafe, the blob is
    /// not a regular file, or its bytes do not match `id`.
    pub fn read_verified(&self, id: &ContentId) -> anyhow::Result<Vec<u8>> {
        let root = self.validate_existing_root()?;
        let path = root.join(id.as_str());
        validate_blob_target(&path, id)?;
        let mut file = open_blob_for_read(&path, id)?;

        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .with_context(|| format!("failed to read blob '{}' at '{}'", id, path.display()))?;
        let actual = ContentId::from_bytes(&bytes);
        if actual != *id {
            bail!(
                "blob '{}' at '{}' has a digest mismatch (found {})",
                id,
                path.display(),
                actual
            );
        }
        Ok(bytes)
    }

    /// Remove unreferenced blobs and stale temporary files from this store.
    ///
    /// A missing root is a no-op and is never created. A symlinked or
    /// non-directory root fails without deleting anything. Only regular files
    /// named by a lowercase SHA-256 digest or the `.tmp-` prefix are removal
    /// candidates; symlinks, directories, unknown names, and every referenced
    /// digest are left untouched.
    ///
    /// Removal is garbage collection, not correctness: individual removal
    /// failures are collected in the report instead of failing the caller.
    ///
    /// # Errors
    ///
    /// Returns an error when the root cannot be inspected or enumerated. No
    /// files are removed in that case.
    pub fn recover_unreferenced(
        &self,
        session_id: &str,
        referenced: &BTreeSet<ContentId>,
    ) -> anyhow::Result<RecoveryReport> {
        let root = self.checked_root();
        match inspect_root_for_write(&root)? {
            RootState::Missing => return Ok(RecoveryReport::default()),
            RootState::Existing => {},
        }

        let entries = fs::read_dir(&root).with_context(|| {
            format!(
                "failed to read blob store directory '{}' for recovery of session {session_id}",
                root.display()
            )
        })?;
        let mut report = RecoveryReport::default();
        for entry in entries {
            recover_entry(entry, &root, session_id, referenced, &mut report);
        }
        report.removed_blobs.sort();
        report.removed_temporary_files.sort();
        Ok(report)
    }

    /// Return a copy of `record` whose message images, function-call-output
    /// images, output text, and checkpoint-retained payloads refer to blobs
    /// instead of carrying inline bytes.
    ///
    /// An image with neither inline bytes nor a content id is rejected. An
    /// image with both must name the digest of its inline bytes, and the blob
    /// must already verify. An output at or above
    /// [`TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES`] is published and
    /// replaced by its content id; a shorter output stays inline unless it
    /// already carries a reference. The original record is unchanged, so a
    /// live conversation can keep the bytes for the next provider request.
    ///
    /// # Errors
    ///
    /// Returns an error when a payload cannot be stored, its declared content
    /// id does not match, its blob is missing or corrupt, or an image has no
    /// payload reference. Error text never includes payload bytes.
    pub fn externalize_record(&self, record: &SessionRecord) -> anyhow::Result<SessionRecord> {
        let mut externalized = record.clone();
        match &mut externalized {
            SessionRecord::Message(data) => {
                self.externalize_images(&mut data.images, "message")?;
            },
            SessionRecord::FunctionCallOutput(data) => {
                self.externalize_images(&mut data.images, "function call output")?;
                self.externalize_output(data, "function call output text")?;
            },
            SessionRecord::Compaction(checkpoint) => {
                let externalized_checkpoint = self.externalize_checkpoint(checkpoint)?;
                *checkpoint = externalized_checkpoint;
            },
            _ => {},
        }
        Ok(externalized)
    }

    /// Return a copy of `checkpoint` whose retained message and
    /// function-call-output payloads refer to blobs instead of carrying inline
    /// bytes.
    ///
    /// An image with neither inline bytes nor a content id is rejected. The
    /// original checkpoint is unchanged, so a live checkpoint can retain the
    /// bytes it measured before persistence. Retained output text at or above
    /// [`TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES`] is externalized; shorter
    /// text stays inline unless it already carries a reference.
    ///
    /// # Errors
    ///
    /// Returns an error when a retained payload cannot be stored, its declared
    /// content id does not match, its blob is missing or corrupt, or an image
    /// has no payload reference. Error text never includes payload bytes.
    pub fn externalize_checkpoint(
        &self,
        checkpoint: &CompactionData,
    ) -> anyhow::Result<CompactionData> {
        let mut externalized = checkpoint.clone();
        for (retained_index, retained) in externalized.retained.iter_mut().enumerate() {
            match &mut retained.record {
                RetainedConversationRecord::Message(data) => self.externalize_images(
                    &mut data.images,
                    &format!("checkpoint retained {retained_index} message"),
                )?,
                RetainedConversationRecord::FunctionCallOutput(data) => {
                    self.externalize_images(
                        &mut data.images,
                        &format!("checkpoint retained {retained_index} function call output"),
                    )?;
                    self.externalize_output(
                        data,
                        &format!("checkpoint retained {retained_index} function call output text"),
                    )?;
                },
                RetainedConversationRecord::Reasoning(_)
                | RetainedConversationRecord::FunctionCall(_) => {},
            }
        }
        Ok(externalized)
    }

    /// Prepare fork seed records for a child session.
    ///
    /// Every content id referenced by `records` is hard-linked from `source`
    /// and verified in this store. Images that remain inline in legacy seed
    /// records and outputs at or above
    /// [`TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES`] are then externalized
    /// into this store. A missing or corrupt parent blob fails before the
    /// caller creates the child session file.
    ///
    /// # Errors
    ///
    /// Returns an error naming the record and digest when a reference is
    /// invalid, a parent blob is missing or corrupt, or an inline payload
    /// cannot be stored or verified. Error text never includes payload bytes.
    pub fn prepare_fork_seed_records(
        &self,
        source: &Self,
        records: &[SessionRecord],
    ) -> anyhow::Result<Vec<SessionRecord>> {
        let mut referenced = BTreeSet::new();
        for (record_index, record) in records.iter().enumerate() {
            for (id_index, value) in record.content_ids().enumerate() {
                let id = ContentId::parse(value).with_context(|| {
                    format!(
                        "fork seed record {record_index} content id {id_index} has an invalid content id"
                    )
                })?;
                referenced.insert(id);
            }
        }

        for id in &referenced {
            self.link_verified(source, id)
                .with_context(|| format!("failed to link fork blob '{id}'"))?;
        }

        records
            .iter()
            .enumerate()
            .map(|(record_index, record)| {
                self.externalize_record(record).with_context(|| {
                    format!("failed to externalize fork seed record {record_index}")
                })
            })
            .collect()
    }

    /// Restore every referenced payload in `records` from this store.
    ///
    /// Walks message images, function-call-output text and images, and payloads
    /// retained inside checkpoints. A payload that carries both inline bytes
    /// and a content id keeps its inline bytes after verifying that they hash
    /// to the id. An image with neither is rejected before it can reach a
    /// provider.
    ///
    /// # Errors
    ///
    /// Returns an error naming the session and digest when a reference is
    /// invalid, missing, unreadable, non-regular, mismatched, or not valid
    /// UTF-8. Error text never includes payload bytes.
    pub fn hydrate_session(
        &self,
        session_id: &str,
        records: &mut [SessionRecord],
    ) -> anyhow::Result<()> {
        for (record_index, record) in records.iter_mut().enumerate() {
            self.hydrate_record(session_id, record_index, record)?;
        }
        Ok(())
    }

    fn hydrate_record(
        &self,
        session_id: &str,
        record_index: usize,
        record: &mut SessionRecord,
    ) -> anyhow::Result<()> {
        match record {
            SessionRecord::Message(data) => self.hydrate_images(
                session_id,
                &mut data.images,
                &format!("record {record_index} message"),
            ),
            SessionRecord::FunctionCallOutput(data) => {
                self.hydrate_images(
                    session_id,
                    &mut data.images,
                    &format!("record {record_index} function call output"),
                )?;
                self.hydrate_output(
                    session_id,
                    data,
                    &format!("record {record_index} function call output text"),
                )?;
                Ok(())
            },
            SessionRecord::Compaction(checkpoint) => {
                self.hydrate_checkpoint(session_id, record_index, checkpoint)
            },
            _ => Ok(()),
        }
    }

    fn hydrate_checkpoint(
        &self,
        session_id: &str,
        record_index: usize,
        checkpoint: &mut CompactionData,
    ) -> anyhow::Result<()> {
        for (retained_index, retained) in checkpoint.retained.iter_mut().enumerate() {
            match &mut retained.record {
                RetainedConversationRecord::Message(data) => self.hydrate_images(
                    session_id,
                    &mut data.images,
                    &format!("record {record_index} checkpoint retained {retained_index} message"),
                )?,
                RetainedConversationRecord::FunctionCallOutput(data) => {
                    self.hydrate_images(
                        session_id,
                        &mut data.images,
                        &format!(
                            "record {record_index} checkpoint retained {retained_index} function call output"
                        ),
                    )?;
                    self.hydrate_output(
                        session_id,
                        data,
                        &format!(
                            "record {record_index} checkpoint retained {retained_index} function call output text"
                        ),
                    )?;
                },
                RetainedConversationRecord::Reasoning(_)
                | RetainedConversationRecord::FunctionCall(_) => {},
            }
        }
        Ok(())
    }

    fn externalize_images(
        &self,
        images: &mut [ImagePart],
        record_kind: &str,
    ) -> anyhow::Result<()> {
        for (index, image) in images.iter_mut().enumerate() {
            self.externalize_image(image, &format!("{record_kind} image {index}"))?;
        }
        Ok(())
    }

    fn externalize_image(&self, image: &mut ImagePart, context: &str) -> anyhow::Result<()> {
        let declared = image
            .content_id
            .as_deref()
            .map(ContentId::parse)
            .transpose()
            .with_context(|| format!("{context} has an invalid content id"))?;

        if image.data_base64.is_empty() {
            let id = declared.ok_or_else(|| {
                anyhow::anyhow!("{context} has neither inline bytes nor a content id")
            })?;
            self.read_verified(&id)
                .with_context(|| format!("{context} content id '{id}'"))?;
            image.content_id = Some(id.as_str().to_owned());
            return Ok(());
        }

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&image.data_base64)
            .with_context(|| format!("{context} has invalid base64 image data"))?;
        let computed = ContentId::from_bytes(&bytes);
        let id = match declared {
            Some(id) => {
                anyhow::ensure!(
                    id == computed,
                    "{context} content id '{id}' does not match its inline bytes (found '{computed}')"
                );
                self.read_verified(&id)
                    .with_context(|| format!("{context} content id '{id}'"))?;
                id
            },
            None => self
                .put(&bytes)
                .with_context(|| format!("failed to store {context}"))?,
        };
        image.data_base64.clear();
        image.content_id = Some(id.as_str().to_owned());
        Ok(())
    }

    fn externalize_output(
        &self,
        data: &mut FunctionCallOutputData,
        context: &str,
    ) -> anyhow::Result<()> {
        let declared = data
            .content_id
            .as_deref()
            .map(ContentId::parse)
            .transpose()
            .with_context(|| format!("{context} has an invalid content id"))?;

        if let Some(id) = declared {
            let bytes = self
                .read_verified(&id)
                .with_context(|| format!("{context} content id '{id}'"))?;
            if !data.output.is_empty() {
                let computed = ContentId::from_bytes(data.output.as_bytes());
                anyhow::ensure!(
                    id == computed,
                    "{context} content id '{id}' does not match its inline bytes (found '{computed}')"
                );
            }
            std::str::from_utf8(&bytes)
                .with_context(|| format!("{context} content id '{id}' is not valid UTF-8"))?;
            data.output.clear();
            data.content_id = Some(id.as_str().to_owned());
            return Ok(());
        }

        if data.output.len() < TOOL_OUTPUT_EXTERNALIZATION_THRESHOLD_BYTES {
            return Ok(());
        }
        let id = self
            .put(data.output.as_bytes())
            .with_context(|| format!("failed to store {context}"))?;
        data.output.clear();
        data.content_id = Some(id.as_str().to_owned());
        Ok(())
    }

    fn hydrate_images(
        &self,
        session_id: &str,
        images: &mut [ImagePart],
        context: &str,
    ) -> anyhow::Result<()> {
        for (index, image) in images.iter_mut().enumerate() {
            let image_context = format!("{context} image {index}");
            let declared = image
                .content_id
                .as_deref()
                .map(ContentId::parse)
                .transpose()
                .with_context(|| {
                    format!("session {session_id}: {image_context} has an invalid content id")
                })?;

            if !image.data_base64.is_empty() {
                if let Some(id) = declared {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(&image.data_base64)
                        .with_context(|| {
                            format!(
                                "session {session_id}: {image_context} has invalid base64 image data"
                            )
                        })?;
                    anyhow::ensure!(
                        id.matches(&bytes),
                        "session {session_id}: {image_context} inline bytes do not match content id '{id}'"
                    );
                }
                continue;
            }

            let id = declared.ok_or_else(|| {
                anyhow::anyhow!(
                    "session {session_id}: {image_context} has neither inline bytes nor a content id"
                )
            })?;
            let bytes = self.read_verified(&id).with_context(|| {
                format!("session {session_id}: failed to hydrate {image_context} content id '{id}'")
            })?;
            image.data_base64 = base64::engine::general_purpose::STANDARD.encode(bytes);
            image.content_id = Some(id.as_str().to_owned());
        }
        Ok(())
    }

    fn hydrate_output(
        &self,
        session_id: &str,
        data: &mut FunctionCallOutputData,
        context: &str,
    ) -> anyhow::Result<()> {
        let declared = data
            .content_id
            .as_deref()
            .map(ContentId::parse)
            .transpose()
            .with_context(|| {
                format!("session {session_id}: {context} has an invalid content id")
            })?;
        let Some(id) = declared else {
            return Ok(());
        };

        if !data.output.is_empty() {
            anyhow::ensure!(
                id.matches(data.output.as_bytes()),
                "session {session_id}: {context} inline bytes do not match content id '{id}'"
            );
            return Ok(());
        }

        let bytes = self.read_verified(&id).with_context(|| {
            format!("session {session_id}: failed to hydrate {context} content id '{id}'")
        })?;
        data.output = String::from_utf8(bytes).with_context(|| {
            format!("session {session_id}: {context} content id '{id}' is not valid UTF-8")
        })?;
        data.content_id = Some(id.as_str().to_owned());
        Ok(())
    }

    /// Hard-link `id` from `source` into this store and verify the result.
    ///
    /// This is the fork primitive. Both roots are validated, an existing
    /// destination is verified rather than replaced, and a newly linked entry
    /// is followed by a directory sync.
    ///
    /// # Errors
    ///
    /// Returns an error when either root is unsafe, the source is missing or
    /// corrupt, the link cannot be created, or the destination does not verify.
    pub fn link_verified(&self, source: &Self, id: &ContentId) -> anyhow::Result<()> {
        let _ = source.read_verified(id)?;
        let destination_root = self.ensure_root_for_write()?;
        let destination = destination_root.join(id.as_str());
        if !self.destination_needs_link(&destination, id)? {
            return Ok(());
        }

        let source_path = source.checked_root().join(id.as_str());
        self.link_temp_blob(&source_path, &destination, &destination_root, id)
    }

    fn destination_needs_link(&self, destination: &Path, id: &ContentId) -> anyhow::Result<bool> {
        match fs::symlink_metadata(destination) {
            Ok(_) => {
                self.read_verified(id)?;
                Ok(false)
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(source_error) => Err(source_error).with_context(|| {
                format!(
                    "failed to inspect destination blob '{}' at '{}'",
                    id,
                    destination.display()
                )
            }),
        }
    }

    fn link_temp_blob(
        &self,
        source_path: &Path,
        destination: &Path,
        destination_root: &Path,
        id: &ContentId,
    ) -> anyhow::Result<()> {
        match fs::hard_link(source_path, destination) {
            Ok(()) => {
                sync_dir(destination_root).with_context(|| {
                    format!(
                        "failed to sync blob directory '{}' after linking '{}'",
                        destination_root.display(),
                        id
                    )
                })?;
                self.read_verified(id)
                    .with_context(|| format!("failed to verify linked blob '{id}'"))?;
                Ok(())
            },
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                self.read_verified(id).with_context(|| {
                    format!("failed to verify existing destination blob '{id}'")
                })?;
                Ok(())
            },
            Err(source_error) => Err(source_error).with_context(|| {
                format!(
                    "failed to link blob '{}' from '{}' to '{}'",
                    id,
                    source_path.display(),
                    destination.display()
                )
            }),
        }
    }

    fn checked_root(&self) -> PathBuf {
        self.root.components().collect()
    }

    fn validate_existing_root(&self) -> anyhow::Result<PathBuf> {
        let root = self.checked_root();
        if root.as_os_str().is_empty() {
            bail!("blob store root is empty");
        }
        match fs::symlink_metadata(&root) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!(
                    "blob store root '{}' is a symlink; Cake refuses to follow it",
                    root.display()
                );
            },
            Ok(metadata) if !metadata.is_dir() => {
                bail!(
                    "blob store root '{}' is not a directory; Cake requires a real directory",
                    root.display()
                );
            },
            Ok(_) => Ok(root),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                bail!("blob store directory '{}' does not exist", root.display());
            },
            Err(source) => Err(source)
                .with_context(|| format!("failed to inspect blob store root '{}'", root.display())),
        }
    }

    fn ensure_root_for_write(&self) -> anyhow::Result<PathBuf> {
        let root = self.checked_root();
        match inspect_root_for_write(&root)? {
            RootState::Existing => {
                owner_only::tighten_dir_owner_only(&root).with_context(|| {
                    format!(
                        "failed to set owner-only mode {:04o} on blob store root '{}'",
                        owner_only::OWNER_ONLY_DIR_MODE,
                        root.display()
                    )
                })?;
            },
            RootState::Missing => create_root_for_write(&root)?,
        }
        Ok(root)
    }
}

/// Inspect one recovery candidate and remove it when it is unreferenced.
fn recover_entry(
    entry: std::io::Result<fs::DirEntry>,
    root: &Path,
    session_id: &str,
    referenced: &BTreeSet<ContentId>,
    report: &mut RecoveryReport,
) {
    let entry = match entry {
        Ok(entry) => entry,
        Err(error) => {
            report.errors.push(format!(
                "session {session_id}: failed to read an entry in '{}': {error}",
                root.display()
            ));
            return;
        },
    };
    let file_type = match entry.file_type() {
        Ok(file_type) => file_type,
        Err(error) => {
            report.errors.push(format!(
                "session {session_id}: failed to inspect '{}': {error}",
                entry.path().display()
            ));
            return;
        },
    };
    if !file_type.is_file() {
        return;
    }
    let path = entry.path();
    let name = entry.file_name();
    let Some(name) = name.to_str() else {
        return;
    };
    if name.starts_with(TEMP_PREFIX) {
        remove_temporary_blob(&path, session_id, report);
        return;
    }
    let Ok(id) = ContentId::parse(name) else {
        return;
    };
    if !referenced.contains(&id) {
        remove_unreferenced_blob(&path, &id, session_id, report);
    }
}

fn remove_temporary_blob(path: &Path, session_id: &str, report: &mut RecoveryReport) {
    match fs::remove_file(path) {
        Ok(()) => report.removed_temporary_files.push(path.to_path_buf()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => report.errors.push(format!(
            "session {session_id}: failed to remove temporary blob '{}': {error}",
            path.display()
        )),
    }
}

fn remove_unreferenced_blob(
    path: &Path,
    id: &ContentId,
    session_id: &str,
    report: &mut RecoveryReport,
) {
    match fs::remove_file(path) {
        Ok(()) => report.removed_blobs.push(id.clone()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => report.errors.push(format!(
            "session {session_id}: failed to remove unreferenced blob '{id}' at '{}': {error}",
            path.display()
        )),
    }
}

/// Collect every valid content id referenced by `records`.
///
/// An invalid id rejects the whole set so recovery cannot delete a blob that
/// an unreadable reference still needs.
///
/// # Errors
///
/// Returns an error when any record carries a malformed content id.
pub fn referenced_content_ids(records: &[SessionRecord]) -> anyhow::Result<BTreeSet<ContentId>> {
    let mut referenced = BTreeSet::new();
    for (record_index, record) in records.iter().enumerate() {
        for (id_index, value) in record.content_ids().enumerate() {
            let id = ContentId::parse(value).with_context(|| {
                format!("session record {record_index} content id {id_index} is invalid")
            })?;
            referenced.insert(id);
        }
    }
    Ok(referenced)
}

/// Build the read-only report for every session file under `sessions_dir`.
///
/// # Errors
///
/// Returns an error when the sessions directory itself cannot be read. A
/// malformed or unreadable session becomes a report entry instead.
pub fn blob_diagnostics(sessions_dir: &Path) -> anyhow::Result<BlobDiagnostics> {
    let mut session_files = BTreeMap::new();
    let mut blob_dirs = BTreeMap::new();
    for entry in fs::read_dir(sessions_dir).with_context(|| {
        format!(
            "failed to read sessions directory '{}' for blob diagnostics",
            sessions_dir.display()
        )
    })? {
        let entry = entry.with_context(|| {
            format!(
                "failed to read an entry in sessions directory '{}'",
                sessions_dir.display()
            )
        })?;
        let path = entry.path();
        let Some(extension) = path.extension().and_then(std::ffi::OsStr::to_str) else {
            continue;
        };
        let Some(stem) = path.file_stem().map(OsString::from) else {
            continue;
        };
        match extension {
            "jsonl" => {
                session_files.insert(stem, path);
            },
            "blobs" => {
                blob_dirs.insert(stem, path);
            },
            _ => {},
        }
    }

    let mut totals = TotalsAccumulator::default();
    let mut sessions = Vec::with_capacity(session_files.len());
    for (stem, session_file) in session_files {
        let blob_dir = session_file.with_extension("blobs");
        blob_dirs.remove(&stem);
        let scan = scan_session_references(&session_file);
        sessions.push(diagnose_session_blobs(
            &stem.to_string_lossy(),
            &session_file,
            &blob_dir,
            scan,
            &mut totals,
        ));
    }

    let mut orphaned_stores = Vec::with_capacity(blob_dirs.len());
    for (_, blob_dir) in blob_dirs {
        let (report, files) = diagnose_orphaned_store(&blob_dir);
        for file in &files {
            totals.observe_orphaned_file(file);
        }
        totals.orphaned_store_count += 1;
        totals.orphaned_store_blob_count += report.blob_count;
        orphaned_stores.push(report);
    }

    Ok(BlobDiagnostics {
        sessions,
        orphaned_stores,
        totals: totals.finish(),
    })
}

/// What one scan of a session file found, even when parsing stopped early.
struct SessionReferenceScan {
    referenced: BTreeSet<ContentId>,
    error: Option<String>,
    complete: bool,
}

impl SessionReferenceScan {
    const fn new() -> Self {
        Self {
            referenced: BTreeSet::new(),
            error: None,
            complete: true,
        }
    }

    fn fail(&mut self, message: String) {
        if self.error.is_none() {
            self.error = Some(message);
        }
        self.complete = false;
    }
}

/// One regular digest-named file in a blob directory.
struct BlobFile {
    id: ContentId,
    size: u64,
    identity: Option<(u64, u64)>,
    referenced: bool,
}

/// A read-only inventory of one blob directory.
struct BlobDirectoryInventory {
    files: Vec<BlobFile>,
    temporary_files: Vec<PathBuf>,
}

fn scan_session_references(path: &Path) -> SessionReferenceScan {
    let mut scan = SessionReferenceScan::new();
    if !session_source_is_regular(path, &mut scan) {
        return scan;
    }
    let Some(file) = open_session_source(path, &mut scan) else {
        return scan;
    };
    let mut framer = SessionFramer::new(BufReader::new(file));
    let saw_record = scan_session_records(&mut framer, path, &mut scan);
    if !saw_record {
        scan.fail(format!("session file '{}' is empty", path.display()));
    }
    scan
}

fn session_source_is_regular(path: &Path, scan: &mut SessionReferenceScan) -> bool {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            scan.fail(format!(
                "failed to inspect session file '{}': {error}",
                path.display()
            ));
            return false;
        },
    };
    if metadata.file_type().is_symlink() {
        scan.fail(format!(
            "session file '{}' is a symlink; refusing to follow it",
            path.display()
        ));
        return false;
    }
    if !metadata.is_file() {
        scan.fail(format!(
            "session file '{}' is not a regular file",
            path.display()
        ));
        return false;
    }
    true
}

fn open_session_source(path: &Path, scan: &mut SessionReferenceScan) -> Option<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    apply_no_follow(&mut options);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) => {
            scan.fail(format!(
                "failed to open session file '{}': {error}",
                path.display()
            ));
            return None;
        },
    };
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        scan.fail(format!(
            "session file '{}' is not a regular file",
            path.display()
        ));
        return None;
    }
    Some(file)
}

fn scan_session_records(
    framer: &mut SessionFramer<BufReader<File>>,
    path: &Path,
    scan: &mut SessionReferenceScan,
) -> bool {
    let mut saw_record = false;
    loop {
        let line = match framer.next_record() {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) => {
                scan.fail(format!(
                    "failed to read session file '{}': {error}",
                    path.display()
                ));
                break;
            },
        };
        saw_record = true;
        if !scan_session_record(&line, path, scan) {
            break;
        }
    }
    saw_record
}

fn scan_session_record(
    line: &FramedSessionLine,
    path: &Path,
    scan: &mut SessionReferenceScan,
) -> bool {
    let Some(text) = line.text.as_deref() else {
        scan.fail(format!(
            "session file '{}' line {} has an ignored partial record: the record is not valid UTF-8",
            path.display(),
            line.line_number
        ));
        return false;
    };
    match serde_json::from_str::<SessionRecord>(text) {
        Ok(record) => {
            collect_record_content_ids(&record, path, line.line_number, scan);
            true
        },
        Err(error) if line.partial_tail => {
            scan.fail(format!(
                "session file '{}' line {} has an ignored partial record: {error}",
                path.display(),
                line.line_number
            ));
            false
        },
        Err(error) => {
            scan.fail(format!(
                "session file '{}' line {} is malformed: {error}",
                path.display(),
                line.line_number
            ));
            false
        },
    }
}

fn collect_record_content_ids(
    record: &SessionRecord,
    path: &Path,
    line_number: usize,
    scan: &mut SessionReferenceScan,
) {
    for value in record.content_ids() {
        match ContentId::parse(value) {
            Ok(id) => {
                scan.referenced.insert(id);
            },
            Err(error) => scan.fail(format!(
                "session file '{}' line {line_number} has an invalid content id: {error}",
                path.display()
            )),
        }
    }
}

fn inspect_blob_directory(blob_dir: &Path) -> anyhow::Result<Option<BlobDirectoryInventory>> {
    match inspect_root_for_write(blob_dir)? {
        RootState::Missing => return Ok(None),
        RootState::Existing => {},
    }

    let mut inventory = BlobDirectoryInventory {
        files: Vec::new(),
        temporary_files: Vec::new(),
    };
    for entry in fs::read_dir(blob_dir).with_context(|| {
        format!(
            "failed to read blob directory '{}' for diagnostics",
            blob_dir.display()
        )
    })? {
        collect_blob_directory_entry(entry, blob_dir, &mut inventory)?;
    }
    inventory
        .files
        .sort_by(|left, right| left.id.cmp(&right.id));
    inventory.temporary_files.sort();
    Ok(Some(inventory))
}

fn collect_blob_directory_entry(
    entry: std::io::Result<fs::DirEntry>,
    blob_dir: &Path,
    inventory: &mut BlobDirectoryInventory,
) -> anyhow::Result<()> {
    let entry = entry.with_context(|| {
        format!(
            "failed to read an entry in blob directory '{}'",
            blob_dir.display()
        )
    })?;
    let file_type = entry.file_type().with_context(|| {
        format!(
            "failed to inspect an entry in blob directory '{}'",
            blob_dir.display()
        )
    })?;
    if !file_type.is_file() {
        return Ok(());
    }
    let path = entry.path();
    let name = entry.file_name();
    let Some(name) = name.to_str() else {
        return Ok(());
    };
    if name.starts_with(TEMP_PREFIX) {
        inventory.temporary_files.push(path);
        return Ok(());
    }
    let Ok(id) = ContentId::parse(name) else {
        return Ok(());
    };
    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("failed to inspect blob '{}' at '{}'", id, path.display()))?;
    inventory.files.push(BlobFile {
        id,
        size: metadata.len(),
        identity: file_identity(&metadata),
        referenced: false,
    });
    Ok(())
}

fn diagnose_session_blobs(
    session_id: &str,
    session_file: &Path,
    blob_dir: &Path,
    scan: SessionReferenceScan,
    totals: &mut TotalsAccumulator,
) -> SessionBlobDiagnostics {
    let mut report = SessionBlobDiagnostics {
        session_id: session_id.to_string(),
        session_file: session_file.to_path_buf(),
        blob_dir: blob_dir.to_path_buf(),
        blob_count: 0,
        stored_bytes: 0,
        referenced_count: scan.referenced.len(),
        referenced_bytes: 0,
        unreferenced_count: 0,
        unreferenced_bytes: 0,
        temporary_file_count: 0,
        temporary_files: Vec::new(),
        missing_references: Vec::new(),
        corrupt_references: Vec::new(),
        session_error: scan.error,
        directory_error: None,
        references_complete: scan.complete,
    };

    let mut files = Vec::new();
    match inspect_blob_directory(blob_dir) {
        Ok(Some(inventory)) => {
            files = apply_blob_inventory(&mut report, inventory, &scan.referenced);
        },
        Ok(None) => {},
        Err(error) => report.directory_error = Some(format!("{error:#}")),
    }

    if report.directory_error.is_none() {
        let (missing, corrupt) = reference_problems(blob_dir, &scan.referenced, &files);
        report.missing_references = missing;
        report.corrupt_references = corrupt;
    }
    sort_reference_problems(&mut report);
    accumulate_session_totals(totals, &report, &files);
    report
}

fn apply_blob_inventory(
    report: &mut SessionBlobDiagnostics,
    mut inventory: BlobDirectoryInventory,
    referenced: &BTreeSet<ContentId>,
) -> Vec<BlobFile> {
    report.blob_count = inventory.files.len();
    report.temporary_file_count = inventory.temporary_files.len();
    report.temporary_files = inventory.temporary_files;
    for file in &mut inventory.files {
        report.stored_bytes += file.size;
        file.referenced = referenced.contains(&file.id);
        if file.referenced {
            report.referenced_bytes += file.size;
        } else {
            report.unreferenced_count += 1;
            report.unreferenced_bytes += file.size;
        }
    }
    inventory.files
}

fn reference_problems(
    blob_dir: &Path,
    referenced: &BTreeSet<ContentId>,
    files: &[BlobFile],
) -> (Vec<MissingBlobReference>, Vec<CorruptBlobReference>) {
    let seen = files
        .iter()
        .filter(|file| file.referenced)
        .map(|file| &file.id)
        .collect::<BTreeSet<_>>();
    let missing = referenced
        .iter()
        .filter(|id| !seen.contains(id))
        .map(|id| MissingBlobReference {
            digest: id.to_string(),
            path: blob_dir.join(id.as_str()),
        })
        .collect();
    let store = BlobStore::new(blob_dir.to_path_buf());
    let corrupt = seen
        .into_iter()
        .filter_map(|id| {
            store
                .read_verified(id)
                .err()
                .map(|error| CorruptBlobReference {
                    digest: id.to_string(),
                    path: blob_dir.join(id.as_str()),
                    reason: format!("{error:#}"),
                })
        })
        .collect();
    (missing, corrupt)
}

fn sort_reference_problems(report: &mut SessionBlobDiagnostics) {
    report.missing_references.sort_by(|left, right| {
        left.digest
            .cmp(&right.digest)
            .then_with(|| left.path.cmp(&right.path))
    });
    report.corrupt_references.sort_by(|left, right| {
        left.digest
            .cmp(&right.digest)
            .then_with(|| left.path.cmp(&right.path))
    });
}

fn accumulate_session_totals(
    totals: &mut TotalsAccumulator,
    report: &SessionBlobDiagnostics,
    files: &[BlobFile],
) {
    totals.session_count += 1;
    totals.blob_count += report.blob_count;
    totals.referenced_count += report.referenced_count;
    totals.unreferenced_count += report.unreferenced_count;
    totals.unreferenced_bytes += report.unreferenced_bytes;
    totals.temporary_file_count += report.temporary_file_count;
    totals.missing_reference_count += report.missing_references.len();
    totals.corrupt_reference_count += report.corrupt_references.len();
    for file in files {
        totals.observe_stored_file(file);
        if file.referenced {
            totals.observe_referenced_file(file);
        }
    }
}

fn diagnose_orphaned_store(blob_dir: &Path) -> (OrphanedBlobStoreDiagnostics, Vec<BlobFile>) {
    let mut report = OrphanedBlobStoreDiagnostics {
        blob_dir: blob_dir.to_path_buf(),
        blob_count: 0,
        stored_bytes: 0,
        temporary_file_count: 0,
        temporary_files: Vec::new(),
        directory_error: None,
    };
    match inspect_blob_directory(blob_dir) {
        Ok(Some(inventory)) => {
            report.blob_count = inventory.files.len();
            report.stored_bytes = inventory.files.iter().map(|file| file.size).sum();
            report.temporary_file_count = inventory.temporary_files.len();
            report.temporary_files = inventory.temporary_files;
            (report, inventory.files)
        },
        Ok(None) => (report, Vec::new()),
        Err(error) => {
            report.directory_error = Some(format!("{error:#}"));
            (report, Vec::new())
        },
    }
}

#[derive(Debug, Default)]
struct TotalsAccumulator {
    session_count: usize,
    blob_count: usize,
    unique_blob_count: usize,
    stored_bytes: u64,
    referenced_count: usize,
    referenced_bytes: u64,
    unreferenced_count: usize,
    unreferenced_bytes: u64,
    temporary_file_count: usize,
    missing_reference_count: usize,
    corrupt_reference_count: usize,
    orphaned_store_count: usize,
    orphaned_store_blob_count: usize,
    orphaned_store_bytes: u64,
    stored_identities: BTreeSet<(u64, u64)>,
    referenced_identities: BTreeSet<(u64, u64)>,
    orphaned_identities: BTreeSet<(u64, u64)>,
}

impl TotalsAccumulator {
    fn observe_stored_file(&mut self, file: &BlobFile) {
        if let Some(identity) = file.identity
            && !self.stored_identities.insert(identity)
        {
            return;
        }
        self.unique_blob_count += 1;
        self.stored_bytes += file.size;
    }

    fn observe_referenced_file(&mut self, file: &BlobFile) {
        if let Some(identity) = file.identity
            && !self.referenced_identities.insert(identity)
        {
            return;
        }
        self.referenced_bytes += file.size;
    }

    fn observe_orphaned_file(&mut self, file: &BlobFile) {
        if let Some(identity) = file.identity
            && !self.orphaned_identities.insert(identity)
        {
            return;
        }
        self.orphaned_store_bytes += file.size;
    }

    fn finish(self) -> BlobDiagnosticsTotals {
        BlobDiagnosticsTotals {
            session_count: self.session_count,
            blob_count: self.blob_count,
            unique_blob_count: self.unique_blob_count,
            stored_bytes: self.stored_bytes,
            referenced_count: self.referenced_count,
            referenced_bytes: self.referenced_bytes,
            unreferenced_count: self.unreferenced_count,
            unreferenced_bytes: self.unreferenced_bytes,
            temporary_file_count: self.temporary_file_count,
            missing_reference_count: self.missing_reference_count,
            corrupt_reference_count: self.corrupt_reference_count,
            orphaned_store_count: self.orphaned_store_count,
            orphaned_store_blob_count: self.orphaned_store_blob_count,
            orphaned_store_bytes: self.orphaned_store_bytes,
        }
    }
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "non-Unix builds have no device and inode identity to deduplicate"
)]
fn file_identity(metadata: &fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Some((metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

fn inspect_root_for_write(root: &Path) -> anyhow::Result<RootState> {
    if root.as_os_str().is_empty() {
        bail!("blob store root is empty");
    }
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!(
                "blob store root '{}' is a symlink; Cake refuses to follow it",
                root.display()
            );
        },
        Ok(metadata) if !metadata.is_dir() => {
            bail!(
                "blob store root '{}' is not a directory; Cake requires a real directory",
                root.display()
            );
        },
        Ok(_) => Ok(RootState::Existing),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(RootState::Missing),
        Err(source) => Err(source)
            .with_context(|| format!("failed to inspect blob store root '{}'", root.display())),
    }
}

fn create_root_for_write(root: &Path) -> anyhow::Result<()> {
    owner_only::create_dir_all_owner_only(root).with_context(|| {
        format!(
            "failed to create blob store root '{}' with owner-only mode {:04o}",
            root.display(),
            owner_only::OWNER_ONLY_DIR_MODE
        )
    })?;
    let metadata = fs::symlink_metadata(root)
        .with_context(|| format!("failed to inspect new blob store root '{}'", root.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "blob store root '{}' was not created as a real directory",
            root.display()
        );
    }
    if let Some(parent) = root.parent() {
        sync_dir(parent).with_context(|| {
            format!(
                "failed to sync parent directory '{}' after creating blob store root '{}'",
                parent.display(),
                root.display()
            )
        })?;
    }
    Ok(())
}

fn validate_blob_target(path: &Path, id: &ContentId) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!(
                "blob '{}' at '{}' is a symlink; Cake refuses to follow it",
                id,
                path.display()
            );
        },
        Ok(metadata) if !metadata.is_file() => {
            bail!(
                "blob '{}' at '{}' is not a regular file; Cake refuses to use it",
                id,
                path.display()
            );
        },
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!("blob '{}' is missing at '{}'", id, path.display());
        },
        Err(source) => Err(source)
            .with_context(|| format!("failed to inspect blob '{}' at '{}'", id, path.display())),
    }
}

fn open_blob_for_read(path: &Path, id: &ContentId) -> anyhow::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    apply_no_follow(&mut options);
    let file = options.open(path).with_context(|| {
        format!(
            "failed to open blob '{}' at '{}' without following symlinks",
            id,
            path.display()
        )
    })?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect blob '{id}'"))?;
    if !metadata.is_file() {
        bail!(
            "blob '{}' at '{}' is not a regular file; Cake refuses to use it",
            id,
            path.display()
        );
    }
    Ok(file)
}

fn write_temp_blob(root: &Path, bytes: &[u8]) -> anyhow::Result<PathBuf> {
    let temp_path = root.join(temp_file_name());
    let mut temp = open_temp_file(&temp_path)?;
    let write_result = (|| -> anyhow::Result<()> {
        owner_only::set_file_owner_only(&temp).with_context(|| {
            format!(
                "failed to set owner-only mode {:04o} on temporary blob '{}'",
                owner_only::OWNER_ONLY_FILE_MODE,
                temp_path.display()
            )
        })?;
        temp.write_all(bytes)
            .with_context(|| format!("failed to write temporary blob '{}'", temp_path.display()))?;
        temp.sync_all().with_context(|| {
            format!(
                "failed to sync temporary blob '{}' to disk",
                temp_path.display()
            )
        })?;
        Ok(())
    })();
    drop(temp);

    match write_result {
        Ok(()) => Ok(temp_path),
        Err(error) => {
            drop(fs::remove_file(&temp_path));
            Err(error)
        },
    }
}

fn open_temp_file(path: &Path) -> anyhow::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    apply_temp_file_options(&mut options);
    options.open(path).with_context(|| {
        format!(
            "failed to create temporary blob '{}' with owner-only mode {:04o}",
            path.display(),
            owner_only::OWNER_ONLY_FILE_MODE
        )
    })
}

fn temp_file_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{TEMP_PREFIX}{}-{nanos}-{counter}", std::process::id())
}

#[cfg(unix)]
fn apply_temp_file_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt as _;

    options
        .mode(owner_only::OWNER_ONLY_FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW);
}

#[cfg(not(unix))]
fn apply_temp_file_options(options: &mut OpenOptions) {
    let _ = options;
}

#[cfg(unix)]
fn apply_no_follow(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt as _;

    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
}

#[cfg(not(unix))]
fn apply_no_follow(options: &mut OpenOptions) {
    let _ = options;
}

#[cfg(unix)]
fn sync_dir(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(path: &Path) -> std::io::Result<()> {
    let _ = path;
    Ok(())
}
