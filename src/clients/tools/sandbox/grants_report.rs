//! Crate-visible snapshot of the effective filesystem grants.
//!
//! `cake debug grants` reports the same grant value a run builds, plus the
//! source lists and filesystem statuses a user needs to explain a path
//! decision. The snapshot is deliberately separate from [`PathGrants`]: the
//! grant value deduplicates and canonicalizes its inputs, so it cannot
//! reverse-map an effective path to the setting that granted it. The report
//! keeps the source lists and the effective sets side by side instead.
//!
//! The report never creates a directory. A configured writable path that is
//! missing at report time is marked `would-create` and excluded from the
//! effective sets; a writable run would provision it later. See ADR 040.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::clients::tools::sandbox::{
    DenyKind, PathGrants, SandboxConfig, SandboxPolicy, SandboxPolicySource,
    deduplicated_with_canonical, resolve_linked_worktree_dirs,
};

/// Which enforcement projection a source list belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantAccess {
    /// Read-write for the in-process tools.
    ReadWrite,
    /// Read-only for the in-process tools.
    ReadOnly,
    /// Credential-bearing path that wins over every ordinary grant.
    Deny,
}

impl GrantAccess {
    /// The stable label used in diagnostic output.
    pub const fn label(self) -> &'static str {
        match self {
            Self::ReadWrite => "read-write",
            Self::ReadOnly => "read-only",
            Self::Deny => "deny",
        }
    }
}

/// Filesystem status of one source path at report time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantPathStatus {
    /// The path exists and is included in the effective sets.
    Granted,
    /// The path is missing and excluded from the effective sets.
    Missing,
    /// The path is missing; a writable run would create and grant it.
    WouldCreate,
    /// The configured path exists but is not a directory.
    NotADirectory,
    /// The path could not be inspected and is excluded.
    Excluded,
    /// The path is a credential deny entry, not an ordinary grant.
    Denied,
}

impl GrantPathStatus {
    /// The stable label used in diagnostic output.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Missing => "missing",
            Self::WouldCreate => "would-create",
            Self::NotADirectory => "not-a-directory",
            Self::Excluded => "excluded",
            Self::Denied => "denied",
        }
    }
}

/// One path in a grant source list.
#[derive(Clone, Debug)]
pub struct GrantSourceEntry {
    pub path: PathBuf,
    pub status: GrantPathStatus,
    /// A detail shown beside the status. Source-path problems also become
    /// warnings; a credential deny entry uses it to name the deny class.
    pub detail: Option<String>,
}

/// One named list of configured or built-in grant sources.
#[derive(Clone, Debug)]
pub struct GrantSourceList {
    /// Stable machine-readable source id.
    pub id: &'static str,
    /// Human-readable source label.
    pub label: &'static str,
    pub access: GrantAccess,
    pub entries: Vec<GrantSourceEntry>,
    /// Source-level warnings that are not attached to one path (for example an
    /// empty settings value).
    pub notices: Vec<String>,
}

/// Whether a credential deny entry names a file or a directory subtree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantDenyKind {
    File,
    Directory,
}

impl GrantDenyKind {
    /// The stable label used in diagnostic output.
    pub const fn label(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
        }
    }
}

/// One path that wins over every ordinary grant.
#[derive(Clone, Debug)]
pub struct GrantDenial {
    pub path: PathBuf,
    pub kind: GrantDenyKind,
}

/// Inputs for a read-only grant report.
///
/// The caller resolves settings, skill discovery, the data directory, and the
/// global config directory with the same rules a run uses. The report resolves
/// the grant inputs that can be resolved without provisioning.
#[derive(Clone, Debug)]
pub struct GrantReportInputs {
    pub policy: SandboxPolicy,
    pub policy_source: SandboxPolicySource,
    pub cwd: PathBuf,
    /// The same temporary directories a run passes to `PathGrants`.
    pub temp_dirs: Vec<PathBuf>,
    /// Resolved `directories` settings values (home-expanded by the loader).
    pub directories: Vec<String>,
    /// Resolved `[sandbox].writable` settings values.
    pub writable: Vec<String>,
    /// Resolved `[sandbox].read_only` settings values.
    pub read_only: Vec<String>,
    /// `--add-dir` values, resolved against `cwd` like a run.
    pub add_dir: Vec<String>,
    /// Base directories of the selected skill catalog.
    pub skill_dirs: Vec<PathBuf>,
    /// `~/.agents/skills`, when a home directory is available.
    pub user_skill_root: Option<PathBuf>,
    /// Cake state roots granted read-only by every run.
    pub data_read_dirs: Vec<PathBuf>,
    /// `<config_dir>/cake`, granted read-only by every run.
    pub global_config_dir: PathBuf,
    /// The resolved account home, or `None` when it is unavailable.
    pub home: Option<PathBuf>,
}

