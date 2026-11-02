//! Explicit trust for authority-bearing project configuration.
//!
//! Project `.cake/` configuration can widen the sandbox, replace the command
//! gate, read arbitrary host files into the prompt, and execute hook and
//! toolbox commands outside the Bash sandbox. This module decides whether a
//! checkout is trusted before any of that authority is applied.
//!
//! A trust record is a versioned SHA-256 manifest of the project `.cake/` tree,
//! stored owner-only under `<config>/cake/trust/<project-key>.json`. The
//! decision is pinned to the canonical project root, so a second checkout or
//! worktree starts untrusted even when the content matches.
//!
//! The module reads and hashes project files; it never parses hook JSON,
//! resolves a credential, or starts a process.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::settings::{
    ProjectAuthority, SettingsError, SettingsLoader, SettingsOrigin, SettingsSource,
};

/// Version of the on-disk trust record format.
pub const TRUST_RECORD_VERSION: u32 = 1;

/// Environment variable that trusts the current project snapshot for one run.
pub const TRUST_PROJECT_ENV: &str = "CAKE_TRUST_PROJECT";

/// Environment variable that ignores project `.cake/` configuration for one run.
pub const PROJECT_CONFIG_ENV: &str = "CAKE_PROJECT_CONFIG";

/// Directory under the cake config directory that stores trust records.
const TRUST_DIR: &str = "trust";

/// The project configuration surface a command uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectConfigUse {
    /// Apply project authority, failing closed when it is untrusted.
    Enforce,
    /// Report project authority, applying it only when it is already trusted.
    Inspect,
    /// Do not read project configuration at all.
    Unused,
}

/// Effective project-configuration mode for one invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProjectConfigMode {
    /// Apply project authority only when a matching trust record exists.
    #[default]
    Enforce,
    /// Trust the current snapshot for this invocation only.
    EnforceOneRun,
    /// Report project authority; apply it only when trusted.
    Inspect,
    /// Trust the current snapshot for this inspection only.
    InspectOneRun,
    /// Ignore project `.cake/` configuration entirely.
    Ignore,
}

impl ProjectConfigMode {
    /// Whether the mode may apply authority and must fail closed when untrusted.
    #[must_use]
    pub const fn is_enforcing(self) -> bool {
        matches!(self, Self::Enforce | Self::EnforceOneRun)
    }

    /// Whether the mode is a read-only inspection.
    #[must_use]
    pub const fn is_inspecting(self) -> bool {
        matches!(self, Self::Inspect | Self::InspectOneRun)
    }

    /// Whether this mode trusts the current snapshot for one invocation.
    #[must_use]
    pub const fn one_run(self) -> bool {
        matches!(self, Self::EnforceOneRun | Self::InspectOneRun)
    }

    /// Whether project `.cake/` files may be read at all.
    #[must_use]
    pub const fn loads_project_config(self) -> bool {
        !matches!(self, Self::Ignore)
    }

    /// Resolve the mode for a command surface, reading the one-run environment.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectConfigModeError`] when two one-run modes are requested.
    pub fn resolve(
        surface: ProjectConfigUse,
        trust_project: bool,
        no_project_config: bool,
    ) -> Result<Self, ProjectConfigModeError> {
        if surface == ProjectConfigUse::Unused {
            return Ok(Self::Ignore);
        }
        let trust_env = std::env::var(TRUST_PROJECT_ENV).ok();
        let ignore_env = std::env::var(PROJECT_CONFIG_ENV).ok();
        Self::resolve_with_env(
            surface,
            trust_project,
            no_project_config,
            trust_env.as_deref(),
            ignore_env.as_deref(),
        )
    }

    /// Resolve the mode against explicit environment values.
    ///
    /// Only `CAKE_TRUST_PROJECT=1` and `CAKE_PROJECT_CONFIG=off` request a
    /// mode; any other value is ignored.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectConfigModeError`] when the effective flags disagree.
    pub fn resolve_with_env(
        surface: ProjectConfigUse,
        trust_project: bool,
        no_project_config: bool,
        trust_env: Option<&str>,
        ignore_env: Option<&str>,
    ) -> Result<Self, ProjectConfigModeError> {
        if surface == ProjectConfigUse::Unused {
            return Ok(Self::Ignore);
        }
        let env_trusts = trust_env == Some("1");
        let env_ignores = ignore_env == Some("off");
        Self::check_conflict(trust_project, no_project_config, env_trusts, env_ignores)?;
        let trusts = trust_project || env_trusts;
        let ignores = no_project_config || env_ignores;
        Ok(Self::from_surface(surface, trusts && !ignores, ignores))
    }

