//! Prepare configured writable grants for an agent run, after policy resolution.

use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use crate::clients::SandboxPolicy;

/// A configured directory cannot be prepared before tools start.
#[derive(Debug, thiserror::Error)]
pub enum SandboxGrantError {
    #[error("Failed to create settings.toml {setting} directory '{path}': {source}")]
    Create {
        setting: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("Failed to inspect settings.toml {setting} directory '{path}': {source}")]
    Inspect {
        setting: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Provision the built-in user skill root as a read-write default.
///
/// The root is `~/.agents/skills`, resolved by [`crate::config::user_skills_dir`].
/// Writable policies create a missing root and its parents before tools start;
/// a failure logs a warning and omits the grant instead of aborting the run,
/// because this is a built-in default rather than user-configured authority.
/// The `read-only` policy returns an existing root so it can be demoted to
/// read-only, and never creates a missing one.
pub fn prepare_user_skills_dir(path: &Path, policy: SandboxPolicy) -> Option<PathBuf> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Some(path.to_path_buf()),
        Ok(_) => {
            tracing::warn!(
                "user skill root '{}' is not a directory, ignoring",
                path.display()
            );
            None
        },
        Err(_) if policy == SandboxPolicy::ReadOnly => None,
        Err(error) if error.kind() == ErrorKind::NotFound => match fs::create_dir_all(path) {
            Ok(()) => {
                tracing::warn!(
                    "user skill root '{}' did not exist; created it and any missing parents",
                    path.display()
                );
                Some(path.to_path_buf())
            },
            Err(error) => {
                tracing::warn!(
                    "failed to create user skill root '{}': {error}; continuing without the writable grant",
                    path.display()
                );
                None
            },
        },
        Err(error) => {
            tracing::warn!(
                "failed to inspect user skill root '{}': {error}; continuing without the writable grant",
                path.display()
            );
            None
        },
    }
}

/// Provision directory grants without granting their intermediate parents.
/// Settings loading and diagnostic commands deliberately do not call this.
pub fn prepare_writable_dirs(
    directories: &[String],
    writable: &[String],
    policy: SandboxPolicy,
) -> Result<Vec<PathBuf>, SandboxGrantError> {
    directories
        .iter()
        .map(|path| ("directories", path))
        .chain(writable.iter().map(|path| ("[sandbox].writable", path)))
        .filter_map(|(setting, path)| prepare_dir(setting, path, policy).transpose())
        .collect()
}

fn prepare_dir(
    setting: &'static str,
    configured: &str,
    policy: SandboxPolicy,
) -> Result<Option<PathBuf>, SandboxGrantError> {
    if configured.is_empty() {
        tracing::warn!("settings.toml {setting} contains an empty path, ignoring");
        return Ok(None);
    }
    let path = PathBuf::from(configured);
    match fs::metadata(&path) {
        Ok(metadata) if metadata.is_dir() => Ok(Some(path)),
        Ok(_) => {
            tracing::warn!(
                "settings.toml {setting} path '{}' is not a directory, ignoring",
                path.display()
            );
            Ok(None)
        },
        Err(_) if policy == SandboxPolicy::ReadOnly => {
            tracing::warn!(
                "settings.toml {setting} path '{}' does not exist or is not accessible, ignoring",
                path.display()
            );
            Ok(None)
        },
        Err(error) if error.kind() == ErrorKind::NotFound => {
            fs::create_dir_all(&path).map_err(|source| SandboxGrantError::Create {
                setting,
                path: path.clone(),
                source,
            })?;
            tracing::warn!(
                "settings.toml {setting} directory '{}' did not exist; created it and any missing parents",
                path.display()
            );
            Ok(Some(path))
        },
        Err(source) => Err(SandboxGrantError::Inspect {
            setting,
            path,
            source,
        }),
    }
}