/// The effective grant value and the source lists behind it.
#[derive(Clone, Debug)]
pub struct GrantReport {
    pub policy: SandboxPolicy,
    pub policy_source: SandboxPolicySource,
    pub sources: Vec<GrantSourceList>,
    pub read_write: Vec<PathBuf>,
    pub read_only: Vec<PathBuf>,
    pub sandbox_writable: Vec<PathBuf>,
    pub sandbox_only_read: Vec<PathBuf>,
    pub denied: Vec<GrantDenial>,
    pub user_grants: Vec<PathBuf>,
    /// The OS-sandbox writable projection: `read_write + sandbox_writable`.
    pub bash_writable: Vec<PathBuf>,
    /// The OS-sandbox read-execute projection:
    /// `(read_only - sandbox_writable) + sandbox_only_read`.
    pub bash_read_execute: Vec<PathBuf>,
}

impl GrantReport {
    /// Resolve the source lists and build the effective grant value once.
    pub fn build(inputs: &GrantReportInputs) -> Self {
        let linked_worktree_dirs = resolve_linked_worktree_dirs(&inputs.cwd);
        let resolved = ResolvedSources::build(inputs, &linked_worktree_dirs);
        let grants = PathGrants::build_with_linked_dirs(
            inputs.policy,
            &inputs.cwd,
            &inputs.temp_dirs,
            &linked_worktree_dirs,
            &resolved.additional_dirs,
            &resolved.settings_dirs,
            &resolved.skill_dirs,
            inputs.home.as_deref(),
        );
        let denied: Vec<GrantDenial> = grants
            .denied
            .iter()
            .map(|entry| GrantDenial {
                path: entry.path.clone(),
                kind: match entry.kind {
                    DenyKind::File => GrantDenyKind::File,
                    DenyKind::Directory => GrantDenyKind::Directory,
                },
            })
            .collect();
        let projections = SandboxConfig::from_grants(&grants);

        let mut sources = resolved.sources;
        sources.push(denied_source(&denied));

        Self {
            policy: grants.policy,
            policy_source: inputs.policy_source,
            sources,
            read_write: grants.read_write,
            read_only: grants.read_only,
            sandbox_writable: grants.sandbox_writable,
            sandbox_only_read: grants.sandbox_only_read,
            denied,
            user_grants: grants.user_grants,
            bash_writable: projections.writable,
            bash_read_execute: projections.read_execute,
        }
    }

    /// Warnings from configured source paths and source-level notices.
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        for source in &self.sources {
            warnings.extend(source.notices.iter().cloned());
            warnings.extend(
                source
                    .entries
                    .iter()
                    .filter(|entry| {
                        matches!(
                            entry.status,
                            GrantPathStatus::Missing
                                | GrantPathStatus::WouldCreate
                                | GrantPathStatus::NotADirectory
                                | GrantPathStatus::Excluded
                        )
                    })
                    .filter_map(|entry| entry.detail.clone()),
            );
        }
        warnings
    }
}

/// The source lists plus the grant inputs derived from them.
struct ResolvedSources {
    sources: Vec<GrantSourceList>,
    settings_dirs: Vec<PathBuf>,
    additional_dirs: Vec<PathBuf>,
    skill_dirs: Vec<PathBuf>,
}

