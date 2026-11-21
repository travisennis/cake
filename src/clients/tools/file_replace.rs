//! Atomic replacement for the in-process Edit and Write tools.
//!
//! A replacement is prepared in a hidden sibling file and renamed over the
//! target only after every preparation step succeeds. Preparation failures
//! remove the sibling and leave the target entry untouched.

use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const STAGE_PREFIX: &str = ".cake-replace-";
const STAGE_ATTEMPTS: usize = 8;
const WRITE_CHUNK_BYTES: usize = 64 * 1024;
const HASH_BUFFER_BYTES: usize = 8 * 1024;

/// Replace `target` with `content` through a staged sibling and an atomic
/// rename.
///
/// # Errors
///
/// Returns a model-visible diagnostic when the target is not a replaceable
/// regular file, when preparation fails, when the target changes during
/// preparation, or when publication fails. A failed preparation attempts to
/// remove its sibling; a cleanup failure is included in the diagnostic.
pub(super) fn replace_file(target: &Path, content: &[u8]) -> Result<(), String> {
    let snapshot = inspect_target(target)?;
    let mut stage = StageFile::create(target, &snapshot)?;
    stage
        .write_and_publish(target, content, &snapshot)
        .map_err(|error| stage.failure_message(&error))
}

/// What the target looked like before staging began.
struct TargetSnapshot {
    identity: Option<FileIdentity>,
    permissions: Option<Permissions>,
}

impl TargetSnapshot {
    const fn missing() -> Self {
        Self {
            identity: None,
            permissions: None,
        }
    }

    fn existing(metadata: &fs::Metadata) -> Self {
        Self {
            identity: Some(FileIdentity::from_metadata(metadata)),
            permissions: Some(stage_permissions(metadata)),
        }
    }

    const fn was_present(&self) -> bool {
        self.identity.is_some()
    }
}

/// Identity of a directory entry, used to reject concurrent replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    len: u64,
    nlink: u64,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    mode: u32,
    #[cfg(not(unix))]
    readonly: bool,
}

impl FileIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            Self {
                len: metadata.len(),
                nlink: metadata.nlink(),
                dev: metadata.dev(),
                ino: metadata.ino(),
                mode: metadata.permissions().mode() & 0o777,
            }
        }

        #[cfg(not(unix))]
        {
            Self {
                len: metadata.len(),
                nlink: hard_link_count(metadata),
                readonly: metadata.permissions().readonly(),
            }
        }
    }

    fn matches(self, metadata: &fs::Metadata) -> bool {
        self == Self::from_metadata(metadata)
    }
}

/// A staged replacement that removes itself unless publication succeeds.
struct StageFile {
    path: PathBuf,
    file: Option<File>,
    identity: Option<FileIdentity>,
    active: bool,
}

impl StageFile {
    fn create(target: &Path, snapshot: &TargetSnapshot) -> Result<Self, String> {
        let parent = target.parent().ok_or_else(|| {
            format!(
                "Cannot replace '{}': the path has no parent directory.",
                target.display()
            )
        })?;
        let (path, file) = create_stage(parent)?;
        let mut stage = Self {
            path,
            file: Some(file),
            identity: None,
            active: true,
        };
        match stage.initialize(snapshot) {
            Ok(()) => Ok(stage),
            Err(error) => Err(stage.failure_message(&error)),
        }
    }

    fn initialize(&mut self, snapshot: &TargetSnapshot) -> io::Result<()> {
        let identity = FileIdentity::from_metadata(&self.file_ref()?.metadata()?);
        self.identity = Some(identity);
        if identity.nlink != 1 {
            return Err(io::Error::other(
                "temporary replacement file has more than one link",
            ));
        }
        if let Some(permissions) = snapshot.permissions.clone() {
            self.file_ref()?.set_permissions(permissions)?;
            self.refresh_identity()?;
        }
        Ok(())
    }

    fn write_and_publish(
        &mut self,
        target: &Path,
        content: &[u8],
        snapshot: &TargetSnapshot,
    ) -> io::Result<()> {
        self.write_content(content)?;
        self.refresh_identity()?;
        self.file_ref()?.sync_all()?;
        self.verify_content(content)?;
        revalidate_target(target, snapshot)?;
        self.revalidate_stage()?;
        self.verify_content(content)?;
        self.revalidate_stage()?;
        self.publish(target)
    }

    fn write_content(&mut self, content: &[u8]) -> io::Result<()> {
        write_chunks(self.file_mut()?, content)
    }

