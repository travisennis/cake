use std::{
    fs::{self, File},
    io::BufReader,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::config::session_jsonl::SessionFramer;
use crate::config::{Session, git, session::CURRENT_FORMAT_VERSION};
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
    /// # Examples
    ///
    /// ```ignore
    /// let data_dir = DataDir::new()?;
    /// ```
    ///
    /// Create a `DataDir` at an arbitrary path (for testing).
    ///
    /// This avoids filesystem access and environment dependencies,
    /// making it suitable for unit tests that need a controlled directory layout.
    #[cfg(test)]
    pub fn new_in_dir(dir: &Path) -> Self {
        Self {
            data_dir: dir.to_path_buf(),
            sessions_dir: dir.join("sessions"),
        }
    }

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

        ensure_dir(&data_dir)?;
        ensure_dir(&sessions_dir)?;

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
    /// under one root. The config directory is deliberately excluded: it holds
    /// credentials and trusted executables. See `docs/security.md`.
    #[must_use]
    pub fn sandbox_read_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.data_dir.clone()];
        if self.sessions_dir != self.data_dir {
            dirs.push(self.sessions_dir.clone());
        }
        dirs
    }

    /// Saves a session to disk as a new JSONL file.
    ///
    /// The session is saved to `~/.local/share/cake/sessions/{session_id}.jsonl`.
    /// The most recent session is determined by the session creation timestamp
    /// in the first JSONL record (no symlink needed).
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let data_dir = DataDir::new()?;
    /// let session = Session::new(uuid::Uuid::new_v4(), PathBuf::from("/project"));
    /// data_dir.save_session(&session)?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if the session file cannot be written.
    #[cfg(test)]
    pub fn save_session(&self, session: &Session) -> anyhow::Result<PathBuf> {
        let session_path = self.session_path(session.id);

        tracing::info!(target: "cake", "Saving session {} to {}", session.id, session_path.display());

        let meta = session_meta_record(session, Vec::new());
        let mut file = Session::create_on_disk(&session_path, &meta)?;
        Session::append_records(&mut file, &session.records)?;

        Ok(session_path)
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
            .map(|(path, _)| Session::load(&path))
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

        Session::load(&session_path).map(Some)
    }
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

