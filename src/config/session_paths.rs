//! Path safety checks for session storage.
//!
//! Session transcripts can contain prompts, tool output, paths, and potentially
//! secrets, so a session path must not redirect writes outside the intended
//! data directory. These checks reject a symlinked or non-directory parent and
//! a symlinked or non-regular final component before create or append.

use std::{fs, path::Path};

use anyhow::Context;

use crate::config::owner_only;

/// Validate the parent of a session file before create or append.
///
/// Using creation may create a missing parent, and does so owner-only.
pub fn validate_session_parent(
    path: &Path,
    create_missing: bool,
    action: &str,
) -> anyhow::Result<()> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };

    match fs::symlink_metadata(parent) {
        Ok(metadata) if metadata.file_type().is_symlink() => anyhow::bail!(
            "Failed to {action} session file '{}': parent '{}' is a symlink; Cake refuses to follow it",
            path.display(),
            parent.display()
        ),
        Ok(metadata) if !metadata.is_dir() => anyhow::bail!(
            "Failed to {action} session file '{}': parent '{}' is not a directory",
            path.display(),
            parent.display()
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create_missing => {
            owner_only::create_dir_all_owner_only(parent).with_context(|| {
                format!(
                    "Failed to {action} session file '{}': cannot create session directory '{}' with owner-only mode {:04o}",
                    path.display(),
                    parent.display(),
                    owner_only::OWNER_ONLY_DIR_MODE
                )
            })
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(source).with_context(|| {
            format!(
                "Failed to {action} session file '{}': cannot inspect parent '{}'",
                path.display(),
                parent.display()
            )
        }),
    }
}

/// Reject an existing symlink or non-regular session path with a diagnostic.
pub fn validate_session_target(path: &Path, action: &str) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => anyhow::bail!(
            "Failed to {action} session file '{}': path is a symlink; Cake refuses to follow it (session files must be regular files with mode {:04o})",
            path.display(),
            owner_only::OWNER_ONLY_FILE_MODE
        ),
        Ok(metadata) if !metadata.is_file() => anyhow::bail!(
            "Failed to {action} session file '{}': path is not a regular file (required mode {:04o})",
            path.display(),
            owner_only::OWNER_ONLY_FILE_MODE
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(source)
            .with_context(|| format!("Failed to {action} session file '{}'", path.display())),
    }
}
