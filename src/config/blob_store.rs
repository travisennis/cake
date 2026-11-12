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
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use base64::Engine as _;
use sha2::{Digest, Sha256};

use crate::config::owner_only;
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