impl ResolvedSources {
    fn build(inputs: &GrantReportInputs, linked_worktree_dirs: &[PathBuf]) -> Self {
        let mut sources = Vec::new();

        sources.push(path_source(
            "working_directory",
            "Working directory",
            GrantAccess::ReadWrite,
            std::slice::from_ref(&inputs.cwd),
        ));
        sources.push(path_source(
            "temp_directories",
            "Temp directories",
            GrantAccess::ReadWrite,
            &inputs.temp_dirs,
        ));
        sources.push(path_source(
            "linked_worktree_git_directories",
            "Linked-worktree Git directories",
            GrantAccess::ReadWrite,
            linked_worktree_dirs,
        ));

        let (directories, directories_source) = resolve_settings_dirs(
            "directories",
            "Configured directories (directories)",
            "settings.toml directories",
            &inputs.directories,
            inputs.policy,
        );
        sources.push(directories_source);

        let (writable, writable_source) = resolve_settings_dirs(
            "sandbox_writable",
            "Configured writable directories ([sandbox].writable)",
            "settings.toml [sandbox].writable",
            &inputs.writable,
            inputs.policy,
        );
        sources.push(writable_source);

        let (user_skill_root, user_skill_source) =
            resolve_user_skill_root(inputs.user_skill_root.as_deref(), inputs.policy);
        sources.push(user_skill_source);

        let (add_dir, add_dir_source) = resolve_add_dir(&inputs.add_dir, &inputs.cwd);
        sources.push(add_dir_source);

        let (read_only, read_only_source) = resolve_read_only(&inputs.read_only);
        sources.push(read_only_source);

        let (skill_dirs, skill_source) = resolve_skill_dirs(&inputs.skill_dirs);
        sources.push(skill_source);

        sources.push(path_source(
            "cake_state_roots",
            "Cake state roots",
            GrantAccess::ReadOnly,
            &inputs.data_read_dirs,
        ));
        sources.push(path_source(
            "global_config_directory",
            "Global config directory",
            GrantAccess::ReadOnly,
            std::slice::from_ref(&inputs.global_config_dir),
        ));

        let (system_read, sandbox_only_read) =
            PathGrants::system_read_paths(inputs.home.as_deref());
        sources.push(path_source(
            "builtin_system_read_paths",
            "Built-in system read paths",
            GrantAccess::ReadOnly,
            &system_read,
        ));

        let toolchain = inputs.home.as_deref().map_or_else(Vec::new, |home| {
            deduplicated_with_canonical(&PathGrants::toolchain_paths(home))
        });
        sources.push(path_source(
            "builtin_toolchain_paths",
            "Built-in toolchain paths",
            GrantAccess::ReadOnly,
            &toolchain,
        ));
        sources.push(path_source(
            "linux_os_only_read_paths",
            "Linux OS-only read paths",
            GrantAccess::ReadOnly,
            &sandbox_only_read,
        ));

        let mut settings_dirs = directories;
        settings_dirs.extend(writable);
        if let Some(root) = user_skill_root {
            settings_dirs.push(root);
        }

        let mut additional_dirs = add_dir;
        additional_dirs.extend(read_only);
        additional_dirs.extend(inputs.data_read_dirs.iter().cloned());
        additional_dirs.push(inputs.global_config_dir.clone());

        Self {
            sources,
            settings_dirs,
            additional_dirs,
            skill_dirs,
        }
    }
}

/// Resolve a configured directory list. Missing paths are never created.
fn resolve_settings_dirs(
    id: &'static str,
    label: &'static str,
    message_prefix: &'static str,
    paths: &[String],
    policy: SandboxPolicy,
) -> (Vec<PathBuf>, GrantSourceList) {
    let mut entries = Vec::new();
    let mut granted = Vec::new();
    let mut notices = Vec::new();

    for configured in paths {
        if configured.is_empty() {
            notices.push(format!("{message_prefix} contains an empty path; ignored"));
            continue;
        }
        let path = PathBuf::from(configured);
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_dir() => {
                granted.push(path.clone());
                entries.push(entry(path, GrantPathStatus::Granted, None));
            },
            Ok(_) => {
                let detail = format!(
                    "{message_prefix} path '{}' is not a directory; ignored",
                    path.display()
                );
                entries.push(entry(path, GrantPathStatus::NotADirectory, Some(detail)));
            },
            Err(error) if error.kind() == ErrorKind::NotFound => {
                if policy == SandboxPolicy::ReadOnly {
                    let detail = format!(
                        "{message_prefix} path '{}' is missing; excluded under the read-only policy",
                        path.display()
                    );
                    entries.push(entry(path, GrantPathStatus::Missing, Some(detail)));
                } else {
                    let detail = format!(
                        "{message_prefix} path '{}' is missing; a writable run would create it and grant it",
                        path.display()
                    );
                    entries.push(entry(path, GrantPathStatus::WouldCreate, Some(detail)));
                }
            },
            Err(error) => {
                let detail = format!(
                    "failed to inspect {message_prefix} path '{}': {error}; excluded",
                    path.display()
                );
                entries.push(entry(path, GrantPathStatus::Excluded, Some(detail)));
            },
        }
    }

    (
        granted,
        GrantSourceList {
            id,
            label,
            access: GrantAccess::ReadWrite,
            entries,
            notices,
        },
    )
}