    fn verify_content(&mut self, content: &[u8]) -> io::Result<()> {
        let path = self.path.clone();
        let file = self.file_mut()?;
        file.seek(SeekFrom::Start(0))?;
        let actual = sha256_reader(file).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("failed to read back '{}': {error}", path.display()),
            )
        })?;
        if actual != sha256_bytes(content) {
            return Err(stage_changed(&path));
        }
        Ok(())
    }

    fn refresh_identity(&mut self) -> io::Result<()> {
        let identity = FileIdentity::from_metadata(&self.file_ref()?.metadata()?);
        self.identity = Some(identity);
        Ok(())
    }

    fn revalidate_stage(&self) -> io::Result<()> {
        let expected = self
            .identity
            .ok_or_else(|| io::Error::other("temporary replacement identity is missing"))?;

        let path_metadata = fs::symlink_metadata(&self.path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "failed to revalidate temporary replacement '{}': {error}",
                    self.path.display()
                ),
            )
        })?;
        if !path_metadata.is_file() || !expected.matches(&path_metadata) {
            return Err(stage_changed(&self.path));
        }

        let handle_metadata = self.file_ref()?.metadata()?;
        if !expected.matches(&handle_metadata) || hard_link_count(&handle_metadata) != 1 {
            return Err(stage_changed(&self.path));
        }
        Ok(())
    }

    fn publish(&mut self, target: &Path) -> io::Result<()> {
        drop(self.file.take());
        fs::rename(&self.path, target).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("failed to publish '{}': {error}", target.display()),
            )
        })?;
        self.active = false;
        Ok(())
    }

    fn failure_message(&mut self, error: &io::Error) -> String {
        self.cleanup().map_or_else(
            || error.to_string(),
            |residue| format!("{error}; {residue}"),
        )
    }

    fn cleanup(&mut self) -> Option<String> {
        if !self.active {
            return None;
        }
        self.active = false;
        let expected = self.cleanup_identity();
        drop(self.file.take());

        expected.map_or_else(
            || {
                Some(format!(
                    "temporary replacement file '{}' could not be identified for cleanup and was left in place",
                    self.path.display()
                ))
            },
            |identity| cleanup_stage(&self.path, identity),
        )
    }

    fn cleanup_identity(&self) -> Option<FileIdentity> {
        if let Some(file) = self.file.as_ref()
            && let Ok(metadata) = file.metadata()
        {
            return Some(FileIdentity::from_metadata(&metadata));
        }
        self.identity
    }

    fn file_ref(&self) -> io::Result<&File> {
        self.file
            .as_ref()
            .ok_or_else(|| io::Error::other("temporary replacement file is closed"))
    }

    fn file_mut(&mut self) -> io::Result<&mut File> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("temporary replacement file is closed"))
    }
}

impl Drop for StageFile {
    fn drop(&mut self) {
        drop(self.cleanup());
    }
}

fn inspect_target(target: &Path) -> Result<TargetSnapshot, String> {
    match fs::symlink_metadata(target) {
        Ok(metadata) => existing_target_snapshot(target, &metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(TargetSnapshot::missing()),
        Err(error) => Err(format!(
            "Failed to inspect file '{}': {error}",
            target.display()
        )),
    }
}

fn existing_target_snapshot(
    target: &Path,
    metadata: &fs::Metadata,
) -> Result<TargetSnapshot, String> {
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "Cannot replace '{}': the final path component is a symlink.",
            target.display()
        ));
    }
    if !metadata.is_file() {
        return Err(format!(
            "Cannot replace '{}': the path is not a regular file.",
            target.display()
        ));
    }

    let links = hard_link_count(metadata);
    if links > 1 {
        return Err(format!(
            "Cannot replace '{}': the file has {links} hard links. Replacing it would leave the other links pointing at the old contents.",
            target.display()
        ));
    }

    ensure_writable_mode(target, metadata)?;
    let opened = open_for_replacement(target, metadata)?;
    Ok(TargetSnapshot::existing(&opened))
}

