//! Prepare configured writable grants for an agent run, after policy resolution.

use std::{fs, io::ErrorKind, path::PathBuf};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_keys_bootstrap_missing_parents_and_preserve_existing_data() {
        for policy in [
            SandboxPolicy::WorkspaceWrite,
            SandboxPolicy::WorkspaceWriteInteractive,
            SandboxPolicy::DangerFullAccess,
        ] {
            let root = tempfile::tempdir().unwrap();
            let legacy = root.path().join("legacy/parent/state");
            let target = root.path().join("new/parent/cache");
            let directories = vec![legacy.to_str().unwrap().to_owned()];
            let writable = vec![target.to_str().unwrap().to_owned()];
            let grants = prepare_writable_dirs(&directories, &writable, policy).unwrap();
            assert_eq!(grants, vec![legacy.clone(), target.clone()]);
            fs::write(target.join("data"), "keep").unwrap();
            assert_eq!(
                prepare_writable_dirs(&directories, &writable, policy).unwrap(),
                grants
            );
            assert_eq!(fs::read_to_string(target.join("data")).unwrap(), "keep");
        }
    }

    #[test]
    fn read_only_keeps_existing_targets_without_creating_missing_ones() {
        let root = tempfile::tempdir().unwrap();
        let existing = root.path().join("existing");
        fs::create_dir(&existing).unwrap();
        let missing = root.path().join("missing/parent/state");
        let directories = vec![existing.to_str().unwrap().to_owned()];
        let writable = vec![missing.to_str().unwrap().to_owned()];
        assert_eq!(
            prepare_writable_dirs(&directories, &writable, SandboxPolicy::ReadOnly).unwrap(),
            vec![existing]
        );
        assert!(!root.path().join("missing").exists());
    }

    #[test]
    fn file_targets_are_ignored_and_blocked_parents_report_context() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("file");
        fs::write(&file, "keep").unwrap();
        let directories = vec![String::new(), file.to_str().unwrap().to_owned()];
        assert!(
            prepare_writable_dirs(&directories, &[], SandboxPolicy::WorkspaceWrite)
                .unwrap()
                .is_empty()
        );
        let blocked = file.join("state");
        let writable = vec![blocked.to_str().unwrap().to_owned()];
        let error =
            prepare_writable_dirs(&[], &writable, SandboxPolicy::WorkspaceWrite).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("[sandbox].writable"), "{message}");
        assert!(message.contains(blocked.to_str().unwrap()), "{message}");
        assert_eq!(fs::read_to_string(file).unwrap(), "keep");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_ancestors_work_and_dangling_targets_fail_creation() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let target = link.join("parent/state");
        let writable = vec![target.to_str().unwrap().to_owned()];
        let grants = prepare_writable_dirs(&[], &writable, SandboxPolicy::WorkspaceWrite).unwrap();
        assert_eq!(grants, vec![target]);
        assert!(real.join("parent/state").is_dir());

        let dangling = root.path().join("dangling");
        std::os::unix::fs::symlink(root.path().join("absent"), &dangling).unwrap();
        let writable = vec![dangling.to_str().unwrap().to_owned()];
        let error =
            prepare_writable_dirs(&[], &writable, SandboxPolicy::WorkspaceWrite).unwrap_err();
        assert!(error.to_string().contains("Failed to create"));
        assert!(!root.path().join("absent").exists());
    }
}
