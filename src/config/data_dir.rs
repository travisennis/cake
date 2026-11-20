use std::{
    fs::{self, File},
    io::BufReader,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::config::blob_store::{self as blob_store, BlobDiagnostics};
use crate::config::session_jsonl::SessionFramer;
use crate::config::{BlobStore, Session, git, session::CURRENT_FORMAT_VERSION};
use crate::types::{GitState, SessionRecord};

/// A data-directory preparation failure.
///
/// These are user-environment problems (a data root that is inaccessible or
/// cannot be created), so they classify as input/config errors (exit 3) rather
/// than agent errors. The io error is the [`std::error::Error::source`], so
/// chain-formatting (anyhow's `{error:#}`) renders the path, the hint, and the
/// underlying io error without duplication.
#[derive(Debug, thiserror::Error)]
pub enum DataDirError {
    /// The path could not be stat'd (`NotFound` excluded). Under a sandbox that
    /// denies stat on an existing directory this is the failure that used to
    /// surface as a misleading `File exists (os error 17)`.
    #[error(
        "data directory '{path}' is not accessible (it may be denied by the current sandbox or filesystem permissions; set CAKE_DATA_DIR to a writable directory or grant it via [sandbox].writable)"
    )]
    NotAccessible {
        /// The data or sessions directory path that could not be accessed.
        path: PathBuf,
        /// The underlying stat error.
        #[source]
        source: std::io::Error,
    },
    /// The path was missing but could not be created.
    #[error(
        "failed to create data directory '{path}' (set CAKE_DATA_DIR to a writable directory or grant it via [sandbox].writable)"
    )]
    CreateFailed {
        /// The data or sessions directory path that could not be created.
        path: PathBuf,
        /// The underlying create error.
        #[source]
        source: std::io::Error,
    },
    /// A configured store path is a symlink, which Cake refuses to follow.
    #[error("{kind} '{path}' is a symlink; Cake requires a real directory and will not follow it")]
    Symlinked {
        /// The data or sessions directory label.
        kind: &'static str,
        /// The symlinked path.
        path: PathBuf,
    },
    /// A configured store path exists but is not a directory.
    #[error("{kind} '{path}' is not a directory; Cake requires a real directory")]
    NotADirectory {
        /// The data or sessions directory label.
        kind: &'static str,
        /// The non-directory path.
        path: PathBuf,
    },
    /// An existing sessions directory could not be tightened to `0700`.
    #[error("failed to set owner-only mode {mode:04o} on {kind} '{path}'")]
    TightenFailed {
        /// The data or sessions directory label.
        kind: &'static str,
        /// The directory path.
        path: PathBuf,
        /// The required mode.
        mode: u32,
        /// The underlying permission error.
        #[source]
        source: std::io::Error,
    },
}

/// Manages the data directory for session storage.
///
/// The cache directory defaults to `~/.cache/cake/` and contains cache data,
/// logs, and other ephemeral state. Session files are stored separately at
/// `~/.local/share/cake/sessions/` for durability and discoverability.
///
/// The directories can be overridden by setting the `CAKE_DATA_DIR` environment
/// variable. This is useful for testing and for running cake inside cake
/// (nested invocations) without filesystem collisions.
#[derive(Debug, Clone)]
pub struct DataDir {
    /// The path to the cache/data directory.
    data_dir: PathBuf,
    /// The path to the sessions directory.
    sessions_dir: PathBuf,
}

impl DataDir {
    /// Creates a new data directory instance for session storage.
    ///
    /// If `CAKE_DATA_DIR` is set, uses that path for both cache and sessions.
    /// Otherwise, cache defaults to `~/.cache/cake/` and sessions to
    /// `~/.local/share/cake/sessions/`. Directories are created if they do not exist.
    ///
    /// # Errors
    ///
    /// Returns [`DataDirError`] when a data root cannot be stat'd or created
    /// (for example, when a sandbox denies access to the default home paths),
    /// and an untyped error when the home directory cannot be determined.
    pub fn new() -> anyhow::Result<Self> {
        let (data_dir, sessions_dir) = if let Ok(custom) = std::env::var("CAKE_DATA_DIR") {
            let custom = PathBuf::from(custom);
            (custom.clone(), custom.join("sessions"))
        } else {
            let home_dir = dirs::home_dir();
            if let Some(home) = home_dir {
                (
                    home.join(".cache").join("cake"),
                    home.join(".local")
                        .join("share")
                        .join("cake")
                        .join("sessions"),
                )
            } else {
                return Err(anyhow!("Could not create data directory."));
            }
        };

        ensure_dir(&data_dir, "data directory", false)?;
        ensure_dir(&sessions_dir, "sessions directory", true)?;

        Ok(Self {
            data_dir,
            sessions_dir,
        })
    }