    /// Reject two one-run modes requested at once.
    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "four independent one-run inputs make the precedence explicit"
    )]
    const fn check_conflict(
        trust_project: bool,
        no_project_config: bool,
        env_trusts: bool,
        env_ignores: bool,
    ) -> Result<(), ProjectConfigModeError> {
        if !(trust_project || env_trusts) || !(no_project_config || env_ignores) {
            return Ok(());
        }
        Err(if trust_project && no_project_config {
            ProjectConfigModeError::ConflictingFlags
        } else if env_trusts && env_ignores {
            ProjectConfigModeError::ConflictingEnvironment
        } else {
            ProjectConfigModeError::FlagEnvironmentConflict
        })
    }

    /// Combine a command surface with the effective one-run choices.
    const fn from_surface(surface: ProjectConfigUse, trusts: bool, ignores: bool) -> Self {
        if ignores {
            return Self::Ignore;
        }
        match (surface, trusts) {
            (ProjectConfigUse::Enforce, false) => Self::Enforce,
            (ProjectConfigUse::Enforce, true) => Self::EnforceOneRun,
            (ProjectConfigUse::Inspect, false) => Self::Inspect,
            (ProjectConfigUse::Inspect, true) => Self::InspectOneRun,
            (ProjectConfigUse::Unused, _) => Self::Ignore,
        }
    }

    /// A stable telemetry label for the effective mode.
    #[must_use]
    pub const fn telemetry_mode(self) -> &'static str {
        match self {
            Self::Enforce | Self::Inspect => "persistent",
            Self::EnforceOneRun | Self::InspectOneRun => "one_run",
            Self::Ignore => "no_project_config",
        }
    }
}

/// Two one-run modes were requested at once.
#[derive(Debug, thiserror::Error)]
pub enum ProjectConfigModeError {
    /// Both root flags were supplied.
    #[error(
        "--trust-project and --no-project-config conflict; choose one trust mode for this invocation"
    )]
    ConflictingFlags,
    /// Both environment variables requested a mode.
    #[error("CAKE_TRUST_PROJECT=1 and CAKE_PROJECT_CONFIG=off conflict; choose one trust mode")]
    ConflictingEnvironment,
    /// A flag and an environment variable requested different modes.
    #[error(
        "a project trust flag conflicts with CAKE_TRUST_PROJECT/CAKE_PROJECT_CONFIG; choose one trust mode"
    )]
    FlagEnvironmentConflict,
}

/// The state of the project trust decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustState {
    /// No authority-bearing project configuration is present.
    NotRequired,
    /// The current snapshot matches a trust record.
    Trusted,
    /// Authority-bearing configuration exists and no record matches it.
    Untrusted,
    /// A record exists but the current snapshot differs.
    Changed,
    /// The record cannot be used: symlink, permissions, version, or root.
    Invalid,
}

impl TrustState {
    /// Whether the project authority may be applied.
    #[must_use]
    pub const fn allows_authority(self) -> bool {
        matches!(self, Self::NotRequired | Self::Trusted)
    }

    /// A stable label for diagnostics and machine output.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::NotRequired => "not_required",
            Self::Trusted => "trusted",
            Self::Untrusted => "untrusted",
            Self::Changed => "changed",
            Self::Invalid => "invalid",
        }
    }
}

/// The kind of project file that supplies authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityKind {
    /// `.cake/settings.toml` authority-bearing keys.
    Settings,
    /// `.cake/hooks.json` or `.cake/hooks.local.json`.
    Hooks,
    /// A regular file under `.cake/tools/`.
    #[serde(rename = "toolbox")]
    ToolboxFile,
}

/// One project-relative file and the authority-bearing keys it supplies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuthoritySource {
    /// Project-relative path, using `/` separators.
    pub path: String,
    /// What kind of file supplies the authority.
    pub kind: AuthorityKind,
    /// Settings keys, formatted for a reviewer.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,
    /// Whether a toolbox file is executable.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub executable: bool,
}

/// One file in a trust manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedFile {
    /// Project-relative path, using `/` separators.
    pub path: String,
    /// Lowercase hex SHA-256 of the file bytes.
    pub sha256: String,
    /// Whether the executable bit is set.
    pub executable: bool,
}

impl TrustedFile {
    /// Build a manifest entry from a path and its exact bytes.
    fn from_bytes(path: String, bytes: &[u8], executable: bool) -> Self {
        Self {
            path,
            sha256: sha256_hex(bytes),
            executable,
        }
    }
}

/// A deterministic snapshot of every regular file under project `.cake/`.
#[derive(Debug, Clone, Default)]
pub struct ProjectManifest {
    /// Files sorted by normalized project-relative path.
    pub files: Vec<TrustedFile>,
}

