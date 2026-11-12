//! Content-addressed image storage for one persisted session.
//!
//! Each session owns a sibling `<uuid>.blobs` directory. A blob's name is the
//! lowercase SHA-256 digest of its decoded bytes. Publication is write-once:
//! callers upload a temporary file, hard-link it into place without replacing
//! an existing digest, and verify the winner before a session record refers to
//! it.
//!
//! The store deliberately has no cross-session lock or sweep. The session's
//! append lock serializes writers, and recovery is scoped to one session. See
//! ADR 061 and `docs/exec-plans/active/content-address-session-images.md`.

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
use crate::types::session::SessionRecord;

/// Prefix reserved for temporary files inside a session blob directory.
///
/// A digest is always 64 lowercase hexadecimal characters, so recovery can
/// remove `.tmp-*` files without risking a published blob or an unknown file.
const TEMP_PREFIX: &str = ".tmp-";

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

/// The content-addressed image store for one session.
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

    /// Return the configured store root.
    #[must_use]
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "diagnostics use the root in Milestone 4")
    )]
    pub fn root(&self) -> &Path {
        &self.root
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

    /// Return a copy of `record` whose message, function-call-output, and
    /// checkpoint-retained images refer to blobs instead of carrying inline
    /// bytes.
    ///
    /// An image with neither inline bytes nor a content id is rejected. An
    /// image with both must name the digest of its inline bytes, and the blob
    /// must already verify. The original record is unchanged, so a live
    /// conversation can keep the bytes for the next provider request.
    ///
    /// # Errors
    ///
    /// Returns an error when an image cannot be decoded, its declared content
    /// id does not match, its blob is missing or corrupt, or it has no payload
    /// reference. Error text never includes image bytes.
    pub fn externalize_record(&self, record: &SessionRecord) -> anyhow::Result<SessionRecord> {
        let mut externalized = record.clone();
        match &mut externalized {
            SessionRecord::Message(data) => {
                self.externalize_images(&mut data.images, "message")?;
            },
            SessionRecord::FunctionCallOutput(data) => {
                self.externalize_images(&mut data.images, "function call output")?;
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
    /// function-call-output images refer to blobs instead of carrying inline
    /// bytes.
    ///
    /// An image with neither inline bytes nor a content id is rejected. The
    /// original checkpoint is unchanged, so a live checkpoint can retain the
    /// bytes it measured before persistence. Retained output text stays inline.
    ///
    /// # Errors
    ///
    /// Returns an error when a retained image cannot be decoded, its declared
    /// content id does not match, its blob is missing or corrupt, or it has no
    /// payload reference. Error text never includes image bytes.
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
                RetainedConversationRecord::FunctionCallOutput(data) => self.externalize_images(
                    &mut data.images,
                    &format!("checkpoint retained {retained_index} function call output"),
                )?,
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
    /// records are then externalized into this store. A missing or corrupt
    /// parent blob fails before the caller creates the child session file.
    ///
    /// # Errors
    ///
    /// Returns an error naming the record and digest when a reference is
    /// invalid, a parent blob is missing or corrupt, or an inline image cannot
    /// be stored or verified. Error text never includes image bytes.
    pub fn prepare_fork_seed_records(
        &self,
        source: &Self,
        records: &[SessionRecord],
    ) -> anyhow::Result<Vec<SessionRecord>> {
        let mut referenced = BTreeSet::new();
        for (record_index, record) in records.iter().enumerate() {
            for (image_index, image) in record.image_parts().iter().enumerate() {
                let Some(value) = image.content_id.as_deref() else {
                    continue;
                };
                let id = ContentId::parse(value).with_context(|| {
                    format!(
                        "fork seed record {record_index} image {image_index} has an invalid content id"
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

    /// Restore every referenced image in `records` from this store.
    ///
    /// Walks message images, function-call-output images, and images retained
    /// inside checkpoints. An image that carries both inline bytes and a
    /// content id keeps its inline bytes after verifying that they hash to the
    /// id. An image with neither is rejected before it can reach a provider.
    ///
    /// # Errors
    ///
    /// Returns an error naming the session and digest when a reference is
    /// invalid, missing, unreadable, non-regular, or mismatched. Error text
    /// never includes image bytes.
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
            SessionRecord::FunctionCallOutput(data) => self.hydrate_images(
                session_id,
                &mut data.images,
                &format!("record {record_index} function call output"),
            ),
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
                    &format!(
                        "record {record_index} checkpoint retained {retained_index} message"
                    ),
                )?,
                RetainedConversationRecord::FunctionCallOutput(data) => self.hydrate_images(
                    session_id,
                    &mut data.images,
                    &format!(
                        "record {record_index} checkpoint retained {retained_index} function call output"
                    ),
                )?,
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
        // A trailing separator would make `symlink_metadata` follow a final
        // symlink on Unix, so all operations use the normalized form.
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
    match serde_json::from_str::<SessionRecord>(&line.text) {
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

#[cfg(test)]
fn is_digest_name(name: &str) -> bool {
    ContentId::parse(name).is_ok()
}

#[cfg(test)]
fn is_temp_name(name: &str) -> bool {
    name.starts_with(TEMP_PREFIX)
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

    // `O_NONBLOCK` closes the race where a regular target is replaced by a
    // FIFO after the metadata check; the post-open shape check still rejects it.
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

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        fs,
        sync::{Arc, Barrier},
        thread,
    };

    use tempfile::TempDir;

    use super::*;
    use crate::types::Role;
    use crate::types::compaction::{RetainedRecord, test_support};
    use crate::types::session::{FunctionCallOutputData, MessageData};

    fn new_store() -> (BlobStore, TempDir) {
        let temporary = TempDir::new().expect("temporary directory");
        let store = BlobStore::new(temporary.path().join("session.blobs"));
        (store, temporary)
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt as _;

        fs::metadata(path).expect("metadata").permissions().mode() & 0o777
    }

    #[test]
    fn content_id_uses_lowercase_sha256_and_parses_strictly() {
        let id = ContentId::from_bytes(b"hello");
        assert_eq!(
            id.as_str(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(ContentId::parse(id.as_str()).expect("parse digest"), id);
        assert!(id.matches(b"hello"));
        assert!(!id.matches(b"world"));

        for invalid in [
            "",
            "abc",
            "2CF24DBA5FB0A30E26E83B2AC5B9E29E1B161E5C1FA7425E73043362938B9824",
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b982",
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b98245",
            "zcf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        ] {
            assert!(
                ContentId::parse(invalid).is_err(),
                "expected {invalid:?} to be rejected"
            );
        }
    }

    #[test]
    fn put_is_write_once_and_uses_the_digest_path() {
        let (store, _temporary) = new_store();
        let bytes = b"image bytes";
        let id = store.put(bytes).expect("put");
        let path = store.root().join(id.as_str());

        assert_eq!(fs::read(&path).expect("read blob"), bytes);
        assert_eq!(store.put(bytes).expect("second put"), id);
        assert_eq!(fs::read_dir(store.root()).expect("read root").count(), 1);
        assert!(is_digest_name(id.as_str()));
        assert!(!is_temp_name(id.as_str()));
        assert_eq!(
            fs::read_dir(store.root())
                .expect("read root")
                .filter_map(Result::ok)
                .filter(|entry| is_temp_name(&entry.file_name().to_string_lossy()))
                .count(),
            0
        );
    }

    #[test]
    fn concurrent_writers_publish_one_verified_blob() {
        let (store, _temporary) = new_store();
        let store = Arc::new(store);
        let barrier = Arc::new(Barrier::new(5));
        let bytes = b"same image".to_vec();

        let mut handles = Vec::with_capacity(4);
        for _ in 0..4 {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            let bytes = bytes.clone();
            handles.push(thread::spawn(move || {
                barrier.wait();
                store.put(&bytes)
            }));
        }
        barrier.wait();

        let ids = handles
            .into_iter()
            .map(|handle| handle.join().expect("thread").expect("put"))
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), 1);
        let id = ids.into_iter().next().expect("one digest");
        assert_eq!(store.read_verified(&id).expect("verified"), bytes);
        assert_eq!(fs::read_dir(store.root()).expect("read root").count(), 1);
    }

    #[test]
    fn existing_mismatched_blob_is_never_overwritten() {
        let (store, _temporary) = new_store();
        let bytes = b"expected image";
        let id = store.put(bytes).expect("put");
        let path = store.root().join(id.as_str());
        fs::write(&path, b"corrupt").expect("corrupt blob");

        let error = store.put(bytes).expect_err("mismatch must fail");
        assert!(format!("{error:#}").contains(id.as_str()), "{error:#}");
        assert_eq!(fs::read(&path).expect("read corrupt blob"), b"corrupt");
        assert_eq!(
            fs::read_dir(store.root())
                .expect("read root")
                .filter_map(Result::ok)
                .filter(|entry| is_temp_name(&entry.file_name().to_string_lossy()))
                .count(),
            0
        );
    }

    #[test]
    fn missing_root_and_missing_blob_fail_with_the_digest() {
        let (store, _temporary) = new_store();
        let id = ContentId::from_bytes(b"missing");
        let error = store.read_verified(&id).expect_err("missing root");
        assert!(format!("{error:#}").contains("does not exist"), "{error:#}");

        fs::create_dir_all(store.root()).expect("create root");
        let error = store.read_verified(&id).expect_err("missing blob");
        assert!(format!("{error:#}").contains(id.as_str()), "{error:#}");
    }

    #[test]
    #[cfg(unix)]
    fn put_creates_owner_only_root_and_file() {
        let (store, _temporary) = new_store();
        let id = store.put(b"permissions").expect("put");

        assert_eq!(mode(store.root()), 0o700);
        assert_eq!(mode(&store.root().join(id.as_str())), 0o600);
    }

    #[test]
    #[cfg(unix)]
    fn existing_root_is_tightened_to_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let (store, _temporary) = new_store();
        fs::create_dir_all(store.root()).expect("create root");
        fs::set_permissions(store.root(), fs::Permissions::from_mode(0o755)).expect("chmod");

        store.put(b"image").expect("put");

        assert_eq!(mode(store.root()), 0o700);
    }

    #[test]
    #[cfg(unix)]
    fn symlinked_root_is_refused_with_or_without_trailing_separator() {
        use std::os::unix::fs::symlink;

        let (store, temporary) = new_store();
        let target = temporary.path().join("target");
        fs::create_dir(&target).expect("target");
        symlink(&target, store.root()).expect("root symlink");

        let error = store.put(b"image").expect_err("symlink root");
        assert!(format!("{error:#}").contains("symlink"), "{error:#}");
        assert_eq!(fs::read_dir(&target).expect("target dir").count(), 0);

        let with_separator = BlobStore::new(PathBuf::from(format!("{}/", store.root().display())));
        let error = with_separator
            .put(b"image")
            .expect_err("trailing symlink root");
        assert!(format!("{error:#}").contains("symlink"), "{error:#}");
    }

    #[test]
    fn non_directory_root_is_refused() {
        let (store, _temporary) = new_store();
        fs::write(store.root(), b"not a directory").expect("root file");

        let error = store.put(b"image").expect_err("file root");
        assert!(
            format!("{error:#}").contains("not a directory"),
            "{error:#}"
        );
        assert_eq!(
            fs::read(store.root()).expect("read root file"),
            b"not a directory"
        );
    }

    #[test]
    #[cfg(unix)]
    fn symlinked_target_is_refused_and_not_followed() {
        use std::os::unix::fs::symlink;

        let (store, temporary) = new_store();
        let bytes = b"image";
        let id = store.put(bytes).expect("put");
        let target = temporary.path().join("elsewhere");
        fs::write(&target, bytes).expect("target file");
        let path = store.root().join(id.as_str());
        fs::remove_file(&path).expect("remove blob");
        symlink(&target, &path).expect("target symlink");

        let error = store.read_verified(&id).expect_err("symlink target");
        assert!(format!("{error:#}").contains("symlink"), "{error:#}");
        let error = store.put(bytes).expect_err("put over symlink");
        assert!(format!("{error:#}").contains("symlink"), "{error:#}");
        assert_eq!(fs::read(&target).expect("target remains"), bytes);
        assert!(
            fs::symlink_metadata(&path)
                .expect("target metadata")
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn non_regular_target_is_refused() {
        let (store, _temporary) = new_store();
        let bytes = b"image";
        let id = ContentId::from_bytes(bytes);
        fs::create_dir_all(store.root().join(id.as_str())).expect("directory target");

        let error = store.put(bytes).expect_err("directory target");
        assert!(
            format!("{error:#}").contains("not a regular file"),
            "{error:#}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn fifo_target_is_refused_without_blocking() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt as _;

        let (store, _temporary) = new_store();
        let bytes = b"image";
        let id = ContentId::from_bytes(bytes);
        fs::create_dir_all(store.root()).expect("root");
        let path = store.root().join(id.as_str());
        let c_path = CString::new(path.as_os_str().as_bytes()).expect("path");
        // SAFETY: `c_path` is a valid NUL-terminated path and the mode is a
        // plain integer; `mkfifo` does not retain the pointer.
        let result = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        assert_eq!(result, 0, "mkfifo failed");

        let error = store.read_verified(&id).expect_err("fifo target");
        assert!(
            format!("{error:#}").contains("not a regular file"),
            "{error:#}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn link_verified_creates_a_shared_inode_and_syncs_destination() {
        use std::os::unix::fs::MetadataExt as _;

        let (source, _source_temp) = new_store();
        let (destination, _destination_temp) = new_store();
        let id = source.put(b"shared image").expect("source put");

        destination
            .link_verified(&source, &id)
            .expect("link verified");

        let source_path = source.root().join(id.as_str());
        let destination_path = destination.root().join(id.as_str());
        let source_metadata = fs::metadata(&source_path).expect("source metadata");
        let destination_metadata = fs::metadata(&destination_path).expect("destination metadata");
        assert_eq!(source_metadata.ino(), destination_metadata.ino());
        assert!(source_metadata.nlink() >= 2);
        assert_eq!(
            destination
                .read_verified(&id)
                .expect("destination verifies"),
            b"shared image"
        );
        assert_eq!(mode(destination.root()), 0o700);
    }

    #[test]
    fn link_verified_refuses_a_mismatched_destination() {
        let (source, _source_temp) = new_store();
        let (destination, _destination_temp) = new_store();
        let id = source.put(b"shared image").expect("source put");
        fs::create_dir_all(destination.root()).expect("destination root");
        let destination_path = destination.root().join(id.as_str());
        fs::write(&destination_path, b"wrong bytes").expect("destination file");

        let error = destination
            .link_verified(&source, &id)
            .expect_err("mismatched destination");
        assert!(format!("{error:#}").contains(id.as_str()), "{error:#}");
        assert_eq!(
            fs::read(&destination_path).expect("destination remains"),
            b"wrong bytes"
        );
    }

    fn image_with_bytes(bytes: &[u8]) -> ImagePart {
        ImagePart {
            media_type: "image/png".to_string(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            content_id: None,
        }
    }

    fn message_with_images(images: Vec<ImagePart>) -> SessionRecord {
        SessionRecord::Message(MessageData {
            role: Role::User,
            content: "look".to_string(),
            id: None,
            status: None,
            timestamp: None,
            images,
        })
    }

    fn output_with_images(images: Vec<ImagePart>) -> SessionRecord {
        SessionRecord::FunctionCallOutput(FunctionCallOutputData {
            call_id: "call-1".to_string(),
            output: "ReadImage /tmp/image.png".to_string(),
            replay: None,
            timestamp: None,
            images,
        })
    }

    fn retained_output_with_images(images: Vec<ImagePart>) -> RetainedRecord {
        RetainedRecord {
            record: RetainedConversationRecord::FunctionCallOutput(FunctionCallOutputData {
                call_id: "call-1".to_string(),
                output: "ReadImage /tmp/image.png".to_string(),
                replay: None,
                timestamp: None,
                images,
            }),
            pin: None,
            source_task: None,
            origin: None,
            skill_read: None,
        }
    }

    #[test]
    fn externalize_and_hydrate_round_trip_message_and_output_images() {
        let (store, _temporary) = new_store();
        let bytes = b"image bytes";
        let message = message_with_images(vec![image_with_bytes(bytes)]);

        let externalized = store.externalize_record(&message).expect("externalize");
        let SessionRecord::Message(data) = &externalized else {
            panic!("message shape");
        };
        assert!(data.images[0].data_base64.is_empty());
        let id = data.images[0].content_id.clone().expect("content id");
        assert_eq!(
            store
                .read_verified(&ContentId::parse(&id).expect("parse"))
                .expect("stored bytes"),
            bytes
        );
        let SessionRecord::Message(original) = &message else {
            panic!("message shape");
        };
        assert_eq!(
            original.images[0].data_base64,
            base64::engine::general_purpose::STANDARD.encode(bytes),
            "externalization must not mutate the live record"
        );

        let output = output_with_images(vec![image_with_bytes(bytes)]);
        let externalized_output = store
            .externalize_record(&output)
            .expect("externalize output");
        let SessionRecord::FunctionCallOutput(output_data) = &externalized_output else {
            panic!("function call output shape");
        };
        assert!(output_data.images[0].data_base64.is_empty());
        assert_eq!(
            output_data.images[0].content_id.as_deref(),
            Some(id.as_str())
        );

        let mut records = vec![externalized, externalized_output];
        store
            .hydrate_session("session-1", &mut records)
            .expect("hydrate");
        let SessionRecord::Message(hydrated_message) = &records[0] else {
            panic!("hydrated message shape");
        };
        assert_eq!(
            hydrated_message.images[0].data_base64,
            base64::engine::general_purpose::STANDARD.encode(bytes)
        );
        assert_eq!(
            hydrated_message.images[0].content_id.as_deref(),
            Some(id.as_str())
        );

        let externalized_again = store
            .externalize_record(&records[0])
            .expect("idempotent externalize");
        let SessionRecord::Message(again) = &externalized_again else {
            panic!("message shape");
        };
        assert!(again.images[0].data_base64.is_empty());
        assert_eq!(again.images[0].content_id.as_deref(), Some(id.as_str()));
        assert_eq!(fs::read_dir(store.root()).expect("read root").count(), 1);
    }

    #[test]
    fn externalize_rejects_an_image_without_a_payload_reference() {
        let (store, _temporary) = new_store();
        let record = message_with_images(vec![ImagePart {
            media_type: "image/png".to_string(),
            data_base64: String::new(),
            content_id: None,
        }]);

        let error = store
            .externalize_record(&record)
            .expect_err("empty image must fail");
        assert!(
            format!("{error:#}").contains("neither inline bytes nor a content id"),
            "{error:#}"
        );
        assert!(!store.root().exists());
    }

    #[test]
    fn externalize_and_hydrate_round_trip_checkpoint_retained_images() {
        let (store, _temporary) = new_store();
        let message_bytes = b"retained message image";
        let output_bytes = b"retained output image";
        let mut retained_message = test_support::message(Role::User, "look");
        let RetainedConversationRecord::Message(data) = &mut retained_message.record else {
            panic!("retained message shape");
        };
        data.images.push(image_with_bytes(message_bytes));
        let retained_output = retained_output_with_images(vec![image_with_bytes(output_bytes)]);
        let call = test_support::exchange("call-1", "out").remove(0);
        let checkpoint = test_support::checkpoint(
            "checkpoint-1",
            "summary",
            vec![retained_message, call, retained_output],
        );

        let externalized = store
            .externalize_checkpoint(&checkpoint)
            .expect("externalize checkpoint");
        let RetainedConversationRecord::Message(message) = &externalized.retained[0].record else {
            panic!("retained message shape");
        };
        let message_id = message.images[0].content_id.clone().expect("message id");
        assert!(message.images[0].data_base64.is_empty());
        let RetainedConversationRecord::FunctionCallOutput(output) =
            &externalized.retained[2].record
        else {
            panic!("retained output shape");
        };
        let output_id = output.images[0].content_id.clone().expect("output id");
        assert!(output.images[0].data_base64.is_empty());
        assert_eq!(output.output, "ReadImage /tmp/image.png");
        assert_ne!(message_id, output_id);
        assert_eq!(fs::read_dir(store.root()).expect("read root").count(), 2);

        let RetainedConversationRecord::Message(original) = &checkpoint.retained[0].record else {
            panic!("original retained message shape");
        };
        assert_eq!(
            original.images[0].data_base64,
            base64::engine::general_purpose::STANDARD.encode(message_bytes),
            "externalization must not mutate the live checkpoint"
        );

        let mut records = vec![SessionRecord::Compaction(externalized)];
        store
            .hydrate_session("session-1", &mut records)
            .expect("hydrate checkpoint");
        let SessionRecord::Compaction(hydrated) = &records[0] else {
            panic!("checkpoint shape");
        };
        let RetainedConversationRecord::Message(message) = &hydrated.retained[0].record else {
            panic!("hydrated message shape");
        };
        assert_eq!(
            message.images[0].data_base64,
            base64::engine::general_purpose::STANDARD.encode(message_bytes)
        );
        let RetainedConversationRecord::FunctionCallOutput(output) = &hydrated.retained[2].record
        else {
            panic!("hydrated output shape");
        };
        assert_eq!(
            output.images[0].data_base64,
            base64::engine::general_purpose::STANDARD.encode(output_bytes)
        );
    }

    #[test]
    fn checkpoint_externalize_rejects_an_image_without_a_payload_reference() {
        let (store, _temporary) = new_store();
        let mut retained = test_support::message(Role::User, "look");
        let RetainedConversationRecord::Message(data) = &mut retained.record else {
            panic!("retained message shape");
        };
        data.images.push(ImagePart {
            media_type: "image/png".to_string(),
            data_base64: String::new(),
            content_id: None,
        });
        let checkpoint = test_support::checkpoint("checkpoint-1", "summary", vec![retained]);

        let error = store
            .externalize_checkpoint(&checkpoint)
            .expect_err("empty retained image must fail");
        assert!(
            format!("{error:#}").contains(
                "checkpoint retained 0 message image 0 has neither inline bytes nor a content id"
            ),
            "{error:#}"
        );
        assert!(!store.root().exists());
    }

    #[test]
    fn hydrate_rejects_a_missing_blob_and_a_mismatched_inline_id() {
        let (store, _temporary) = new_store();
        let missing = ContentId::from_bytes(b"missing image");
        let mut records = vec![message_with_images(vec![ImagePart {
            media_type: "image/png".to_string(),
            data_base64: String::new(),
            content_id: Some(missing.as_str().to_string()),
        }])];

        let error = store
            .hydrate_session("session-1", &mut records)
            .expect_err("missing blob");
        let text = format!("{error:#}");
        assert!(text.contains("session-1"), "{text}");
        assert!(text.contains(missing.as_str()), "{text}");
        assert!(text.contains("does not exist"), "{text}");

        let declared = ContentId::from_bytes(b"different image");
        let mut records = vec![message_with_images(vec![ImagePart {
            media_type: "image/png".to_string(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(b"inline image"),
            content_id: Some(declared.as_str().to_string()),
        }])];
        let error = store
            .hydrate_session("session-2", &mut records)
            .expect_err("mismatched inline id");
        let text = format!("{error:#}");
        assert!(text.contains("session-2"), "{text}");
        assert!(text.contains(declared.as_str()), "{text}");
        assert!(text.contains("do not match"), "{text}");
    }

    #[test]
    fn hydrate_restores_checkpoint_retained_images() {
        let (store, _temporary) = new_store();
        let bytes = b"retained image";
        let id = store.put(bytes).expect("put");
        let mut retained = test_support::message(Role::User, "look");
        let RetainedConversationRecord::Message(data) = &mut retained.record else {
            panic!("retained message shape");
        };
        data.images.push(ImagePart {
            media_type: "image/png".to_string(),
            data_base64: String::new(),
            content_id: Some(id.as_str().to_string()),
        });
        let checkpoint = test_support::checkpoint("checkpoint-1", "summary", vec![retained]);
        let mut records = vec![SessionRecord::Compaction(checkpoint)];

        store
            .hydrate_session("session-1", &mut records)
            .expect("hydrate checkpoint");

        let SessionRecord::Compaction(checkpoint) = &records[0] else {
            panic!("checkpoint shape");
        };
        let RetainedConversationRecord::Message(data) = &checkpoint.retained[0].record else {
            panic!("retained message shape");
        };
        assert_eq!(
            data.images[0].data_base64,
            base64::engine::general_purpose::STANDARD.encode(bytes)
        );
    }

    #[test]
    fn hydrate_keeps_a_legacy_inline_checkpoint_image_without_a_blob() {
        let (store, _temporary) = new_store();
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"legacy image");
        let mut retained = test_support::message(Role::User, "look");
        let RetainedConversationRecord::Message(data) = &mut retained.record else {
            panic!("retained message shape");
        };
        data.images.push(ImagePart {
            media_type: "image/png".to_string(),
            data_base64: encoded.clone(),
            content_id: None,
        });
        let checkpoint = test_support::checkpoint("checkpoint-1", "summary", vec![retained]);
        let mut records = vec![SessionRecord::Compaction(checkpoint)];

        store
            .hydrate_session("session-1", &mut records)
            .expect("legacy inline checkpoint hydrates");

        let SessionRecord::Compaction(checkpoint) = &records[0] else {
            panic!("checkpoint shape");
        };
        let RetainedConversationRecord::Message(data) = &checkpoint.retained[0].record else {
            panic!("retained message shape");
        };
        assert_eq!(data.images[0].data_base64, encoded);
        assert_eq!(data.images[0].content_id, None);
        assert!(!store.root().exists(), "hydration must not create a store");
    }

    fn write_diagnostic_session(sessions_dir: &Path, session_id: &str, content_ids: &[&ContentId]) {
        fs::create_dir_all(sessions_dir).expect("sessions directory");
        let images = content_ids
            .iter()
            .map(|id| {
                serde_json::json!({
                    "media_type": "image/png",
                    "content_id": id.as_str(),
                })
            })
            .collect::<Vec<_>>();
        let meta = serde_json::json!({
            "type": "session_meta",
            "format_version": 4,
            "session_id": session_id,
            "timestamp": "2026-01-02T03:00:00Z",
            "working_directory": "/work",
            "tools": [],
        });
        let message = serde_json::json!({
            "type": "message",
            "role": "user",
            "content": "look",
            "timestamp": "2026-01-02T03:00:01Z",
            "images": images,
        });
        fs::write(
            sessions_dir.join(format!("{session_id}.jsonl")),
            format!(
                "{}\n{}\n",
                serde_json::to_string(&meta).expect("serialize meta"),
                serde_json::to_string(&message).expect("serialize message")
            ),
        )
        .expect("write diagnostic session");
    }

    #[test]
    fn recover_removes_unreferenced_blobs_and_temporary_files() {
        let (store, _temporary) = new_store();
        let referenced = store.put(b"referenced image").expect("referenced blob");
        let orphan = store.put(b"orphan image").expect("orphan blob");
        let temp_path = store.root().join(format!("{TEMP_PREFIX}interrupted"));
        fs::write(&temp_path, b"partial image").expect("temporary blob");

        let report = store
            .recover_unreferenced("session-1", &BTreeSet::from([referenced.clone()]))
            .expect("recover");

        assert_eq!(report.removed_blobs, vec![orphan]);
        assert_eq!(report.removed_temporary_files, vec![temp_path.clone()]);
        assert!(report.errors.is_empty());
        assert!(store.root().join(referenced.as_str()).exists());
        assert!(!temp_path.exists());
        assert_eq!(fs::read_dir(store.root()).expect("read root").count(), 1);
    }

    #[test]
    fn recover_missing_root_does_not_create_it() {
        let (store, _temporary) = new_store();

        let report = store
            .recover_unreferenced("session-1", &BTreeSet::new())
            .expect("recover missing root");

        assert!(report.removed_blobs.is_empty());
        assert!(report.removed_temporary_files.is_empty());
        assert!(report.errors.is_empty());
        assert!(!store.root().exists());
    }

    #[test]
    #[cfg(unix)]
    fn recover_refuses_a_symlinked_root_without_deleting() {
        use std::os::unix::fs::symlink;

        let (store, temporary) = new_store();
        let target = temporary.path().join("target");
        fs::create_dir(&target).expect("target directory");
        let orphan = ContentId::from_bytes(b"orphan");
        let orphan_path = target.join(orphan.as_str());
        fs::write(&orphan_path, b"orphan").expect("orphan blob");
        symlink(&target, store.root()).expect("root symlink");

        let error = store
            .recover_unreferenced("session-1", &BTreeSet::new())
            .expect_err("symlinked root must be refused");

        assert!(format!("{error:#}").contains("symlink"), "{error:#}");
        assert!(orphan_path.exists(), "recovery must not traverse the root");
    }

    #[test]
    #[cfg(unix)]
    fn recover_ignores_symlinks_directories_and_unknown_names() {
        use std::os::unix::fs::symlink;

        let (store, temporary) = new_store();
        let referenced = store.put(b"referenced").expect("create root");
        let linked_id = ContentId::from_bytes(b"linked file");
        let target = temporary.path().join("target");
        fs::write(&target, b"target").expect("target file");
        let linked_path = store.root().join(linked_id.as_str());
        symlink(&target, &linked_path).expect("blob symlink");
        let directory_id = ContentId::from_bytes(b"directory");
        let directory = store.root().join(directory_id.as_str());
        fs::create_dir(&directory).expect("digest directory");
        let unknown = store.root().join("notes.txt");
        fs::write(&unknown, b"not a blob").expect("unknown file");

        let report = store
            .recover_unreferenced("session-1", &BTreeSet::from([referenced]))
            .expect("recover");

        assert!(report.errors.is_empty());
        assert!(
            report.removed_blobs.is_empty(),
            "symlinks and directories are not published blobs"
        );
        assert!(linked_path.exists());
        assert!(directory.exists());
        assert!(unknown.exists());
        assert_eq!(fs::read(&target).expect("target remains"), b"target");
    }

    #[test]
    fn recover_keeps_a_corrupt_referenced_blob() {
        let (store, _temporary) = new_store();
        let id = store.put(b"referenced").expect("referenced blob");
        let path = store.root().join(id.as_str());
        fs::write(&path, b"corrupt").expect("corrupt blob");

        let report = store
            .recover_unreferenced("session-1", &BTreeSet::from([id]))
            .expect("recover");

        assert!(report.removed_blobs.is_empty());
        assert_eq!(fs::read(&path).expect("corrupt blob remains"), b"corrupt");
    }

    #[test]
    fn referenced_ids_reject_a_malformed_content_id() {
        let message = message_with_images(vec![ImagePart {
            media_type: "image/png".to_string(),
            data_base64: String::new(),
            content_id: Some("not-a-digest".to_string()),
        }]);

        let error = referenced_content_ids(&[message]).expect_err("invalid id must fail");

        assert!(
            format!("{error:#}").contains("content id 0 is invalid"),
            "{error:#}"
        );
    }

    #[test]
    fn referenced_ids_cover_messages_outputs_and_checkpoint_images() {
        let message_id = ContentId::from_bytes(b"message image");
        let output_id = ContentId::from_bytes(b"output image");
        let retained_id = ContentId::from_bytes(b"retained image");
        let message = message_with_images(vec![ImagePart {
            media_type: "image/png".to_string(),
            data_base64: String::new(),
            content_id: Some(message_id.as_str().to_string()),
        }]);
        let output = output_with_images(vec![ImagePart {
            media_type: "image/png".to_string(),
            data_base64: String::new(),
            content_id: Some(output_id.as_str().to_string()),
        }]);
        let mut retained = test_support::message(Role::User, "look");
        let RetainedConversationRecord::Message(data) = &mut retained.record else {
            panic!("retained message shape");
        };
        data.images.push(ImagePart {
            media_type: "image/png".to_string(),
            data_base64: String::new(),
            content_id: Some(retained_id.as_str().to_string()),
        });
        let checkpoint = test_support::checkpoint("checkpoint-1", "summary", vec![retained]);

        let referenced =
            referenced_content_ids(&[message, output, SessionRecord::Compaction(checkpoint)])
                .expect("collect references");

        assert_eq!(
            referenced,
            BTreeSet::from([message_id, output_id, retained_id])
        );
    }

    #[test]
    #[expect(
        clippy::cognitive_complexity,
        reason = "one fixture asserts every diagnostic category"
    )]
    fn diagnostics_report_missing_corrupt_unreferenced_and_temporary_files() {
        let temporary = TempDir::new().expect("temporary directory");
        let sessions = temporary.path().join("sessions");
        let session_id = "session-1";
        let valid = ContentId::from_bytes(b"valid image");
        let corrupt = ContentId::from_bytes(b"corrupt image");
        let missing = ContentId::from_bytes(b"missing image");
        let orphan = ContentId::from_bytes(b"orphan image");
        write_diagnostic_session(&sessions, session_id, &[&valid, &corrupt, &missing]);
        let blob_dir = sessions.join(format!("{session_id}.blobs"));
        let store = BlobStore::new(blob_dir.clone());
        store.put(b"valid image").expect("valid blob");
        fs::write(blob_dir.join(corrupt.as_str()), b"not the corrupt image").expect("corrupt blob");
        fs::write(blob_dir.join(orphan.as_str()), b"orphan image").expect("orphan blob");
        let temp_path = blob_dir.join(format!("{TEMP_PREFIX}stale"));
        fs::write(&temp_path, b"temporary").expect("temporary blob");

        let diagnostics = blob_diagnostics(&sessions).expect("diagnostics");

        assert_eq!(diagnostics.sessions.len(), 1);
        let session = &diagnostics.sessions[0];
        assert_eq!(session.session_id, session_id);
        assert_eq!(session.blob_count, 3);
        assert_eq!(session.referenced_count, 3);
        assert_eq!(session.unreferenced_count, 1);
        assert_eq!(session.unreferenced_bytes, b"orphan image".len() as u64);
        assert_eq!(session.temporary_file_count, 1);
        assert_eq!(session.temporary_files, vec![temp_path.clone()]);
        assert_eq!(session.missing_references.len(), 1);
        assert_eq!(session.missing_references[0].digest, missing.as_str());
        assert_eq!(session.corrupt_references.len(), 1);
        assert_eq!(session.corrupt_references[0].digest, corrupt.as_str());
        assert!(session.references_complete);
        assert!(session.session_error.is_none());
        assert!(session.directory_error.is_none());
        assert!(diagnostics.orphaned_stores.is_empty());
        assert_eq!(diagnostics.totals.session_count, 1);
        assert_eq!(diagnostics.totals.blob_count, 3);
        assert_eq!(diagnostics.totals.referenced_count, 3);
        assert_eq!(diagnostics.totals.unreferenced_count, 1);
        assert_eq!(diagnostics.totals.temporary_file_count, 1);
        assert_eq!(diagnostics.totals.missing_reference_count, 1);
        assert_eq!(diagnostics.totals.corrupt_reference_count, 1);

        assert!(!blob_dir.join(missing.as_str()).exists());
        assert!(blob_dir.join(corrupt.as_str()).exists());
        assert!(blob_dir.join(orphan.as_str()).exists());
        assert!(temp_path.exists(), "diagnostics must not delete");
    }

    #[test]
    fn diagnostics_report_an_orphan_store_and_a_session_without_blobs() {
        let temporary = TempDir::new().expect("temporary directory");
        let sessions = temporary.path().join("sessions");
        let missing = ContentId::from_bytes(b"missing image");
        write_diagnostic_session(&sessions, "session-1", &[&missing]);
        let orphan = ContentId::from_bytes(b"orphan image");
        let orphan_dir = sessions.join("orphan.blobs");
        fs::create_dir_all(&orphan_dir).expect("orphan store");
        fs::write(orphan_dir.join(orphan.as_str()), b"orphan image").expect("orphan blob");

        let diagnostics = blob_diagnostics(&sessions).expect("diagnostics");

        assert_eq!(diagnostics.sessions.len(), 1);
        assert_eq!(diagnostics.sessions[0].blob_count, 0);
        assert_eq!(diagnostics.sessions[0].stored_bytes, 0);
        assert_eq!(
            diagnostics.sessions[0].missing_references[0].digest,
            missing.as_str()
        );
        assert!(
            !sessions.join("session-1.blobs").exists(),
            "diagnostics must not create a blob directory"
        );
        assert_eq!(diagnostics.orphaned_stores.len(), 1);
        assert_eq!(diagnostics.orphaned_stores[0].blob_count, 1);
        assert_eq!(
            diagnostics.orphaned_stores[0].stored_bytes,
            b"orphan image".len() as u64
        );
        assert_eq!(diagnostics.totals.orphaned_store_count, 1);
        assert_eq!(diagnostics.totals.orphaned_store_blob_count, 1);
        assert_eq!(
            diagnostics.totals.orphaned_store_bytes,
            b"orphan image".len() as u64
        );
        assert!(orphan_dir.join(orphan.as_str()).exists());
    }

    #[test]
    #[cfg(unix)]
    fn diagnostics_ignore_a_symlink_in_a_blob_directory() {
        use std::os::unix::fs::symlink;

        let temporary = TempDir::new().expect("temporary directory");
        let sessions = temporary.path().join("sessions");
        write_diagnostic_session(&sessions, "session-1", &[]);
        let blob_dir = sessions.join("session-1.blobs");
        fs::create_dir_all(&blob_dir).expect("blob directory");
        let linked_id = ContentId::from_bytes(b"linked file");
        let target = temporary.path().join("target");
        fs::write(&target, b"target").expect("target file");
        symlink(&target, blob_dir.join(linked_id.as_str())).expect("blob symlink");

        let diagnostics = blob_diagnostics(&sessions).expect("diagnostics");

        let session = &diagnostics.sessions[0];
        assert_eq!(session.blob_count, 0);
        assert_eq!(session.stored_bytes, 0);
        assert!(session.directory_error.is_none());
        assert!(blob_dir.join(linked_id.as_str()).exists());
        assert_eq!(fs::read(&target).expect("target remains"), b"target");
    }

    #[test]
    #[cfg(unix)]
    fn diagnostics_refuse_a_symlinked_blob_directory() {
        use std::os::unix::fs::symlink;

        let temporary = TempDir::new().expect("temporary directory");
        let sessions = temporary.path().join("sessions");
        let id = ContentId::from_bytes(b"image");
        write_diagnostic_session(&sessions, "session-1", &[&id]);
        let target = temporary.path().join("target");
        fs::create_dir(&target).expect("target directory");
        let blob_path = target.join(id.as_str());
        fs::write(&blob_path, b"image").expect("target blob");
        symlink(&target, sessions.join("session-1.blobs")).expect("blob dir symlink");

        let diagnostics = blob_diagnostics(&sessions).expect("diagnostics");

        let session = &diagnostics.sessions[0];
        assert!(session.directory_error.is_some());
        assert_eq!(session.blob_count, 0);
        assert!(
            session.missing_references.is_empty(),
            "no traversal on a symlink"
        );
        assert!(blob_path.exists());
    }

    #[test]
    fn diagnostics_report_a_malformed_session_without_failing() {
        let temporary = TempDir::new().expect("temporary directory");
        let sessions = temporary.path().join("sessions");
        fs::create_dir_all(&sessions).expect("sessions directory");
        fs::write(
            sessions.join("session-1.jsonl"),
            "{\"type\":\"message\"}\nnot json\n",
        )
        .expect("malformed session");

        let diagnostics = blob_diagnostics(&sessions).expect("diagnostics");

        assert_eq!(diagnostics.sessions.len(), 1);
        assert!(!diagnostics.sessions[0].references_complete);
        assert!(diagnostics.sessions[0].session_error.is_some());
        assert_eq!(diagnostics.sessions[0].blob_count, 0);
    }

    #[test]
    #[cfg(unix)]
    fn diagnostics_deduplicate_hard_linked_fork_bytes() {
        let temporary = TempDir::new().expect("temporary directory");
        let sessions = temporary.path().join("sessions");
        let id = ContentId::from_bytes(b"shared image");
        write_diagnostic_session(&sessions, "parent", &[&id]);
        write_diagnostic_session(&sessions, "fork", &[&id]);
        let parent = BlobStore::new(sessions.join("parent.blobs"));
        let fork = BlobStore::new(sessions.join("fork.blobs"));
        parent.put(b"shared image").expect("parent blob");
        fork.link_verified(&parent, &id).expect("fork link");

        let diagnostics = blob_diagnostics(&sessions).expect("diagnostics");

        assert_eq!(diagnostics.totals.session_count, 2);
        assert_eq!(diagnostics.totals.blob_count, 2);
        assert_eq!(diagnostics.totals.unique_blob_count, 1);
        assert_eq!(
            diagnostics.totals.stored_bytes,
            b"shared image".len() as u64
        );
        assert_eq!(diagnostics.totals.referenced_count, 2);
        assert_eq!(
            diagnostics.totals.referenced_bytes,
            b"shared image".len() as u64
        );
    }

    #[test]
    fn temp_names_are_distinct_from_digests() {
        let first = temp_file_name();
        let second = temp_file_name();
        assert!(is_temp_name(&first));
        assert!(is_temp_name(&second));
        assert_ne!(first, second);
        assert!(!is_digest_name(&first));
    }
}