    /// Returns the path to the cache directory.
    ///
    /// The cache directory is typically `~/.cache/cake/`.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let data_dir = DataDir::new()?;
    /// let cache_path = data_dir.get_cache_dir();
    /// ```
    pub fn get_cache_dir(&self) -> PathBuf {
        self.data_dir.clone()
    }

    /// Returns the sessions directory path.
    ///
    /// Defaults to `~/.local/share/cake/sessions/` or `{CAKE_DATA_DIR}/sessions`.
    pub fn sessions_dir(&self) -> PathBuf {
        self.sessions_dir.clone()
    }

    /// Returns the canonical path for a session UUID.
    pub fn session_path(&self, id: uuid::Uuid) -> PathBuf {
        self.sessions_dir().join(format!("{id}.jsonl"))
    }

    /// Returns the blob directory sibling for a session UUID.
    ///
    /// The directory is not created by this method; the first blob write
    /// creates it lazily.
    pub fn session_blob_dir(&self, id: uuid::Uuid) -> PathBuf {
        self.sessions_dir().join(format!("{id}.blobs"))
    }

    /// Returns the content-addressed payload store for a session UUID.
    pub fn session_blob_store(&self, id: uuid::Uuid) -> BlobStore {
        BlobStore::new(self.session_blob_dir(id))
    }

    /// Build the read-only blob-health report for this data directory.
    ///
    /// The report pairs every `.jsonl` file with its sibling `.blobs`
    /// directory and reports unpaired stores. It never deletes, creates a
    /// directory, follows a symlink, or reads a payload into its output.
    ///
    /// # Errors
    ///
    /// Returns an error when the sessions directory itself cannot be read.
    /// A malformed or unreadable session is reported as data instead.
    pub fn blob_diagnostics(&self) -> anyhow::Result<BlobDiagnostics> {
        blob_store::blob_diagnostics(&self.sessions_dir)
    }

    /// Returns the canonical telemetry sidecar path for a session UUID.
    pub fn session_telemetry_path(&self, id: uuid::Uuid) -> PathBuf {
        self.get_cache_dir()
            .join("session-telemetry")
            .join(format!("{id}.ndjson"))
    }

