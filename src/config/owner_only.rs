//! Owner-only permissions for Cake's session storage.
//!
//! Session transcripts can contain prompts, tool output, and paths, so Cake
//! creates its data and sessions directories `0700` and session files `0600`.
//! Each mode is applied explicitly instead of inheriting the process umask.
//! Path checks still apply on every platform; mode enforcement is Unix-only.

use std::{
    fs::{self, File, OpenOptions},
    io,
    path::Path,
};

/// Owner-only mode for Cake's data and sessions directories.
pub const OWNER_ONLY_DIR_MODE: u32 = 0o700;

/// Owner-only mode for session files.
pub const OWNER_ONLY_FILE_MODE: u32 = 0o600;

/// Create `path` and its missing parents, leaving the final directory `0700`.
///
/// The explicit set-permissions call makes the final mode independent of the
/// process umask. A umask can only remove bits, so the required owner bits are
/// restored after creation.
pub fn create_dir_all_owner_only(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;

        fs::DirBuilder::new()
            .recursive(true)
            .mode(OWNER_ONLY_DIR_MODE)
            .create(path)?;
        set_dir_owner_only(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}

/// Set an owned directory to `0700`, including one just created under an
/// unusual umask.
pub fn set_dir_owner_only(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(path, fs::Permissions::from_mode(OWNER_ONLY_DIR_MODE))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Remove group and other access from an existing directory.
///
/// A narrower mode is left alone rather than broadened: the store must never
/// grant access it did not already have, and a deliberately unreadable sessions
/// directory must stay unreadable so listing can report it.
pub fn tighten_dir_owner_only(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        if fs::metadata(path)?.permissions().mode().trailing_zeros() >= 6 {
            return Ok(());
        }
        set_dir_owner_only(path)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Tighten an open session file to `0600` before it is written.
pub fn set_file_owner_only(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        if file.metadata()?.permissions().mode() & 0o777 == OWNER_ONLY_FILE_MODE {
            return Ok(());
        }
        file.set_permissions(fs::Permissions::from_mode(OWNER_ONLY_FILE_MODE))
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Ok(())
    }
}

/// Apply the session-file creation mode and refuse to follow a final symlink.
///
/// `O_NOFOLLOW` closes the gap between the path check and the open on Unix;
/// platforms without it still rely on the explicit symlink check.
pub fn apply_session_options(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options
            .mode(OWNER_ONLY_FILE_MODE)
            .custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(not(unix))]
    {
        let _ = options;
    }
}