/// Resolve `[sandbox].read_only`, which may name files or directories.
fn resolve_read_only(paths: &[String]) -> (Vec<PathBuf>, GrantSourceList) {
    let mut entries = Vec::new();
    let mut granted = Vec::new();
    let mut notices = Vec::new();

    for configured in paths {
        if configured.is_empty() {
            notices.push(
                "settings.toml [sandbox].read_only contains an empty path; ignored".to_string(),
            );
            continue;
        }
        let path = PathBuf::from(configured);
        match std::fs::metadata(&path) {
            Ok(_) => {
                granted.push(path.clone());
                entries.push(entry(path, GrantPathStatus::Granted, None));
            },
            Err(error) if error.kind() == ErrorKind::NotFound => {
                let detail = format!(
                    "settings.toml [sandbox].read_only path '{}' is missing; ignored",
                    path.display()
                );
                entries.push(entry(path, GrantPathStatus::Missing, Some(detail)));
            },
            Err(error) => {
                let detail = format!(
                    "failed to inspect settings.toml [sandbox].read_only path '{}': {error}; excluded",
                    path.display()
                );
                entries.push(entry(path, GrantPathStatus::Excluded, Some(detail)));
            },
        }
    }

    (
        granted,
        GrantSourceList {
            id: "sandbox_read_only",
            label: "Configured read-only paths ([sandbox].read_only)",
            access: GrantAccess::ReadOnly,
            entries,
            notices,
        },
    )
}

/// Resolve `--add-dir` values against the invocation directory.
fn resolve_add_dir(paths: &[String], cwd: &Path) -> (Vec<PathBuf>, GrantSourceList) {
    let mut entries = Vec::new();
    let mut granted = Vec::new();
    let mut notices = Vec::new();

    for configured in paths {
        if configured.is_empty() {
            notices.push("--add-dir contains an empty path; ignored".to_string());
            continue;
        }
        let configured_path = PathBuf::from(configured);
        let path = if configured_path.is_absolute() {
            configured_path
        } else {
            cwd.join(configured_path)
        };
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_dir() => {
                let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                granted.push(canonical.clone());
                entries.push(entry(canonical, GrantPathStatus::Granted, None));
            },
            Ok(_) => {
                let detail = format!(
                    "--add-dir path '{}' is not a directory; ignored",
                    path.display()
                );
                entries.push(entry(path, GrantPathStatus::NotADirectory, Some(detail)));
            },
            Err(error) if error.kind() == ErrorKind::NotFound => {
                let detail = format!(
                    "--add-dir path '{}' does not exist; ignored",
                    path.display()
                );
                entries.push(entry(path, GrantPathStatus::Missing, Some(detail)));
            },
            Err(error) => {
                let detail = format!(
                    "failed to inspect --add-dir path '{}': {error}; excluded",
                    path.display()
                );
                entries.push(entry(path, GrantPathStatus::Excluded, Some(detail)));
            },
        }
    }

    (
        granted,
        GrantSourceList {
            id: "add_dir",
            label: "--add-dir",
            access: GrantAccess::ReadOnly,
            entries,
            notices,
        },
    )
}