    /// Roots Cake grants sandboxed commands and the in-process path checks
    /// read-only access to by default.
    ///
    /// Session analysis and debugging skills read Cake's own state --- session
    /// files, telemetry sidecars, and daily logs --- so those roots are granted
    /// read-only for every run. Returns the cache/data directory and the
    /// sessions directory, deduplicated when `CAKE_DATA_DIR` collapses them
    /// under one root. The global config directory is granted read-only by the
    /// run resource builder alongside these roots. See `docs/security.md`.
    #[must_use]
    pub fn sandbox_read_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.data_dir.clone()];
        if self.sessions_dir != self.data_dir {
            dirs.push(self.sessions_dir.clone());
        }
        dirs
    }

    /// Create a new session file and return a locked append handle.
    pub fn create_session_file(
        &self,
        session: &Session,
        tools: Vec<String>,
    ) -> anyhow::Result<File> {
        let session_path = self.session_path(session.id);
        let meta = session_meta_record(session, tools);
        Session::create_on_disk(&session_path, &meta)
    }

    /// Open an existing session file and return a locked append handle.
    pub fn open_session_for_append(&self, id: uuid::Uuid) -> anyhow::Result<File> {
        Session::open_for_append(&self.session_path(id))
    }

    /// Loads the most recent session for a given working directory.
    ///
    /// Scans all session files and finds the newest `.jsonl` file by the session
    /// creation timestamp in its `session_meta` record whose `working_directory`
    /// header field matches the given directory.
    /// Returns `None` if no matching sessions exist.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let data_dir = DataDir::new()?;
    /// let session = data_dir.load_latest_session(&PathBuf::from("/project"))?;
    /// match session {
    ///     Some(s) => println!("Found session: {}", s.id),
    ///     None => println!("No previous session found"),
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if the session directory cannot be read or if a
    /// matching session file cannot be loaded.
    pub fn load_latest_session(&self, working_dir: &Path) -> anyhow::Result<Option<Session>> {
        let session_dir = self.sessions_dir();

        tracing::info!(target: "cake", "Looking for latest session in {} for {}", session_dir.display(), working_dir.display());

        if !session_dir.exists() {
            tracing::info!(target: "cake", "Session directory does not exist: {}", session_dir.display());
            return Ok(None);
        }

        let result = fs::read_dir(&session_dir)
            .with_context(|| {
                format!(
                    "Failed to read session directory: {}",
                    session_dir.display()
                )
            })?
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
            .filter_map(|entry| {
                let path = entry.path();
                let header = read_session_header(&path).ok()?;
                (header.working_directory == working_dir).then_some((path, header.timestamp))
            })
            .max_by_key(|(_, timestamp)| *timestamp)
            .map(|(path, _)| self.load_session_file(&path))
            .transpose()?;

        if let Some(ref session) = result {
            tracing::info!(target: "cake", "Found latest session: {}", session.id);
        } else {
            tracing::info!(target: "cake", "No session found for working directory");
        }

        Ok(result)
    }

    /// Loads a specific session by UUID.
    ///
    /// Returns the session with the given ID, or `None` if no such session exists.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let data_dir = DataDir::new()?;
    /// let session = data_dir.load_session(
    ///     uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap()
    /// )?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if the session file exists but cannot be loaded.
    pub fn load_session(&self, id: uuid::Uuid) -> anyhow::Result<Option<Session>> {
        let session_path = self.session_path(id);

        tracing::info!(target: "cake", "Loading session {id} from {}", session_path.display());

        if !session_path.exists() {
            tracing::info!(target: "cake", "Session file does not exist: {}", session_path.display());
            return Ok(None);
        }

        self.load_session_file(&session_path).map(Some)
    }

    /// Load and hydrate one session file discovered in this data directory.
    ///
    /// The filename UUID selects the blob directory, so the header id must
    /// match it before any reference is resolved. Raw session readers remain
    /// available for callers that only need identity or ordering.
    fn load_session_file(&self, path: &Path) -> anyhow::Result<Session> {
        let id = session_id_from_path(path)?;
        let mut session = Session::load(path)?;
        anyhow::ensure!(
            session.id == id,
            "Session header id {} does not match filename UUID {id} in {}",
            session.id,
            path.display()
        );
        self.session_blob_store(id)
            .hydrate_session(&id.to_string(), &mut session.records)
            .with_context(|| format!("Failed to hydrate session {} from {}", id, path.display()))?;
        Ok(session)
    }
}

/// Parse the session UUID encoded in a session file's name.
fn session_id_from_path(path: &Path) -> anyhow::Result<uuid::Uuid> {
    let stem = path
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| anyhow!("Session file name is not a UUID: {}", path.display()))?;
    uuid::Uuid::parse_str(stem)
        .with_context(|| format!("Session file name is not a UUID: {}", path.display()))
}

/// Minimal header struct for quickly inspecting a session file without
/// loading the entire conversation history.
#[derive(Deserialize)]
struct SessionFileHeader {
    format_version: u32,
    working_directory: PathBuf,
    /// Timestamp from the `session_meta` record, used instead of filesystem mtime.
    timestamp: DateTime<Utc>,
}

/// Reads only the first record line of a session file to extract its header.
///
/// Blank leading lines are skipped, so a header is found regardless of leading
/// whitespace, matching full load, replay, and listing.
fn read_session_header(path: &Path) -> anyhow::Result<SessionFileHeader> {
    let file = fs::File::open(path)
        .with_context(|| format!("Failed to open session file: {}", path.display()))?;
    let mut framer = SessionFramer::new(BufReader::new(file));
    let Some(line) = framer.next_record().with_context(|| {
        format!(
            "Failed to read header from session file: {}",
            path.display()
        )
    })?
    else {
        anyhow::bail!("Session file is empty: {}", path.display());
    };
    let header: SessionFileHeader = serde_json::from_str(&line.text)
        .with_context(|| format!("Failed to parse session header: {}", path.display()))?;
    if header.format_version != CURRENT_FORMAT_VERSION {
        anyhow::bail!(
            "Unsupported session format_version: {} (expected {}). Session file: {}",
            header.format_version,
            CURRENT_FORMAT_VERSION,
            path.display()
        );
    }
    Ok(header)
}