/// Ensures `path` exists, distinguishing a denied stat from a missing path.
///
/// `Path::exists()` reports every stat error as "missing", so under a sandbox
/// that denies stat on an existing directory the previous
/// `if !path.exists() { create_dir_all(path) }` flow called `mkdir` on the
/// existing path and surfaced a misleading `AlreadyExists (os error 17)`.
/// Here a stat failure that is not `NotFound` reports the path as
/// inaccessible instead of attempting creation.
fn ensure_dir(path: &Path) -> Result<(), DataDirError> {
    match fs::metadata(path) {
        Ok(_) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => fs::create_dir_all(path)
            .map_err(|source| DataDirError::CreateFailed {
                path: path.to_path_buf(),
                source,
            }),
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
        tools,
        cake_version: Some(env!("CARGO_PKG_VERSION").to_string()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::git::test_support::{init_repo, run_git};
    use tempfile::TempDir;

    fn test_data_dir() -> (DataDir, TempDir) {
        let tmp = TempDir::new().unwrap();
        let dd = DataDir {
            data_dir: tmp.path().to_path_buf(),
            sessions_dir: tmp.path().join("sessions"),
        };
        (dd, tmp)
    }

    #[test]
    fn sandbox_read_dirs_returns_cache_and_sessions() {
        let (dd, _tmp) = test_data_dir();
        assert_eq!(
            dd.sandbox_read_dirs(),
            vec![dd.get_cache_dir(), dd.sessions_dir()]
        );
    }

    #[test]
    fn sandbox_read_dirs_collapses_when_data_dir_covers_sessions() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let dd = DataDir {
            data_dir: root.clone(),
            sessions_dir: root.clone(),
        };
        assert_eq!(dd.sandbox_read_dirs(), vec![root]);
    }

    #[test]
    fn save_and_load_session_round_trip() {
        let (dd, _tmp) = test_data_dir();
        let mut session = Session::new(uuid::Uuid::new_v4(), PathBuf::from("/work"));
        session.system_prompt = Some("full system prompt".to_string());
        session.git = Some(GitState {
            repository_url: Some("https://example.com/cake.git".to_string()),
            branch: Some("main".to_string()),
            commit_hash: Some("abc123".to_string()),
        });
        dd.save_session(&session).unwrap();
        let loaded = dd.load_session(session.id).unwrap().unwrap();
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.working_dir, session.working_dir);

        let first_line = fs::read_to_string(dd.session_path(session.id))
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_string();
        let meta: serde_json::Value = serde_json::from_str(&first_line).unwrap();
        assert_eq!(meta["system_prompt"], "full system prompt");
        assert_eq!(
            meta["git"]["repository_url"],
            "https://example.com/cake.git"
        );
        assert_eq!(meta["git"]["branch"], "main");
        assert_eq!(meta["git"]["commit_hash"], "abc123");
    }

    #[test]
    fn session_meta_includes_empty_git_outside_repository() {
        let (dd, tmp) = test_data_dir();
        let mut session = Session::new(uuid::Uuid::new_v4(), tmp.path().join("not-a-repo"));
        session.system_prompt = Some("full system prompt".to_string());
        fs::create_dir_all(&session.working_dir).unwrap();

        dd.save_session(&session).unwrap();

        let first_line = fs::read_to_string(dd.session_path(session.id))
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_string();
        let meta: serde_json::Value = serde_json::from_str(&first_line).unwrap();
        assert_eq!(meta["system_prompt"], "full system prompt");
        assert!(meta["git"].is_object());
        assert!(meta["git"]["repository_url"].is_null());
        assert!(meta["git"]["branch"].is_null());
        assert!(meta["git"]["commit_hash"].is_null());
    }

    #[test]
    fn git_state_captures_repository_url_branch_and_commit() {
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        run_git(
            repo.path(),
            &["remote", "add", "origin", "https://example.com/cake.git"],
        );
        run_git(repo.path(), &["checkout", "-b", "feature/session-meta"]);

        let expected_commit = git_output(repo.path(), &["rev-parse", "HEAD"]).unwrap();
        let state = git_state(repo.path()).unwrap();

        assert_eq!(
            state.repository_url.as_deref(),
            Some("https://example.com/cake.git")
        );
        assert_eq!(state.branch.as_deref(), Some("feature/session-meta"));
        assert_eq!(state.commit_hash.as_deref(), Some(expected_commit.as_str()));
    }

    #[test]
    fn git_state_ignores_an_inherited_git_dir() {
        // Regression: session metadata captured the repository pinned by an
        // inherited GIT_DIR rather than the session's working directory.
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        run_git(repo.path(), &["checkout", "-b", "feature/hermetic"]);

        let elsewhere = TempDir::new().unwrap();
        init_repo(elsewhere.path());
        let poison = fs::canonicalize(elsewhere.path().join(".git")).unwrap();

        let state =
            temp_env::with_var("GIT_DIR", Some(&poison), || git_state(repo.path())).unwrap();

        assert_eq!(state.branch.as_deref(), Some("feature/hermetic"));
    }

    #[test]
    fn discovery_skips_leading_empty_lines() {
        // Regression for #275: a blank line before `session_meta` must not hide
        // the header during latest-session discovery, matching full load,
        // replay, and listing.
        let (dd, _tmp) = test_data_dir();
        let session = Session::new(uuid::Uuid::new_v4(), PathBuf::from("/work"));
        dd.save_session(&session).unwrap();
        let path = dd.session_path(session.id);
        let content = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("\n\n{content}")).unwrap();

        let latest = dd
            .load_latest_session(&PathBuf::from("/work"))
            .unwrap()
            .unwrap();
        assert_eq!(latest.id, session.id);
    }

    #[test]
    fn save_and_load_latest_session() {
        let (dd, _tmp) = test_data_dir();
        let session = Session::new(uuid::Uuid::new_v4(), PathBuf::from("/work"));
        dd.save_session(&session).unwrap();
        let latest = dd
            .load_latest_session(&PathBuf::from("/work"))
            .unwrap()
            .unwrap();
        assert_eq!(latest.id, session.id);
    }

    #[test]
    fn load_session_missing_returns_none() {
        let (dd, _tmp) = test_data_dir();
        let result = dd.load_session(uuid::Uuid::new_v4()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_latest_session_missing_returns_none() {
        let (dd, _tmp) = test_data_dir();
        let result = dd
            .load_latest_session(&PathBuf::from("/nonexistent"))
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn multiple_sessions_same_working_dir() {
        let (dd, _tmp) = test_data_dir();
        let working_dir = PathBuf::from("/work");

        let session1 = Session::new(uuid::Uuid::new_v4(), working_dir.clone());
        let session2 = Session::new(uuid::Uuid::new_v4(), working_dir.clone());

        dd.save_session(&session1).unwrap();
        dd.save_session(&session2).unwrap();

        // Both sessions should be loadable
        let loaded1 = dd.load_session(session1.id).unwrap().unwrap();
        let loaded2 = dd.load_session(session2.id).unwrap().unwrap();

        assert_eq!(loaded1.id, session1.id);
        assert_eq!(loaded2.id, session2.id);

        // Latest should be session2 (last saved)
        let latest = dd.load_latest_session(&working_dir).unwrap().unwrap();
        assert_eq!(latest.id, session2.id);
    }

    #[test]
    fn different_working_dirs_isolated() {
        let (dd, _tmp) = test_data_dir();

        let session1 = Session::new(uuid::Uuid::new_v4(), PathBuf::from("/work1"));
        let session2 = Session::new(uuid::Uuid::new_v4(), PathBuf::from("/work2"));

        dd.save_session(&session1).unwrap();
        dd.save_session(&session2).unwrap();

        // Each working dir should have its own latest
        let latest1 = dd
            .load_latest_session(&PathBuf::from("/work1"))
            .unwrap()
            .unwrap();
        let latest2 = dd
            .load_latest_session(&PathBuf::from("/work2"))
            .unwrap()
            .unwrap();

        assert_eq!(latest1.id, session1.id);
        assert_eq!(latest2.id, session2.id);
    }

    #[test]
    fn new_respects_cake_data_dir_env() {
        let tmp = TempDir::new().unwrap();
        let custom_path = tmp.path().join("custom_cake");

        // SAFETY: This test runs in a single-threaded context and no other code
        // reads CAKE_DATA_DIR concurrently.
        unsafe {
            std::env::set_var("CAKE_DATA_DIR", &custom_path);
        }
        let dd = DataDir::new().unwrap();
        // SAFETY: Restoring the environment for subsequent tests. Same safety
        // assumptions as the set_var call above.
        unsafe {
            std::env::remove_var("CAKE_DATA_DIR");
        }

        assert_eq!(dd.data_dir, custom_path);
        assert!(custom_path.exists());
    }

    #[test]
    fn ensure_dir_accepts_existing_directory() {
        let tmp = TempDir::new().unwrap();
        ensure_dir(tmp.path()).unwrap();
    }

    #[test]
    fn ensure_dir_creates_missing_directory() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("nested/data");

        ensure_dir(&missing).unwrap();

        assert!(missing.is_dir());
    }

    #[test]
    #[cfg(unix)]
    fn ensure_dir_reports_inaccessible_directory() {
        use std::os::unix::fs::PermissionsExt;

        // Root ignores permission bits, so the denial cannot be simulated.
        // SAFETY: geteuid is always safe to call.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }

        // A parent with no permission bits denies stat on the child: the
        // sandbox-denial shape that used to surface as
        // `File exists (os error 17)` from `create_dir_all`.
        let tmp = TempDir::new().unwrap();
        let sealed = tmp.path().join("sealed");
        std::fs::create_dir(&sealed).unwrap();
        let mut perms = std::fs::metadata(&sealed).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&sealed, perms).unwrap();
        let child = sealed.join("child");

        let err = ensure_dir(&child).unwrap_err();

        // Restore before the TempDir drops so cleanup can unlink the child.
        let mut perms = std::fs::metadata(&sealed).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&sealed, perms).unwrap();

        match &err {
            DataDirError::NotAccessible { path, .. } => assert_eq!(path, &child),
            other @ DataDirError::CreateFailed { .. } => {
                panic!("expected NotAccessible, got {other:?}")
            },
        }
        let text = err.to_string();
        assert!(
            text.contains("not accessible"),
            "unexpected message: {text}"
        );
        assert!(text.contains("CAKE_DATA_DIR"), "unexpected message: {text}");
    }

    #[test]
    fn new_reports_inaccessible_data_dir_with_path_and_remedy() {
        use std::os::unix::fs::PermissionsExt;

        // Root ignores permission bits, so the denial cannot be simulated.
        // SAFETY: geteuid is always safe to call.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }

        let tmp = TempDir::new().unwrap();
        let sealed = tmp.path().join("sealed");
        std::fs::create_dir(&sealed).unwrap();
        let mut perms = std::fs::metadata(&sealed).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&sealed, perms).unwrap();
        let root = sealed.join("cake");

        let result = temp_env::with_var("CAKE_DATA_DIR", Some(root.as_os_str()), DataDir::new);

        // Restore before the TempDir drops so cleanup can unlink the child.
        let mut perms = std::fs::metadata(&sealed).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&sealed, perms).unwrap();

        let err = result.unwrap_err();
        assert!(
            err.downcast_ref::<DataDirError>().is_some(),
            "expected a typed DataDirError, got: {err:#}"
        );
        let text = format!("{err:#}");
        assert!(
            text.contains("is not accessible"),
            "unexpected message: {text}"
        );
        assert!(text.contains("cake"), "unexpected message: {text}");
        assert!(text.contains("CAKE_DATA_DIR"), "unexpected message: {text}");
    }

    #[test]
    fn data_dir_error_chain_names_path_remedy_and_cause() {
        let err: anyhow::Error = DataDirError::NotAccessible {
            path: PathBuf::from("/Users/me/.cache/cake"),
            source: std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "operation not permitted",
            ),
        }
        .into();

        let text = format!("{err:#}");
        assert!(text.contains("/Users/me/.cache/cake"), "unexpected: {text}");
        assert!(text.contains("not accessible"), "unexpected: {text}");
        assert!(text.contains("CAKE_DATA_DIR"), "unexpected: {text}");
        assert!(
            text.contains("operation not permitted"),
            "unexpected: {text}"
        );
        assert!(!text.contains("File exists"), "unexpected: {text}");
    }
}