/// Resolve the user skill root without creating it.
fn resolve_user_skill_root(
    root: Option<&Path>,
    policy: SandboxPolicy,
) -> (Option<PathBuf>, GrantSourceList) {
    let mut entries = Vec::new();
    let notices = Vec::new();
    let granted = root.and_then(|path| match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            entries.push(entry(path.to_path_buf(), GrantPathStatus::Granted, None));
            Some(path.to_path_buf())
        },
        Ok(_) => {
            let detail = format!(
                "user skill root '{}' is not a directory; ignored",
                path.display()
            );
            entries.push(entry(
                path.to_path_buf(),
                GrantPathStatus::NotADirectory,
                Some(detail),
            ));
            None
        },
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let status = if policy == SandboxPolicy::ReadOnly {
                GrantPathStatus::Missing
            } else {
                GrantPathStatus::WouldCreate
            };
            entries.push(entry(path.to_path_buf(), status, None));
            None
        },
        Err(error) => {
            let detail = format!(
                "failed to inspect user skill root '{}': {error}; excluded",
                path.display()
            );
            entries.push(entry(
                path.to_path_buf(),
                GrantPathStatus::Excluded,
                Some(detail),
            ));
            None
        },
    });

    (
        granted,
        GrantSourceList {
            id: "user_skill_root",
            label: "User skill root",
            access: GrantAccess::ReadWrite,
            entries,
            notices,
        },
    )
}

/// Deduplicate and sort skill base directories so repeated runs match.
fn resolve_skill_dirs(paths: &[PathBuf]) -> (Vec<PathBuf>, GrantSourceList) {
    let mut skill_dirs: Vec<PathBuf> = paths.to_vec();
    skill_dirs.sort();
    skill_dirs.dedup();
    let mut entries = Vec::with_capacity(skill_dirs.len());
    for path in &skill_dirs {
        let status = if path.is_dir() {
            GrantPathStatus::Granted
        } else {
            GrantPathStatus::Missing
        };
        entries.push(entry(path.clone(), status, None));
    }
    (
        skill_dirs,
        GrantSourceList {
            id: "skill_directories",
            label: "Skill directories",
            access: GrantAccess::ReadOnly,
            entries,
            notices: Vec::new(),
        },
    )
}

/// Build a source list for built-in paths that have no provisioning rules.
fn path_source(
    id: &'static str,
    label: &'static str,
    access: GrantAccess,
    paths: &[PathBuf],
) -> GrantSourceList {
    let entries = paths
        .iter()
        .map(|path| {
            let status = if path.exists() {
                GrantPathStatus::Granted
            } else {
                GrantPathStatus::Missing
            };
            entry(path.clone(), status, None)
        })
        .collect();
    GrantSourceList {
        id,
        label,
        access,
        entries,
        notices: Vec::new(),
    }
}

/// Build the credential deny source list from the effective grant value.
fn denied_source(denied: &[GrantDenial]) -> GrantSourceList {
    let entries = denied
        .iter()
        .map(|denial| {
            entry(
                denial.path.clone(),
                GrantPathStatus::Denied,
                Some(format!("credential deny entry ({})", denial.kind.label())),
            )
        })
        .collect();
    GrantSourceList {
        id: "credential_deny_list",
        label: "Credential deny list",
        access: GrantAccess::Deny,
        entries,
        notices: Vec::new(),
    }
}

