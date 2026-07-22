//! Shared secure temp directory for tools that write files to temp dirs.
//!
//! On Unix, creates `<temp_dir>/cake-<uid>-<random>` with `0o700` permissions.
//! The random suffix (64 bits of entropy) prevents pre-creation attacks on
//! shared sticky-bit temp directories: an attacker cannot predict the full
//! path and therefore cannot create a directory or symlink that would cause
//! permanent denial of service or ownership validation failure.  The path
//! is cached after first creation so subsequent calls are instant and
//! consistent within a single process invocation.
//!
//! On non-Unix platforms it falls back to `<temp_dir>/cake`.
//!
//! # Symlink protection
//!
//! Every call validates the path with non-following `symlink_metadata` to
//! detect symlink replacements.  If the directory was replaced by a symlink,
//! the symlink is removed and a real directory is created in its place.
//! Ownership is validated against the current uid, and `0o700` permissions
//! are enforced on every access.
//!
//! # Fail-closed
//!
//! Returns an error when the secure directory cannot be created or validated.
//! Callers must fail closed (do not fall back to an unvalidated shared path)
//! so that tool artifacts are never written to an attacker-controlled location.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Return a secure per-user temporary directory, creating it on first call
/// and caching the path for the lifetime of the process.
///
/// On every call (including cache hits) the directory is re-validated for
/// ownership, permissions, and symlink integrity so that a temp cleaner or
/// attacker cannot weaken security between calls.
pub(super) fn secure_temp_dir() -> std::io::Result<&'static Path> {
    static DIR: OnceLock<PathBuf> = OnceLock::new();

    // Fast path: already initialised this session.
    if let Some(dir) = DIR.get() {
        // Revalidate: a temp cleaner or attacker may have removed, replaced,
        // or changed the directory since the last call.  Using only `exists()`
        // is insufficient because a symlink replacement would pass the check.
        // Full validation (non-following metadata, ownership, permissions)
        // ensures the path is still a secure directory before reuse.
        ensure_secure_temp_dir(dir)?;
        return Ok(dir.as_path());
    }

    // Create the secure temp directory (may fail with io::Error).
    let dir = create_secure_temp_dir()?;

    // Cache it.  If another thread beat us, theirs is fine too — the
    // extra directory is unused and will be cleaned up naturally.
    Ok(DIR.get_or_init(|| dir).as_path())
}

/// Create a secure temp directory.
///
/// On Unix, generates an unguessable path `<temp_dir>/cake-<uid>-<random>`
/// and creates it with `0o700` permissions. On non-Unix, creates
/// `<temp_dir>/cake`.
#[cfg(unix)]
fn create_secure_temp_dir() -> std::io::Result<PathBuf> {
    // Generate an unguessable directory name using 64 bits of randomness
    // from a UUID.  An attacker on a shared sticky-bit temp directory like
    // `/tmp` cannot pre-create the full path because they cannot predict
    // the random suffix.
    let random_part = uuid::Uuid::new_v4().as_u128();
    let suffix = format!("{:016x}", (random_part >> 64) as u64);

    // SAFETY: `getuid()` is a simple system call with no safety requirements.
    let uid = unsafe { libc::getuid() };
    let dir = std::env::temp_dir().join(format!("cake-{uid}-{suffix}"));

    std::fs::create_dir_all(&dir)?;

    // Delegate to the shared validator which enforces ownership and mode.
    ensure_secure_temp_dir(&dir)?;

    Ok(dir)
}

/// Validate or recreate the cached secure temp directory before reuse.
///
/// Uses non-following `symlink_metadata` to detect symlink replacements:
/// an attacker who replaces the directory with a symlink to another location
/// cannot bypass validation because the check operates on the link itself,
/// not its target.  Validates ownership (must match the current user) and
/// enforces `0o700` permissions on every call.
///
/// On non-Unix platforms this simply re-creates the directory if missing.
#[cfg(unix)]
fn ensure_secure_temp_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    // SAFETY: `getuid()` is a simple system call with no safety requirements.
    let uid = unsafe { libc::getuid() };

    match std::fs::symlink_metadata(dir) {
        Ok(md) if md.file_type().is_symlink() => {
            // Attacker replaced our directory with a symlink — remove it
            // and create a real directory in its place.
            std::fs::remove_file(dir)?;
            std::fs::create_dir_all(dir)?;
        },
        Ok(md) if md.is_dir() => {
            // Real directory exists — validate ownership.
            if md.uid() != uid {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "directory '{}' is owned by uid {} but expected {uid}",
                        dir.display(),
                        md.uid()
                    ),
                ));
            }
            // Fix permissions if they drifted.
            let mut perms = md.permissions();
            if perms.mode() & 0o777 != 0o700 {
                perms.set_mode(0o700);
                std::fs::set_permissions(dir, perms)?;
            }
            return Ok(());
        },
        Ok(_) => {
            // Exists but is neither a symlink nor a directory.
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("'{}' exists but is not a directory", dir.display()),
            ));
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Directory vanished — recreate it.
            std::fs::create_dir_all(dir)?;
        },
        Err(e) => return Err(e),
    }

    // Set permissions on a freshly created path (symlink was removed or
    // directory was recreated from scratch).
    let md = std::fs::symlink_metadata(dir)?;
    let mut perms = md.permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(dir, perms)?;

    Ok(())
}

#[cfg(not(unix))]
fn ensure_secure_temp_dir(dir: &Path) -> std::io::Result<()> {
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn create_secure_temp_dir() -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join("cake");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn validator_rejects_non_directory() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("occupied");
        std::fs::write(&path, b"not a directory").unwrap();

        let error = ensure_secure_temp_dir(&path).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(error.to_string().contains("exists but is not a directory"));
    }

    #[test]
    fn validator_repairs_directory_permissions() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("insecure-mode");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();

        ensure_secure_temp_dir(&path).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn validator_recreates_missing_directory() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("missing");

        ensure_secure_temp_dir(&path).unwrap();

        assert!(path.is_dir());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn validator_replaces_symlink_with_directory() {
        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join("target");
        let path = parent.path().join("link");
        std::fs::create_dir(&target).unwrap();
        symlink(&target, &path).unwrap();

        ensure_secure_temp_dir(&path).unwrap();

        let metadata = std::fs::symlink_metadata(&path).unwrap();
        assert!(metadata.is_dir());
        assert!(!metadata.file_type().is_symlink());
        assert!(target.is_dir());
    }
}