impl ProjectManifest {
    /// Compare this snapshot against a stored record.
    ///
    /// The result is sorted by path so diagnostics and tests are stable.
    #[must_use]
    pub fn compare(&self, record: &TrustRecord) -> Vec<ManifestChange> {
        let current: BTreeMap<&str, &TrustedFile> = self
            .files
            .iter()
            .map(|file| (file.path.as_str(), file))
            .collect();
        let trusted: BTreeMap<&str, &TrustedFile> = record
            .files
            .iter()
            .map(|file| (file.path.as_str(), file))
            .collect();
        let mut changes = Vec::new();
        for (path, file) in &current {
            match trusted.get(path) {
                None => changes.push(ManifestChange {
                    path: (*path).to_string(),
                    change: ManifestChangeKind::Added,
                }),
                Some(trusted_file) if trusted_file.sha256 != file.sha256 => {
                    changes.push(ManifestChange {
                        path: (*path).to_string(),
                        change: ManifestChangeKind::Modified,
                    });
                },
                Some(trusted_file) if trusted_file.executable != file.executable => {
                    changes.push(ManifestChange {
                        path: (*path).to_string(),
                        change: ManifestChangeKind::ModeChanged,
                    });
                },
                Some(_) => {},
            }
        }
        for path in trusted.keys() {
            if !current.contains_key(path) {
                changes.push(ManifestChange {
                    path: (*path).to_string(),
                    change: ManifestChangeKind::Removed,
                });
            }
        }
        changes.sort_by(|left, right| left.path.cmp(&right.path));
        changes
    }
}

/// How one file differs from the trusted snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestChangeKind {
    /// The file is present now but was not trusted.
    Added,
    /// The file was trusted but is now absent.
    Removed,
    /// The file bytes changed.
    Modified,
    /// The executable bit changed.
    ModeChanged,
}

impl ManifestChangeKind {
    /// A stable label for diagnostics.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Modified => "modified",
            Self::ModeChanged => "mode-changed",
        }
    }
}

/// One changed path in a trust comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ManifestChange {
    /// Project-relative path.
    pub path: String,
    /// What changed.
    pub change: ManifestChangeKind,
}

/// A versioned trust record keyed to one canonical project root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustRecord {
    /// Record format version.
    pub version: u32,
    /// Canonical project root this record was created for.
    pub project_root: String,
    /// The trusted file manifest.
    pub files: Vec<TrustedFile>,
}

impl TrustRecord {
    /// Read and validate a trust record for `expected_root`.
    ///
    /// Returns `Ok(None)` when no record exists. A symlinked, non-regular,
    /// group- or world-accessible, wrong-version, or wrong-root record is an
    /// invalid record and is never followed.
    ///
    /// # Errors
    ///
    /// Returns [`TrustError::Invalid`] for an unusable record and
    /// [`TrustError::Io`] for an unreadable path.
    pub fn read(path: &Path, expected_root: &Path) -> Result<Option<Self>, TrustError> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(TrustError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            },
        };
        Self::validate_metadata(path, &metadata)?;
        let bytes = fs::read(path).map_err(|source| TrustError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let record: Self = serde_json::from_slice(&bytes).map_err(|error| TrustError::Invalid {
            path: path.to_path_buf(),
            reason: format!("the trust record is not valid JSON: {error}"),
        })?;
        Self::validate_contents(path, &record, expected_root)?;
        Ok(Some(record))
    }

    /// Reject a symlinked, non-regular, or group/world-accessible record.
    fn validate_metadata(path: &Path, metadata: &fs::Metadata) -> Result<(), TrustError> {
        if metadata.file_type().is_symlink() {
            return Err(TrustError::Invalid {
                path: path.to_path_buf(),
                reason: "the trust record is a symlink".to_string(),
            });
        }
        if !metadata.is_file() {
            return Err(TrustError::Invalid {
                path: path.to_path_buf(),
                reason: "the trust record is not a regular file".to_string(),
            });
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(TrustError::Invalid {
                    path: path.to_path_buf(),
                    reason: "the trust record must be owner-only (0600)".to_string(),
                });
            }
        }
        Ok(())
    }

    /// Reject an unsupported version or a record for another root.
    fn validate_contents(
        path: &Path,
        record: &Self,
        expected_root: &Path,
    ) -> Result<(), TrustError> {
        if record.version != TRUST_RECORD_VERSION {
            return Err(TrustError::Invalid {
                path: path.to_path_buf(),
                reason: format!(
                    "unsupported trust record version {}; expected {TRUST_RECORD_VERSION}",
                    record.version
                ),
            });
        }
        let expected = expected_root.to_string_lossy();
        if record.project_root != expected {
            return Err(TrustError::Invalid {
                path: path.to_path_buf(),
                reason: format!(
                    "the trust record belongs to '{}', not '{}'",
                    record.project_root, expected
                ),
            });
        }
        Ok(())
    }

    /// Write a record atomically with owner-only permissions.
    ///
    /// The parent directory is created with `0700` on Unix. A failed write
    /// leaves any existing record untouched.
    ///
    /// # Errors
    ///
    /// Returns [`TrustError::Invalid`] when the destination is a symlink or
    /// unsafe, and [`TrustError::Io`] when the write fails.
    pub fn write_atomic(path: &Path, record: &Self) -> Result<(), TrustError> {
        let Some(parent) = path.parent() else {
            return Err(TrustError::Invalid {
                path: path.to_path_buf(),
                reason: "the trust record path has no parent directory".to_string(),
            });
        };
        ensure_owner_only_dir(parent)?;
        let json = serde_json::to_vec_pretty(record).map_err(|error| TrustError::Invalid {
            path: path.to_path_buf(),
            reason: format!("failed to serialize the trust record: {error}"),
        })?;
        let temp = parent.join(format!(
            ".{}.{}.{}.tmp",
            path.file_name()
                .map_or("trust", |name| name.to_str().unwrap_or("trust")),
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos()),
        ));
        let write_result = write_temp_file(&temp, &json).and_then(|()| {
            fs::rename(&temp, path).map_err(|source| TrustError::Io {
                path: path.to_path_buf(),
                source,
            })
        });
        if write_result.is_err() {
            drop(fs::remove_file(&temp));
        }
        write_result
    }
}