/// Ensures `path` is a real directory, distinguishing a denied stat from a
/// missing path.
///
/// `Path::exists()` reports every stat error as "missing", so under a sandbox
/// that denies stat on an existing directory the previous
/// `if !path.exists() { create_dir_all(path) }` flow called `mkdir` on the
/// existing path and surfaced a misleading `AlreadyExists (os error 17)`.
/// Here a stat failure that is not `NotFound` reports the path as
/// inaccessible instead of attempting creation.
///
/// A symlink or non-directory is rejected rather than followed, so a store path
/// cannot redirect session writes outside the intended directory. New paths are
/// created owner-only regardless of the process umask. `tighten_existing`
/// applies `0700` to an existing directory; it is used for the sessions
/// directory and deliberately not for a user-configured data root.
fn ensure_dir(path: &Path, kind: &'static str, tighten_existing: bool) -> Result<(), DataDirError> {
    ensure_dir_with(
        path,
        kind,
        tighten_existing,
        crate::config::owner_only::tighten_dir_owner_only,
    )
}

/// The shared body of [`ensure_dir`], with the mode setter split out so the
/// failure path is testable without requiring a directory owned by another
/// user.
fn ensure_dir_with(
    path: &Path,
    kind: &'static str,
    tighten_existing: bool,
    set_mode: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<(), DataDirError> {
    // `lstat` follows a symlink when the path ends in a separator, so check a
    // lexically normalized path. Otherwise `CAKE_DATA_DIR=/tmp/link/` would
    // pass the symlink check and resolve through the link.
    let checked = path.components().collect::<PathBuf>();
    match fs::symlink_metadata(&checked) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(DataDirError::Symlinked {
            kind,
            path: path.to_path_buf(),
        }),
        Ok(metadata) if !metadata.is_dir() => Err(DataDirError::NotADirectory {
            kind,
            path: path.to_path_buf(),
        }),
        Ok(_) => {
            if tighten_existing {
                set_mode(&checked).map_err(|source| DataDirError::TightenFailed {
                    kind,
                    path: path.to_path_buf(),
                    mode: crate::config::owner_only::OWNER_ONLY_DIR_MODE,
                    source,
                })?;
            }
            Ok(())
        },
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            crate::config::owner_only::create_dir_all_owner_only(&checked).map_err(|source| {
                DataDirError::CreateFailed {
                    path: path.to_path_buf(),
                    source,
                }
            })
        },
        Err(source) => Err(DataDirError::NotAccessible {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn session_meta_record(session: &Session, tools: Vec<String>) -> SessionRecord {
    SessionRecord::SessionMeta {
        format_version: CURRENT_FORMAT_VERSION,
        session_id: session.id.to_string(),
        timestamp: chrono::Utc::now(),
        working_directory: session.working_dir.clone(),
        model: session.model.clone(),
        model_config: session.model_config.clone(),
        provider_identity: session.provider_identity.clone().map(Box::new),
        tools,
        cake_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        build_commit: crate::build_info::commit(),
        build_dirty: crate::build_info::dirty(),
        system_prompt: session.system_prompt.clone(),
        git: session
            .git
            .clone()
            .or_else(|| git_state(&session.working_dir))
            .unwrap_or_default(),
    }
}

fn git_state(working_dir: &Path) -> Option<GitState> {
    let commit_hash = git_output(working_dir, &["rev-parse", "HEAD"])?;
    Some(GitState {
        repository_url: git_output(working_dir, &["config", "--get", "remote.origin.url"]),
        branch: git_output(working_dir, &["branch", "--show-current"]),
        commit_hash: Some(commit_hash),
    })
}

fn git_output(working_dir: &Path, args: &[&str]) -> Option<String> {
    let output = git::command(working_dir).args(args).output().ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    let trimmed = stdout.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}