#[cfg(unix)]
fn ensure_writable_mode(target: &Path, metadata: &fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o222 == 0 {
        return Err(format!(
            "Cannot replace '{}': the file has no write permission bits.",
            target.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_writable_mode(target: &Path, metadata: &fs::Metadata) -> Result<(), String> {
    if metadata.permissions().readonly() {
        return Err(format!(
            "Cannot replace '{}': the file is read-only.",
            target.display()
        ));
    }
    Ok(())
}

fn open_for_replacement(target: &Path, metadata: &fs::Metadata) -> Result<fs::Metadata, String> {
    let expected = FileIdentity::from_metadata(metadata);
    let file = OpenOptions::new()
        .write(true)
        .open(target)
        .map_err(|error| format!("Cannot replace '{}': {error}", target.display()))?;
    let opened = file.metadata().map_err(|error| {
        format!(
            "Failed to inspect opened file '{}': {error}",
            target.display()
        )
    })?;
    if !expected.matches(&opened) {
        return Err(target_changed(target));
    }
    Ok(opened)
}

fn create_stage(parent: &Path) -> Result<(PathBuf, File), String> {
    for _ in 0..STAGE_ATTEMPTS {
        let path = parent.join(format!("{STAGE_PREFIX}{}", uuid::Uuid::new_v4().simple()));
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {},
            Err(error) => {
                return Err(format!(
                    "Cannot create a temporary replacement in '{}': {error}. The parent directory must be writable.",
                    parent.display()
                ));
            },
        }
    }

    Err(format!(
        "Cannot create a unique temporary replacement in '{}' after {STAGE_ATTEMPTS} attempts.",
        parent.display()
    ))
}

fn write_chunks(file: &mut File, content: &[u8]) -> io::Result<()> {
    let mut chunks = content.chunks(WRITE_CHUNK_BYTES);
    if let Some(first) = chunks.next() {
        file.write_all(first)?;
    }
    for chunk in chunks {
        file.write_all(chunk)?;
    }
    Ok(())
}

fn sha256_reader(file: &mut File) -> io::Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; HASH_BUFFER_BYTES];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut output = [0u8; 32];
    output.copy_from_slice(&digest);
    Ok(output)
}

fn sha256_bytes(content: &[u8]) -> [u8; 32] {
    let digest = Sha256::digest(content);
    let mut output = [0u8; 32];
    output.copy_from_slice(&digest);
    output
}

fn revalidate_target(target: &Path, snapshot: &TargetSnapshot) -> io::Result<()> {
    match fs::symlink_metadata(target) {
        Ok(metadata) => validate_unchanged_target(target, snapshot, &metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound && !snapshot.was_present() => Ok(()),
        Err(error) => Err(io::Error::new(
            error.kind(),
            format!("failed to revalidate '{}': {error}", target.display()),
        )),
    }
}

fn validate_unchanged_target(
    target: &Path,
    snapshot: &TargetSnapshot,
    metadata: &fs::Metadata,
) -> io::Result<()> {
    if !snapshot.was_present() {
        return Err(target_appeared(target));
    }

    let expected = snapshot
        .identity
        .ok_or_else(|| io::Error::other("target identity is missing"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || !expected.matches(metadata) {
        return Err(io::Error::other(target_changed(target)));
    }
    Ok(())
}

fn cleanup_stage(path: &Path, expected: FileIdentity) -> Option<String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if expected.matches(&metadata) => remove_stage(path),
        Ok(_) => Some(format!(
            "temporary replacement file '{}' was replaced and was left in place",
            path.display()
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => Some(format!(
            "temporary replacement file '{}' could not be inspected for cleanup: {error}",
            path.display()
        )),
    }
}

fn remove_stage(path: &Path) -> Option<String> {
    match fs::remove_file(path) {
        Ok(()) => None,
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => Some(format!(
            "temporary replacement file '{}' could not be removed: {error}",
            path.display()
        )),
    }
}

fn stage_changed(path: &Path) -> io::Error {
    io::Error::other(format!(
        "Temporary replacement file '{}' changed before publication; the target was left unchanged.",
        path.display()
    ))
}

fn target_changed(path: &Path) -> String {
    format!(
        "Target '{}' changed while the replacement was being prepared; retry the write.",
        path.display()
    )
}

fn target_appeared(path: &Path) -> io::Error {
    io::Error::other(format!(
        "Target '{}' appeared while the replacement was being prepared; retry the write.",
        path.display()
    ))
}

#[cfg(unix)]
fn hard_link_count(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink()
}

#[cfg(not(unix))]
fn hard_link_count(_metadata: &fs::Metadata) -> u64 {
    1
}

#[cfg(unix)]
fn stage_permissions(metadata: &fs::Metadata) -> Permissions {
    use std::os::unix::fs::PermissionsExt;
    Permissions::from_mode(metadata.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn stage_permissions(metadata: &fs::Metadata) -> Permissions {
    metadata.permissions()
}