/// Exact bytes of a project hook file, read once by the trust snapshot.
#[derive(Debug, Clone)]
pub struct HookFileSource {
    /// Absolute path used for parse diagnostics.
    pub path: PathBuf,
    /// The exact bytes that were hashed.
    pub bytes: Vec<u8>,
}

/// The trust decision for one project snapshot.
#[derive(Debug, Clone)]
pub struct ProjectTrustReport {
    /// The decision.
    pub state: TrustState,
    /// The effective mode this report was evaluated for.
    pub mode: ProjectConfigMode,
    /// The canonical project root.
    pub project_root: PathBuf,
    /// One-way key derived from the canonical root.
    pub project_key: String,
    /// Where the persistent record would live.
    pub record_path: PathBuf,
    /// Authority-bearing files and keys.
    pub authority: Vec<AuthoritySource>,
    /// Differences from a persistent record, sorted by path.
    pub changes: Vec<ManifestChange>,
    /// Why an invalid record was rejected, when known.
    pub invalid_reason: Option<String>,
}

impl ProjectTrustReport {
    /// Whether project authority may be applied.
    #[must_use]
    pub const fn allows_authority(&self) -> bool {
        self.state.allows_authority()
    }

    /// Number of distinct authority-bearing files.
    #[must_use]
    pub fn authority_file_count(&self) -> usize {
        let mut paths: Vec<&str> = self
            .authority
            .iter()
            .map(|source| source.path.as_str())
            .collect();
        paths.sort_unstable();
        paths.dedup();
        paths.len()
    }

    /// A path-free telemetry mode label.
    #[must_use]
    pub const fn telemetry_mode(&self) -> &'static str {
        if matches!(self.mode, ProjectConfigMode::Ignore) {
            "no_project_config"
        } else if matches!(self.state, TrustState::NotRequired) {
            "not_required"
        } else {
            self.mode.telemetry_mode()
        }
    }

    /// Return the diagnostic that stops an enforcing invocation.
    ///
    /// # Errors
    ///
    /// Returns [`TrustError`] when the state is untrusted, changed, or invalid.
    pub fn require_trusted(&self) -> Result<(), TrustError> {
        match self.state {
            TrustState::NotRequired | TrustState::Trusted => Ok(()),
            TrustState::Untrusted => Err(TrustError::Untrusted {
                detail: format_authority(&self.authority),
                record_path: self.record_path.clone(),
            }),
            TrustState::Changed => Err(TrustError::Changed {
                detail: format_changes(&self.changes),
                record_path: self.record_path.clone(),
            }),
            TrustState::Invalid => Err(TrustError::Invalid {
                path: self.record_path.clone(),
                reason: self
                    .invalid_reason
                    .clone()
                    .unwrap_or_else(|| "the trust record cannot be used".to_string()),
            }),
        }
    }
}

/// A trust-boundary failure that must stop the invocation.
#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    /// Authority-bearing project configuration exists with no trust record.
    #[error(
        "Project configuration requires trust before it can be used:\n{detail}\n\
         \n\
         Run `cake trust` to review and trust this snapshot.\n\
         Or re-run with --trust-project (CAKE_TRUST_PROJECT=1) for this invocation only.\n\
         Or re-run with --no-project-config (CAKE_PROJECT_CONFIG=off) to ignore project .cake configuration.\n\
         Trust record: {record_path}"
    )]
    Untrusted {
        /// Formatted authority-bearing files and keys.
        detail: String,
        /// Where the persistent record would live.
        record_path: PathBuf,
    },
    /// A record exists but the current snapshot differs.
    #[error(
        "Project configuration changed since it was trusted:\n{detail}\n\
         \n\
         Run `cake trust` again to review the new snapshot.\n\
         Trust record: {record_path}"
    )]
    Changed {
        /// Formatted changed files.
        detail: String,
        /// The record path.
        record_path: PathBuf,
    },
    /// The trust record cannot be used.
    #[error("Trust record is invalid: {path}: {reason}")]
    Invalid {
        /// The record path.
        path: PathBuf,
        /// Why the record was rejected.
        reason: String,
    },
    /// An authority-bearing path is a symlink or not a regular file/directory.
    #[error(
        "Unsafe project configuration path '{path}': {reason}. Authority-bearing project files must be regular files or directories inside the project root."
    )]
    UnsafePath {
        /// The offending path.
        path: PathBuf,
        /// What is wrong with it.
        reason: String,
    },
    /// A project file or record could not be read.
    #[error("Failed to read {path}: {source}")]
    Io {
        /// The failing path.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
}