const fn entry(path: PathBuf, status: GrantPathStatus, detail: Option<String>) -> GrantSourceEntry {
    GrantSourceEntry {
        path,
        status,
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn report_inputs(policy: SandboxPolicy, home: &Path, cwd: &Path) -> GrantReportInputs {
        GrantReportInputs {
            policy,
            policy_source: SandboxPolicySource::Default,
            cwd: cwd.to_path_buf(),
            temp_dirs: Vec::new(),
            directories: Vec::new(),
            writable: Vec::new(),
            read_only: Vec::new(),
            add_dir: Vec::new(),
            skill_dirs: Vec::new(),
            user_skill_root: None,
            data_read_dirs: Vec::new(),
            global_config_dir: home.join("config/cake"),
            home: Some(home.to_path_buf()),
        }
    }

    fn create_dir(parent: &Path, name: &str) -> PathBuf {
        let path = parent.join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn source<'a>(report: &'a GrantReport, id: &str) -> &'a GrantSourceList {
        report
            .sources
            .iter()
            .find(|source| source.id == id)
            .expect("source id should exist")
    }

    #[test]
    fn read_only_report_demotes_the_workspace_and_toolchain() {
        let root = TempDir::new().unwrap();
        let home = create_dir(root.path(), "home");
        let cwd = create_dir(root.path(), "workspace");
        let temp = create_dir(root.path(), "tmp");

        let mut inputs = report_inputs(SandboxPolicy::ReadOnly, &home, &cwd);
        inputs.temp_dirs = vec![temp.clone()];
        let report = GrantReport::build(&inputs);

        assert!(
            report.read_write.iter().any(|path| path == &temp),
            "temp directories stay writable under read-only"
        );
        assert!(
            !report.read_write.iter().any(|path| path == &cwd),
            "the workspace is demoted under read-only"
        );
        assert!(report.read_only.iter().any(|path| path == &cwd));
        assert!(
            report
                .read_only
                .iter()
                .any(|path| path == &home.join(".npm")),
            "toolchain paths stay readable in-process"
        );
        assert!(
            report.sandbox_writable.is_empty(),
            "Bash loses toolchain writes under read-only"
        );
        assert!(
            report.bash_writable.iter().any(|path| path == &temp),
            "the Bash projection keeps temp writable"
        );
        assert!(
            !report.bash_writable.iter().any(|path| path == &cwd),
            "the Bash projection demotes the workspace"
        );
    }

    #[test]
    fn report_matches_the_grant_builder_for_the_same_inputs() {
        let root = TempDir::new().unwrap();
        let home = create_dir(root.path(), "home");
        let cwd = create_dir(root.path(), "workspace");
        let temp = create_dir(root.path(), "tmp");
        let settings = create_dir(root.path(), "settings");
        let add = create_dir(root.path(), "add");
        let skill = create_dir(root.path(), "skill");
        let user_skills = create_dir(root.path(), "user-skills");
        let state = create_dir(root.path(), "state");
        let config = create_dir(root.path(), "config/cake");

        let mut inputs = report_inputs(SandboxPolicy::WorkspaceWrite, &home, &cwd);
        inputs.temp_dirs = vec![temp.clone()];
        inputs.directories = vec![settings.display().to_string()];
        inputs.add_dir = vec![add.display().to_string()];
        inputs.skill_dirs = vec![skill.clone()];
        inputs.user_skill_root = Some(user_skills.clone());
        inputs.data_read_dirs = vec![state.clone()];
        inputs.global_config_dir = config.clone();
        let report = GrantReport::build(&inputs);

        let canonical_add = add.canonicalize().unwrap();
        let expected = PathGrants::build(
            SandboxPolicy::WorkspaceWrite,
            &cwd,
            &[temp],
            &[canonical_add, state, config],
            &[settings, user_skills],
            &[skill],
            Some(&home),
        );

        assert_eq!(report.read_write, expected.read_write);
        assert_eq!(report.read_only, expected.read_only);
        assert_eq!(report.sandbox_writable, expected.sandbox_writable);
        assert_eq!(report.sandbox_only_read, expected.sandbox_only_read);
        assert_eq!(report.user_grants, expected.user_grants);
        assert_eq!(report.denied.len(), expected.denied.len());
        for (reported, built) in report.denied.iter().zip(&expected.denied) {
            assert_eq!(reported.path, built.path);
            assert_eq!(
                reported.kind.label(),
                match built.kind {
                    DenyKind::File => "file",
                    DenyKind::Directory => "directory",
                }
            );
        }

        let projections = SandboxConfig::from_grants(&expected);
        assert_eq!(report.bash_writable, projections.writable);
        assert_eq!(report.bash_read_execute, projections.read_execute);
        assert_eq!(report.policy, SandboxPolicy::WorkspaceWrite);
        assert_eq!(report.policy_source, SandboxPolicySource::Default);
    }

    #[test]
    fn missing_writable_path_is_would_create_and_never_created() {
        let root = TempDir::new().unwrap();
        let home = create_dir(root.path(), "home");
        let cwd = create_dir(root.path(), "workspace");
        let missing = root.path().join("missing/parent/cache");

        let mut inputs = report_inputs(SandboxPolicy::WorkspaceWrite, &home, &cwd);
        inputs.writable = vec![missing.display().to_string()];
        let report = GrantReport::build(&inputs);
        let writable = source(&report, "sandbox_writable");
        assert_eq!(writable.entries[0].status, GrantPathStatus::WouldCreate);
        assert!(
            report
                .warnings()
                .iter()
                .any(|warning| warning.contains("would create it and grant it"))
        );
        assert!(!missing.exists(), "the report must not create a grant");
        assert!(
            !report.read_write.iter().any(|path| path == &missing),
            "a missing path must not become a grant"
        );

        let mut read_only = report_inputs(SandboxPolicy::ReadOnly, &home, &cwd);
        read_only.writable = vec![missing.display().to_string()];
        let report = GrantReport::build(&read_only);
        let writable = source(&report, "sandbox_writable");
        assert_eq!(writable.entries[0].status, GrantPathStatus::Missing);
        assert!(
            report
                .warnings()
                .iter()
                .any(|warning| warning.contains("excluded under the read-only policy"))
        );
        assert!(!missing.exists(), "read-only must not create a grant");
    }

    #[test]
    fn source_lists_and_skill_grants_are_deterministic() {
        let root = TempDir::new().unwrap();
        let home = create_dir(root.path(), "home");
        let cwd = create_dir(root.path(), "workspace");
        let alpha = create_dir(root.path(), "alpha");
        let zeta = create_dir(root.path(), "zeta");

        let mut first = report_inputs(SandboxPolicy::WorkspaceWrite, &home, &cwd);
        first.skill_dirs = vec![zeta.clone(), alpha.clone()];
        let first = GrantReport::build(&first);

        let mut second = report_inputs(SandboxPolicy::WorkspaceWrite, &home, &cwd);
        second.skill_dirs = vec![alpha.clone(), zeta.clone()];
        let second = GrantReport::build(&second);

        let ids: Vec<&str> = first.sources.iter().map(|source| source.id).collect();
        assert_eq!(
            ids,
            vec![
                "working_directory",
                "temp_directories",
                "linked_worktree_git_directories",
                "directories",
                "sandbox_writable",
                "user_skill_root",
                "add_dir",
                "sandbox_read_only",
                "skill_directories",
                "cake_state_roots",
                "global_config_directory",
                "builtin_system_read_paths",
                "builtin_toolchain_paths",
                "linux_os_only_read_paths",
                "credential_deny_list",
            ]
        );
        assert_eq!(
            source(&first, "skill_directories")
                .entries
                .iter()
                .map(|entry| &entry.path)
                .collect::<Vec<_>>(),
            vec![&alpha, &zeta]
        );
        assert_eq!(first.read_only, second.read_only);
        assert_eq!(first.user_grants, second.user_grants);
    }

    #[test]
    fn denied_entries_are_reported_and_win_over_a_granted_home() {
        let root = TempDir::new().unwrap();
        let home = create_dir(root.path(), "home");
        let cwd = create_dir(root.path(), "workspace");
        let ssh = create_dir(&home, ".ssh");
        std::fs::write(home.join(".npmrc"), "token = ignored").unwrap();

        let mut inputs = report_inputs(SandboxPolicy::WorkspaceWrite, &home, &cwd);
        inputs.directories = vec![home.display().to_string()];
        let report = GrantReport::build(&inputs);

        assert!(report.read_write.iter().any(|path| path == &home));
        assert!(
            report
                .denied
                .iter()
                .any(|entry| entry.path == ssh && entry.kind == GrantDenyKind::Directory)
        );
        assert!(
            report
                .denied
                .iter()
                .any(|entry| entry.path == home.join(".npmrc")
                    && entry.kind == GrantDenyKind::File)
        );

        let grants = PathGrants::build(
            SandboxPolicy::WorkspaceWrite,
            &cwd,
            &[],
            &[],
            std::slice::from_ref(&home),
            &[],
            Some(&home),
        );
        assert!(
            grants.is_denied(&ssh.join("id_ed25519")),
            "the deny list must win over the granted home"
        );
        assert!(
            grants.is_denied(&home.join(".npmrc")),
            "the file deny entry must win over the granted home"
        );
        assert!(
            !grants.is_denied(&home.join(".npmrc.backup")),
            "a file deny entry must not deny a sibling"
        );
    }
}