/// A settings or trust failure while loading project configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigLoadError {
    /// Settings are invalid.
    #[error(transparent)]
    Settings(#[from] SettingsError),
    /// Project authority is untrusted.
    #[error(transparent)]
    Trust(#[from] TrustError),
}

/// The project files the snapshot read, and the settings parsed from them.
#[derive(Debug)]
pub struct ProjectConfigSnapshot {
    /// Canonical project root.
    pub project_root: PathBuf,
    /// One-way key derived from the canonical root.
    pub project_key: String,
    /// Effective mode used to load the snapshot.
    pub mode: ProjectConfigMode,
    /// Every regular file under `.cake/`.
    pub manifest: ProjectManifest,
    /// `.cake/settings.toml` parsed from the manifest bytes.
    pub settings: Option<SettingsSource>,
    /// `.cake/hooks.json` and `.cake/hooks.local.json` bytes.
    pub hook_files: Vec<HookFileSource>,
    /// Authority-bearing files and keys.
    pub authority: Vec<AuthoritySource>,
    /// Where the persistent record lives.
    pub record_path: PathBuf,
    /// Non-authority symlinks that were skipped.
    pub warnings: Vec<String>,
}

impl ProjectConfigSnapshot {
    /// Read and classify the project `.cake/` tree without executing anything.
    ///
    /// In [`ProjectConfigMode::Ignore`], no project file is read.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsError`] when project settings cannot be parsed or
    /// define models, and [`TrustError`] when an authority path is unsafe.
    pub fn load(
        project_dir: &Path,
        config_dir: &Path,
        mode: ProjectConfigMode,
    ) -> Result<Self, ConfigLoadError> {
        let project_root = match canonical_project_root(project_dir) {
            Ok(root) => root,
            Err(TrustError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                // A missing project directory has no configuration to trust;
                // callers such as tests may probe a path before it exists.
                project_dir.to_path_buf()
            },
            Err(error) => return Err(error.into()),
        };
        let project_key = project_key(&project_root);
        let record_path = trust_record_path(config_dir, &project_root);
        if !mode.loads_project_config() {
            return Ok(Self::empty(project_root, project_key, record_path, mode));
        }
        let scan = scan_project_cake(&project_root)?;
        let settings = Self::parse_settings(&project_root, &scan)?;
        let authority = Self::build_authority(&project_root, &scan, settings.as_ref());
        Ok(Self {
            project_root,
            project_key,
            mode,
            manifest: scan.manifest,
            settings,
            hook_files: scan.hook_files,
            authority,
            record_path,
            warnings: scan.warnings,
        })
    }

    /// Parse project settings and reject a digest mismatch or project models.
    fn parse_settings(
        project_root: &Path,
        scan: &ProjectConfigSnapshotFiles,
    ) -> Result<Option<SettingsSource>, ConfigLoadError> {
        let Some(bytes) = scan.settings_bytes.as_deref() else {
            return Ok(None);
        };
        let source = SettingsLoader::read_source_bytes(
            &project_root.join(".cake").join("settings.toml"),
            SettingsOrigin::Project,
            bytes,
        )?;
        Self::validate_settings_digest(scan, &source)?;
        if !source.settings.models.is_empty() {
            return Err(SettingsError::ProjectModelsUnsupported {
                path: source.path.clone(),
                names: source
                    .settings
                    .models
                    .iter()
                    .map(|model| model.name.clone())
                    .collect(),
            }
            .into());
        }
        Ok(Some(source))
    }

    /// Ensure the parsed settings bytes are the manifest bytes.
    fn validate_settings_digest(
        scan: &ProjectConfigSnapshotFiles,
        source: &SettingsSource,
    ) -> Result<(), ConfigLoadError> {
        let manifest_digest = scan
            .manifest
            .files
            .iter()
            .find(|file| file.path == ".cake/settings.toml")
            .map(|file| file.sha256.as_str());
        if manifest_digest == Some(source.sha256.as_str()) {
            return Ok(());
        }
        Err(TrustError::Invalid {
            path: source.path.clone(),
            reason: "the parsed settings digest does not match the manifest entry".to_string(),
        }
        .into())
    }

    /// Build the authority report from settings, hook, and toolbox files.
    fn build_authority(
        project_root: &Path,
        scan: &ProjectConfigSnapshotFiles,
        settings: Option<&SettingsSource>,
    ) -> Vec<AuthoritySource> {
        let mut authority = settings.map_or_else(Vec::new, |source| {
            ProjectAuthority::from_settings(&source.settings, Path::new(".cake/settings.toml"))
        });
        authority.extend(scan.hook_files.iter().map(|hook| AuthoritySource {
            path: relative_display(project_root, &hook.path),
            kind: AuthorityKind::Hooks,
            keys: Vec::new(),
            executable: false,
        }));
        authority.extend(scan.toolbox_files.iter().map(|file| AuthoritySource {
            path: file.path.clone(),
            kind: AuthorityKind::ToolboxFile,
            keys: Vec::new(),
            executable: file.executable,
        }));
        authority
    }

    /// Build an empty snapshot without touching the filesystem.
    fn empty(
        project_root: PathBuf,
        project_key: String,
        record_path: PathBuf,
        mode: ProjectConfigMode,
    ) -> Self {
        Self {
            project_root,
            project_key,
            mode,
            manifest: ProjectManifest::default(),
            settings: None,
            hook_files: Vec::new(),
            authority: Vec::new(),
            record_path,
            warnings: Vec::new(),
        }
    }

    /// Evaluate the trust decision.
    ///
    /// Enforcing modes return an error when authority is untrusted, changed, or
    /// invalid. Inspection modes return a report for those states.
    ///
    /// # Errors
    ///
    /// Returns [`TrustError`] when an enforcing mode cannot use the project.
    pub fn evaluate(&self) -> Result<ProjectTrustReport, TrustError> {
        if !self.mode.loads_project_config() || self.authority.is_empty() {
            return Ok(self.report(TrustState::NotRequired, Vec::new()));
        }
        if self.mode.one_run() {
            return Ok(self.report(TrustState::Trusted, Vec::new()));
        }
        self.evaluate_record()
    }

    fn evaluate_record(&self) -> Result<ProjectTrustReport, TrustError> {
        match TrustRecord::read(&self.record_path, &self.project_root) {
            Ok(None) => self.handle_missing_record(),
            Ok(Some(record)) => self.compare_record(&record),
            Err(error) => self.handle_record_error(error),
        }
    }

    fn handle_missing_record(&self) -> Result<ProjectTrustReport, TrustError> {
        if self.mode.is_inspecting() {
            return Ok(self.report(TrustState::Untrusted, Vec::new()));
        }
        Err(TrustError::Untrusted {
            detail: format_authority(&self.authority),
            record_path: self.record_path.clone(),
        })
    }

    fn compare_record(&self, record: &TrustRecord) -> Result<ProjectTrustReport, TrustError> {
        let changes = self.manifest.compare(record);
        if changes.is_empty() {
            return Ok(self.report(TrustState::Trusted, Vec::new()));
        }
        if self.mode.is_inspecting() {
            return Ok(self.report(TrustState::Changed, changes));
        }
        Err(TrustError::Changed {
            detail: format_changes(&changes),
            record_path: self.record_path.clone(),
        })
    }

    fn handle_record_error(&self, error: TrustError) -> Result<ProjectTrustReport, TrustError> {
        if !self.mode.is_inspecting() {
            return Err(error);
        }
        let mut report = self.report(TrustState::Invalid, Vec::new());
        report.invalid_reason = Some(error.to_string());
        Ok(report)
    }

    fn report(&self, state: TrustState, changes: Vec<ManifestChange>) -> ProjectTrustReport {
        ProjectTrustReport {
            state,
            mode: self.mode,
            project_root: self.project_root.clone(),
            project_key: self.project_key.clone(),
            record_path: self.record_path.clone(),
            authority: self.authority.clone(),
            changes,
            invalid_reason: None,
        }
    }
}

/// Raw files read from project `.cake/`.
struct CakeScan {
    manifest: ProjectManifest,
    settings_bytes: Option<Vec<u8>>,
    hook_files: Vec<HookFileSource>,
    toolbox_files: Vec<TrustedFile>,
    warnings: Vec<String>,
}

impl CakeScan {
    const fn new() -> Self {
        Self {
            manifest: ProjectManifest { files: Vec::new() },
            settings_bytes: None,
            hook_files: Vec::new(),
            toolbox_files: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

/// Scan project `.cake/` with `symlink_metadata` and build a manifest.
///
/// The scan rejects a symlinked `.cake`, settings file, hook file, or toolbox
/// entry. Symlinks elsewhere are skipped and reported as warnings.
///
/// # Errors
///
/// Returns [`TrustError::UnsafePath`] for an authority path that is a symlink
/// or not a regular file/directory, and [`TrustError::Io`] for read failures.
pub fn scan_project_cake(project_root: &Path) -> Result<ProjectConfigSnapshotFiles, TrustError> {
    let cake = project_root.join(".cake");
    let metadata = match fs::symlink_metadata(&cake) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(ProjectConfigSnapshotFiles {
                manifest: ProjectManifest::default(),
                settings_bytes: None,
                hook_files: Vec::new(),
                toolbox_files: Vec::new(),
                warnings: Vec::new(),
            });
        },
        Err(source) => {
            return Err(TrustError::Io { path: cake, source });
        },
    };
    if metadata.file_type().is_symlink() {
        return Err(TrustError::UnsafePath {
            path: cake,
            reason: "the .cake directory is a symlink".to_string(),
        });
    }
    if !metadata.is_dir() {
        return Err(TrustError::UnsafePath {
            path: cake,
            reason: "the .cake path is not a directory".to_string(),
        });
    }
    let mut scan = CakeScan::new();
    walk_cake(project_root, &cake, &mut scan)?;
    scan.manifest
        .files
        .sort_by(|left, right| left.path.cmp(&right.path));
    Ok(ProjectConfigSnapshotFiles {
        manifest: scan.manifest,
        settings_bytes: scan.settings_bytes,
        hook_files: scan.hook_files,
        toolbox_files: scan.toolbox_files,
        warnings: scan.warnings,
    })
}

/// Files read from project `.cake/`, exposed for focused tests.
#[derive(Debug)]
pub struct ProjectConfigSnapshotFiles {
    /// Every regular file under `.cake/`.
    pub manifest: ProjectManifest,
    /// Exact `.cake/settings.toml` bytes.
    pub settings_bytes: Option<Vec<u8>>,
    /// Exact hook file bytes.
    pub hook_files: Vec<HookFileSource>,
    /// Toolbox files under `.cake/tools/`.
    pub toolbox_files: Vec<TrustedFile>,
    /// Non-authority symlinks that were skipped.
    pub warnings: Vec<String>,
}

fn walk_cake(project_root: &Path, dir: &Path, scan: &mut CakeScan) -> Result<(), TrustError> {
    let read_dir = fs::read_dir(dir).map_err(|source| TrustError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    let mut entries: Vec<PathBuf> = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    for path in entries {
        let Some(relative) = relative_path(project_root, &path)? else {
            scan.warnings.push(format!(
                "skipping non-UTF-8 path under .cake: {}",
                path.display()
            ));
            continue;
        };
        let metadata = fs::symlink_metadata(&path).map_err(|source| TrustError::Io {
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            visit_symlink(scan, path, &relative)?;
        } else if metadata.is_dir() {
            visit_directory(project_root, scan, path, &relative)?;
        } else {
            visit_file(scan, path, &relative, &metadata)?;
        }
    }
    Ok(())
}

fn visit_symlink(scan: &mut CakeScan, path: PathBuf, relative: &str) -> Result<(), TrustError> {
    if is_authority_path(relative) {
        return Err(TrustError::UnsafePath {
            path,
            reason: format!("'{relative}' is a symlink"),
        });
    }
    scan.warnings.push(format!(
        "skipping symlink '{relative}' under .cake; only authority-bearing symlinks are rejected"
    ));
    Ok(())
}

fn visit_directory(
    project_root: &Path,
    scan: &mut CakeScan,
    path: PathBuf,
    relative: &str,
) -> Result<(), TrustError> {
    if relative == ".cake/worktrees" {
        // Worktrees are generated checkouts, never project configuration.
        // Hashing them would be slow and would make trust depend on unrelated
        // working trees.
        return Ok(());
    }
    if is_authority_file(relative) {
        return Err(TrustError::UnsafePath {
            path,
            reason: format!("'{relative}' is not a regular file"),
        });
    }
    walk_cake(project_root, &path, scan)
}

fn visit_file(
    scan: &mut CakeScan,
    path: PathBuf,
    relative: &str,
    metadata: &fs::Metadata,
) -> Result<(), TrustError> {
    if !metadata.is_file() {
        return Err(TrustError::UnsafePath {
            path,
            reason: "not a regular file or directory".to_string(),
        });
    }
    let bytes = fs::read(&path).map_err(|source| TrustError::Io {
        path: path.clone(),
        source,
    })?;
    let executable = is_executable(metadata);
    let trusted = TrustedFile::from_bytes(relative.to_string(), &bytes, executable);
    if is_hook_file(relative) {
        scan.hook_files.push(HookFileSource { path, bytes });
    } else if relative == ".cake/settings.toml" {
        scan.settings_bytes = Some(bytes);
    }
    if relative.starts_with(".cake/tools/") {
        scan.toolbox_files.push(trusted.clone());
    }
    scan.manifest.files.push(trusted);
    Ok(())
}

/// Whether a path is an authority-bearing file that may not be a symlink.
fn is_authority_path(relative: &str) -> bool {
    is_authority_file(relative) || relative == ".cake/tools" || relative.starts_with(".cake/tools/")
}

/// Whether a path is a settings or hook file.
fn is_authority_file(relative: &str) -> bool {
    relative == ".cake/settings.toml" || is_hook_file(relative)
}

fn is_hook_file(relative: &str) -> bool {
    matches!(relative, ".cake/hooks.json" | ".cake/hooks.local.json")
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
const fn is_executable(_metadata: &fs::Metadata) -> bool {
    false
}

fn relative_path(root: &Path, path: &Path) -> Result<Option<String>, TrustError> {
    let Ok(relative) = path.strip_prefix(root) else {
        return Err(TrustError::UnsafePath {
            path: path.to_path_buf(),
            reason: "path is outside the project root".to_string(),
        });
    };
    let Some(text) = relative.to_str() else {
        if is_authority_os_path(root, path) {
            return Err(TrustError::UnsafePath {
                path: path.to_path_buf(),
                reason: "path is not valid UTF-8".to_string(),
            });
        }
        return Ok(None);
    };
    Ok(Some(text.replace('\\', "/")))
}

fn relative_display(root: &Path, path: &Path) -> String {
    relative_path(root, path)
        .ok()
        .flatten()
        .unwrap_or_else(|| path.display().to_string())
}

/// Whether a raw OS path is one of the known authority paths.
fn is_authority_os_path(root: &Path, path: &Path) -> bool {
    let cake = root.join(".cake");
    path == cake.join("settings.toml")
        || path == cake.join("hooks.json")
        || path == cake.join("hooks.local.json")
        || path.starts_with(cake.join("tools"))
}

/// Resolve a project directory to its canonical root.
///
/// # Errors
///
/// Returns [`TrustError::Io`] when the directory cannot be canonicalized.
pub fn canonical_project_root(project_dir: &Path) -> Result<PathBuf, TrustError> {
    fs::canonicalize(project_dir).map_err(|source| TrustError::Io {
        path: project_dir.to_path_buf(),
        source,
    })
}

/// A stable one-way filename key for a canonical project root.
#[must_use]
pub fn project_key(root: &Path) -> String {
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt as _;
        root.as_os_str().as_bytes().to_vec()
    };
    #[cfg(not(unix))]
    let bytes = root.to_string_lossy().as_bytes().to_vec();
    sha256_hex(&bytes)
}

/// The persistent trust record path for a canonical project root.
#[must_use]
pub fn trust_record_path(config_dir: &Path, root: &Path) -> PathBuf {
    config_dir
        .join("cake")
        .join(TRUST_DIR)
        .join(format!("{}.json", project_key(root)))
}

/// Remove the persistent trust record for a canonical project root.
///
/// Returns `true` when a record was removed. A missing record is success. A
/// symlinked trust directory is rejected rather than followed; a symlinked
/// record is unlinked itself, never its target.
///
/// # Errors
///
/// Returns [`TrustError::Invalid`] for a symlinked trust directory and
/// [`TrustError::Io`] when removal fails.
pub fn revoke_trust_record(config_dir: &Path, root: &Path) -> Result<bool, TrustError> {
    let path = trust_record_path(config_dir, root);
    let Some(parent) = path.parent() else {
        return Ok(false);
    };
    match fs::symlink_metadata(parent) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(TrustError::Invalid {
                path: parent.to_path_buf(),
                reason: "the trust directory is a symlink".to_string(),
            });
        },
        Ok(_) => {},
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(TrustError::Io {
                path: parent.to_path_buf(),
                source,
            });
        },
    }
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() => Err(TrustError::Invalid {
            path,
            reason: "the trust record is a directory".to_string(),
        }),
        Ok(_) => {
            fs::remove_file(&path).map_err(|source| TrustError::Io { path, source })?;
            Ok(true)
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(TrustError::Io { path, source }),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn ensure_owner_only_dir(path: &Path) -> Result<(), TrustError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(TrustError::Invalid {
                    path: path.to_path_buf(),
                    reason: "the trust directory is a symlink".to_string(),
                });
            }
            if !metadata.is_dir() {
                return Err(TrustError::Invalid {
                    path: path.to_path_buf(),
                    reason: "the trust directory is not a directory".to_string(),
                });
            }
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|source| TrustError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        },
        Err(source) => {
            return Err(TrustError::Io {
                path: path.to_path_buf(),
                source,
            });
        },
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
            TrustError::Io {
                path: path.to_path_buf(),
                source,
            }
        })?;
    }
    Ok(())
}

fn write_temp_file(path: &Path, bytes: &[u8]) -> Result<(), TrustError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|source| TrustError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| TrustError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn format_authority(authority: &[AuthoritySource]) -> String {
    let mut lines = Vec::new();
    for source in authority {
        if source.keys.is_empty() {
            let executable = if source.executable {
                " (executable)"
            } else {
                ""
            };
            lines.push(format!("  {}{executable}", source.path));
        } else {
            lines.push(format!("  {}: {}", source.path, source.keys.join(", ")));
        }
    }
    lines.join("\n")
}

fn format_changes(changes: &[ManifestChange]) -> String {
    changes
        .iter()
        .map(|change| format!("  {}: {}", change.change.label(), change.path))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[path = "trust_tests.rs"]
mod tests;
