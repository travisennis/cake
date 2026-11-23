use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Read, Seek, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use crate::config::{
    BlobStore, owner_only,
    session_jsonl::SessionFramer,
    session_paths::{validate_session_parent, validate_session_target},
};

use anyhow::Context;
use fs4::{FileExt, TryLockError};

use crate::types::ProviderIdentity;
use crate::types::{
    CompactionData, ContextIdentity, ContextProvenance, ContextSeed, GitState, SessionRecord,
    SessionRunConfig, TurnUsageData, Usage,
};

/// Shared writer that holds the locked append handle for a session JSONL file.
///
/// Both the agent persistence callback and `HookRunner` clone a `SessionWriter`
/// so all writers append through the same file handle and advisory lock,
/// avoiding lock contention between hook records and conversation records.
///
/// The inner `File` is wrapped in a `BufWriter` so that each JSONL record
/// serialized by `serde_json::to_writer` issues a single `write()` syscall
/// instead of many small fragment writes.
///
/// A checkpoint commit is durable: it holds the same mutex through
/// serialization, newline, flush, and `File::sync_data`. Any append, flush, or
/// sync failure moves the shared state terminally to failed while the mutex is
/// held, so every clone rejects later writes instead of appending to a file
/// whose tail is unknown. The failed writer detaches its buffer without a
/// drop-time flush and keeps the locked file until every owner drops.
#[derive(Clone)]
pub struct SessionWriter {
    inner: Arc<Mutex<WriterState>>,
    /// The owning session's payload store. Every non-checkpoint append
    /// externalizes a clone of the record against this store before writing.
    blobs: Arc<BlobStore>,
}

struct WriterState {
    /// The buffered append handle, present until a write fails terminally.
    buffer: Option<BufWriter<File>>,
    /// The advisory-locked handle retained after a terminal failure, so the
    /// lock outlives every clone's ability to append.
    locked_file: Option<File>,
    /// The first append/flush/sync failure, preserved as the primary cause
    /// later writers are rejected with.
    failure: Option<String>,
}

impl WriterState {
    /// Borrow the append handle, rejecting a closed or terminally failed writer.
    fn open_buffer(&mut self) -> anyhow::Result<&mut BufWriter<File>> {
        if let Some(cause) = &self.failure {
            anyhow::bail!(
                "Session writer is permanently failed after an earlier write error: {cause}"
            );
        }
        self.buffer
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Session writer is already closed"))
    }

    /// Move to the terminal failed state without flushing unwritten bytes.
    fn fail(&mut self, error: &anyhow::Error) {
        self.failure.get_or_insert_with(|| format!("{error:#}"));
        if let Some(buffer) = self.buffer.take() {
            let (file, unwritten) = buffer.into_parts();
            drop(unwritten);
            self.locked_file = Some(file);
        }
    }
}

impl SessionWriter {
    /// Create a writer that owns the locked append handle and payload store.
    pub fn new(file: File, blobs: Arc<BlobStore>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(WriterState {
                buffer: Some(BufWriter::new(file)),
                locked_file: None,
                failure: None,
            })),
            blobs,
        }
    }

    /// Append a single record using the shared file handle.
    ///
    /// A `compaction` record routes through [`Self::commit_checkpoint`] so no
    /// caller can persist a checkpoint without its durability and validation
    /// semantics. Every other record is cloned and externalized inside the
    /// write critical section, so a blob failure terminally fails the writer
    /// instead of landing a record whose payload is missing.
    pub fn append_record(&self, record: &SessionRecord) -> anyhow::Result<()> {
        match record {
            SessionRecord::Compaction(checkpoint) => self.commit_checkpoint(checkpoint),
            record => self.write(|buffer| {
                let externalized = self.blobs.externalize_record(record)?;
                Session::append_record(buffer, &externalized)
            }),
        }
    }

    /// Append one checkpoint and sync it before returning.
    ///
    /// Structural validation runs first and leaves the writer untouched on a
    /// caller error. The whole write then holds the shared mutex and the
    /// advisory lock through externalization, serialization, the newline, the
    /// flush, and `sync_data`. Retained payloads are cloned and externalized
    /// before [`CompactionData::to_record_bytes`] measures the record, so the
    /// ceiling applies to references rather than payload bytes. A blob,
    /// serialization, or size failure is an append failure that terminally
    /// fails every clone.
    pub fn commit_checkpoint(&self, checkpoint: &CompactionData) -> anyhow::Result<()> {
        checkpoint.validate_structure()?;
        self.write(|buffer| {
            let externalized = self.blobs.externalize_checkpoint(checkpoint)?;
            let bytes = externalized.to_record_bytes()?;
            buffer
                .write_all(&bytes)
                .context("Failed to write compaction checkpoint")?;
            buffer.flush().context("Failed to flush session file")?;
            buffer
                .get_ref()
                .sync_data()
                .context("Failed to sync session file")
        })
    }

    /// Run one append against the shared buffer, failing the writer on error.
    fn write(
        &self,
        append: impl FnOnce(&mut BufWriter<File>) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = {
            let buffer = state.open_buffer()?;
            append(buffer)
        };
        if let Err(error) = result {
            state.fail(&error);
            drop(state);
            return Err(error);
        }
        Ok(())
    }

    /// Return the preserved cause when the shared writer has failed terminally.
    ///
    /// Callers check this before any paid provider work so a terminal append
    /// failure cannot be hidden by a disabled feature or a later retry.
    pub fn failure(&self) -> Option<String> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .failure
            .clone()
    }

    /// Reject a terminal writer failure before another append or provider call.
    ///
    /// # Errors
    ///
    /// Returns the preserved first failure with the same cause every later
    /// append would report.
    pub fn ensure_healthy(&self) -> anyhow::Result<()> {
        if let Some(cause) = self.failure() {
            anyhow::bail!(
                "Session writer is permanently failed after an earlier write error: {cause}"
            );
        }
        Ok(())
    }
}

/// Session format version for append-only task event logs.
pub const CURRENT_FORMAT_VERSION: u32 = 4;
const SESSION_LOCK_RETRY_COUNT: usize = 2;
const SESSION_LOCK_RETRY_DELAY: Duration = Duration::from_millis(10);

/// Initial window, in bytes, of the backwards scan that finds the last record
/// boundary of a session file. It doubles until a newline or the file start.
const SESSION_TAIL_WINDOW_BYTES: u64 = 64 * 1024;

/// In-memory session state reconstructed from a JSONL file.
///
/// A session represents a conversation with the AI, including its unique ID,
/// working directory, model used, and full record history.
///
/// # Examples
///
/// ```ignore
/// let session = Session::new(uuid::Uuid::new_v4(), PathBuf::from("/project"));
/// assert!(session.records.is_empty());
/// ```
#[derive(Debug, Clone)]
pub struct Session {
    /// Unique session identifier (UUID v4)
    pub id: uuid::Uuid,
    /// Working directory where session was created
    pub working_dir: PathBuf,
    /// Provider model ID used for the session (e.g. `gpt-5.6-luna`).
    pub model: Option<String>,
    /// The `[[models]]` entry name the session was created with
    /// (e.g. `codex-luna-xhigh`). Preferred resume identity: the model
    /// ID alone is ambiguous when entries share one ID.
    pub model_config: Option<String>,
    /// Sanitized provider destination pinned when the session was created.
    /// Absent on sessions written before provider identity was recorded.
    pub provider_identity: Option<ProviderIdentity>,
    /// Full system prompt used when the session was created.
    pub system_prompt: Option<String>,
    /// Run choices stored with the session and reapplied on resume.
    pub run_config: Option<SessionRunConfig>,
    /// Sanitized context identity captured when the session was created.
    pub context_identity: Option<ContextIdentity>,
    /// Git repository state captured when the session was created.
    pub git: Option<GitState>,
    /// Full record history (`SessionMeta`, task boundaries, messages, tool calls, etc.)
    pub records: Vec<SessionRecord>,
}

impl Session {
    /// Creates a new empty session.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let id = uuid::Uuid::new_v4();
    /// let session = Session::new(id, PathBuf::from("/project"));
    /// assert_eq!(session.id, id);
    /// ```
    pub const fn new(id: uuid::Uuid, working_dir: PathBuf) -> Self {
        Self {
            id,
            working_dir,
            model: None,
            model_config: None,
            provider_identity: None,
            system_prompt: None,
            run_config: None,
            context_identity: None,
            git: None,
            records: Vec::new(),
        }
    }

    /// Returns the full raw conversation and the latest checkpoint's boundary.
    ///
    /// Restore uses this seed, so the request projector can replace every
    /// conversation item before the latest checkpoint record's position. The boundary is a count
    /// of conversation items, derived by scanning records in order, so it stays
    /// stable across resume and fork without storing an index.
    pub fn context_seed(&self) -> ContextSeed {
        let latest = self.latest_checkpoint_index();
        let mut seed = ContextSeed::default();
        for (index, record) in self.records.iter().enumerate() {
            match record {
                SessionRecord::Compaction(checkpoint) if latest == Some(index) => {
                    seed.checkpoint = Some(checkpoint.clone());
                    seed.boundary = Some(seed.messages.len());
                },
                SessionRecord::Compaction(_) => {},
                record => {
                    if let Some(item) = record.to_conversation_item() {
                        let replay = match record {
                            SessionRecord::FunctionCall(data) => data.replay,
                            SessionRecord::FunctionCallOutput(data) => data.replay,
                            _ => None,
                        };
                        seed.messages.push(item);
                        seed.provenance.push(ContextProvenance { replay });
                    }
                },
            }
        }
        seed
    }

    /// Returns the record index of the latest `compaction` checkpoint.
    ///
    /// Restore, fork seeding, and [`Self::context_seed`] all select the newest
    /// checkpoint through this one rule, so they cannot disagree about which
    /// summary is active.
    pub fn latest_checkpoint_index(&self) -> Option<usize> {
        self.records
            .iter()
            .rposition(|record| matches!(record, SessionRecord::Compaction(_)))
    }

    /// Returns the usage of the most recent provider attempt that reported it
    /// after the newest checkpoint.
    ///
    /// Scans from the end of the record history so later task-boundary
    /// records (for example `TaskComplete`) do not hide the last usage audit.
    /// A checkpoint starts a new projected context, so usage written before it
    /// describes retired history and never seeds a resume. Lets a resumed
    /// session know its current context size before the first new provider
    /// request.
    pub fn last_turn_usage(&self) -> Option<Usage> {
        self.records
            .iter()
            .rev()
            .take_while(|record| !matches!(record, SessionRecord::Compaction(_)))
            .find_map(|record| match record {
                SessionRecord::TurnUsage(TurnUsageData {
                    usage,
                    purpose: None | Some(crate::types::TurnUsagePurpose::Main),
                    usage_presence,
                    ..
                }) if !matches!(
                    usage_presence,
                    Some(
                        crate::types::UsagePresence::Partial
                            | crate::types::UsagePresence::Unreported
                    )
                ) =>
                {
                    Some(*usage)
                },
                _ => None,
            })
    }

    /// Returns the names of skills activated during this session.
    pub fn activated_skills(&self) -> HashSet<String> {
        self.records
            .iter()
            .filter_map(|record| match record {
                SessionRecord::SkillActivated { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    /// Returns the MCP tool names activated by `ToolSearch` in this session.
    ///
    /// Records are additive, so the union of every activation batch is the
    /// current set. Resume and fork revalidate each name against the live MCP
    /// catalog and the current tool allowlist before registering it.
    pub fn activated_mcp_tools(&self) -> HashSet<String> {
        self.records
            .iter()
            .flat_map(|record| match record {
                SessionRecord::McpToolsActivated { tools, .. } => tools.as_slice(),
                _ => &[],
            })
            .cloned()
            .collect()
    }

    /// Loads a v4 append-only session from a JSONL file as raw records.
    ///
    /// Payload references are not resolved here. Callers that build provider
    /// input must hydrate through [`crate::config::DataDir`] or
    /// [`crate::config::BlobStore::hydrate_session`] first.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened, or if any line
    /// cannot be parsed as valid JSON.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let file = fs::File::open(path)
            .with_context(|| format!("Failed to open session file: {}", path.display()))?;
        Self::read_from(BufReader::new(file), path)
    }

    /// Load a session through a file already opened for append as raw records.
    ///
    /// [`Self::load`] opens its own handle. A caller that must lock first ---
    /// such as the checkpoint append command --- uses this instead, so the
    /// identity check, the derived boundary, and the append all happen under
    /// one advisory lock without opening a second handle. The handle is rewound
    /// to the start of the file before it is read. Payload references are not
    /// resolved here; provider-bound callers must hydrate first.
    pub fn load_from_append_handle(file: &File, path: &Path) -> anyhow::Result<Self> {
        let mut reader = file;
        reader
            .seek(std::io::SeekFrom::Start(0))
            .with_context(|| format!("Failed to read session file: {}", path.display()))?;
        Self::read_from(BufReader::new(reader), path)
    }

    /// Read a session from an open stream, the shared body of [`Self::load`]
    /// and [`Self::load_from_append_handle`].
    fn read_from(reader: impl BufRead, path: &Path) -> anyhow::Result<Self> {
        let mut framer = SessionFramer::new(reader);
        let (meta, header) = read_session_header(&mut framer, path)?;
        let records = read_session_records(&mut framer, path, header.timestamp, meta, header.id)?;

        Ok(Self {
            id: header.id,
            working_dir: header.working_dir,
            model: header.model,
            model_config: header.model_config,
            provider_identity: header.provider_identity,
            system_prompt: header.system_prompt,
            run_config: header.run_config,
            context_identity: header.context_identity,
            git: header.git,
            records,
        })
    }

    /// Create a new session file, write its metadata record, and return a locked append handle.
    ///
    /// The session file and any directory created for it are owner-only on
    /// Unix, independent of the process umask. A symlinked or non-directory
    /// parent, or a symlinked final component, is rejected.
    pub fn create_on_disk(path: &Path, meta: &SessionRecord) -> anyhow::Result<File> {
        if !matches!(meta, SessionRecord::SessionMeta { .. }) {
            anyhow::bail!("First record in a new session file must be session_meta");
        }
        validate_session_parent(path, true, "create")?;
        validate_session_target(path, "create")?;

        let mut options = OpenOptions::new();
        options.read(true).append(true).create_new(true);
        owner_only::apply_session_options(&mut options);
        let mut file = options
            .open(path)
            .with_context(|| format!("Failed to create session file: {}", path.display()))?;
        owner_only::set_file_owner_only(&file).with_context(|| {
            format!(
                "Failed to create session file '{}': cannot set owner-only mode {:04o}",
                path.display(),
                owner_only::OWNER_ONLY_FILE_MODE
            )
        })?;
        lock_session_file(&file, path)?;
        Self::append_record(&mut file, meta)?;
        Ok(file)
    }

    /// Open an existing session file for append and acquire an advisory lock.
    ///
    /// The final component must be a regular file, not a symlink, and its
    /// parent must be a real directory. On Unix a broader file mode is
    /// tightened to `0600` before the caller can write. An incomplete final
    /// record left by an interrupted writer is repaired under the lock after
    /// the mode check, so the next append starts at a record boundary. See
    /// [`repair_partial_tail`].
    pub fn open_for_append(path: &Path) -> anyhow::Result<File> {
        validate_session_parent(path, false, "open")?;
        validate_session_target(path, "open")?;

        let mut options = OpenOptions::new();
        options.read(true).append(true);
        owner_only::apply_session_options(&mut options);
        let file = options
            .open(path)
            .with_context(|| format!("Failed to open session file: {}", path.display()))?;
        if !file
            .metadata()
            .with_context(|| format!("Failed to inspect session file: {}", path.display()))?
            .is_file()
        {
            anyhow::bail!(
                "Failed to open session file '{}': path is not a regular file (required mode {:04o})",
                path.display(),
                owner_only::OWNER_ONLY_FILE_MODE
            );
        }
        lock_session_file(&file, path)?;
        owner_only::set_file_owner_only(&file).with_context(|| {
            format!(
                "Failed to open session file '{}': cannot set owner-only mode {:04o}",
                path.display(),
                owner_only::OWNER_ONLY_FILE_MODE
            )
        })?;
        repair_partial_tail(&file, path).map(|()| file)
    }

    /// Append one JSONL session record and flush it to disk.
    ///
    /// Accepts any `Write` implementor so callers can pass a `File` directly
    /// (e.g., during session creation) or a `BufWriter<File>` (shared writer).
    pub fn append_record(file: &mut impl Write, record: &SessionRecord) -> anyhow::Result<()> {
        serde_json::to_writer(&mut *file, record).context("Failed to serialize session record")?;
        file.write_all(b"\n").context("Failed to write newline")?;
        file.flush().context("Failed to flush session file")
    }

    /// Append multiple JSONL session records.
    pub fn append_records(file: &mut impl Write, records: &[SessionRecord]) -> anyhow::Result<()> {
        for record in records {
            Self::append_record(file, record)?;
        }
        Ok(())
    }
}

/// Restore a record boundary in a session file whose last write was interrupted.
///
/// [`Session::load`] tolerates an incomplete final record, but the file itself
/// stays unframed: the next append would concatenate its record onto the
/// partial bytes and leave a malformed interior line that no later load can
/// read. This runs under the writer lock, before the append handle is used, and
/// it touches only the unterminated final line:
///
/// - A final line that is already a complete JSON object lost only its
///   terminating newline, so the newline is appended and the record is kept.
/// - Any other unterminated tail is an incomplete fragment and is truncated.
///
/// Complete records are never modified, and a file that already ends at a
/// record boundary is left byte-for-byte unchanged. A malformed line that is
/// terminated by a newline, wherever it appears, is never treated as a
/// recoverable tail: the repair cannot tell where the writer meant to stop, so
/// loading reports it instead.
fn repair_partial_tail(file: &File, path: &Path) -> anyhow::Result<()> {
    let len = file
        .metadata()
        .with_context(|| format!("Failed to inspect session file: {}", path.display()))?
        .len();
    let Some(tail) = read_unterminated_tail(file, len)
        .with_context(|| format!("Failed to read session file: {}", path.display()))?
    else {
        return Ok(());
    };

    let tail_len = tail.len() as u64;
    let is_complete_record =
        serde_json::from_slice::<serde_json::Value>(&tail).is_ok_and(|value| value.is_object());

    let mut handle = file;
    if is_complete_record {
        tracing::warn!(
            "Session file {} ends with a complete record missing its trailing newline; terminating the record",
            path.display()
        );
        return handle
            .write_all(b"\n")
            .with_context(|| format!("Failed to repair session file: {}", path.display()));
    }

    if tail_len == len {
        anyhow::bail!(
            "Session file {} has no complete record to recover; the first record was never written",
            path.display()
        );
    }

    tracing::warn!(
        "Truncating {tail_len} incomplete byte(s) from the final record of session file {}",
        path.display()
    );
    handle
        .set_len(len - tail_len)
        .with_context(|| format!("Failed to repair session file: {}", path.display()))
}

/// Read the bytes after the last newline of a file, or `None` when the file is
/// empty or already ends at a record boundary.
///
/// The scan reads backwards in windows, doubling the window until it reaches a
/// newline or the start of the file, so it does not hold a whole large session
/// in memory to inspect its last record.
fn read_unterminated_tail(file: &File, len: u64) -> std::io::Result<Option<Vec<u8>>> {
    if len == 0 {
        return Ok(None);
    }

    let mut window = len.min(SESSION_TAIL_WINDOW_BYTES);
    loop {
        let start = len - window;
        let mut buffer = vec![0; usize::try_from(window).unwrap_or(usize::MAX)];
        let mut handle = file;
        handle.seek(std::io::SeekFrom::Start(start))?;
        handle.read_exact(&mut buffer)?;

        if let Some(last_newline) = buffer.iter().rposition(|byte| *byte == b'\n') {
            buffer.drain(..=last_newline);
            return Ok((!buffer.is_empty()).then_some(buffer));
        }
        if start == 0 {
            return Ok(Some(buffer));
        }
        window = window.saturating_mul(2).min(len);
    }
}

struct SessionHeader {
    id: uuid::Uuid,
    working_dir: PathBuf,
    model: Option<String>,
    model_config: Option<String>,
    provider_identity: Option<ProviderIdentity>,
    system_prompt: Option<String>,
    run_config: Option<SessionRunConfig>,
    context_identity: Option<ContextIdentity>,
    git: Option<GitState>,
    timestamp: chrono::DateTime<chrono::Utc>,
}

fn read_session_header<R: std::io::BufRead>(
    framer: &mut SessionFramer<R>,
    path: &Path,
) -> anyhow::Result<(SessionRecord, SessionHeader)> {
    let first_line = framer
        .next_record()
        .with_context(|| format!("Failed to read session file: {}", path.display()))?
        .ok_or_else(|| anyhow::anyhow!("Session file is empty"))?
        .text;

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&first_line)
        && value.get("type").and_then(|value| value.as_str()) != Some("session_meta")
    {
        anyhow::bail!(
            "Unsupported or legacy session file format: expected session_meta as first record in {}",
            path.display()
        );
    }

    let meta: SessionRecord = serde_json::from_str(&first_line).with_context(|| {
        format!(
            "Failed to parse session_meta record of session file: {}",
            path.display()
        )
    })?;

    let header = match &meta {
        SessionRecord::SessionMeta {
            format_version,
            session_id,
            timestamp,
            working_directory,
            model,
            model_config,
            provider_identity,
            system_prompt,
            run_config,
            context_identity,
            git,
            ..
        } => {
            if *format_version != CURRENT_FORMAT_VERSION {
                anyhow::bail!(
                    "Unsupported session format_version: {} (expected {}). Session file: {}",
                    format_version,
                    CURRENT_FORMAT_VERSION,
                    path.display()
                );
            }
            SessionHeader {
                id: uuid::Uuid::parse_str(session_id)
                    .with_context(|| format!("Invalid session UUID '{session_id}'"))?,
                working_dir: working_directory.clone(),
                model: model.clone(),
                model_config: model_config.clone(),
                provider_identity: provider_identity.as_deref().cloned(),
                system_prompt: system_prompt.clone(),
                run_config: run_config.clone(),
                context_identity: context_identity.as_deref().cloned(),
                git: Some(git.clone()),
                timestamp: *timestamp,
            }
        },
        _ => {
            return Err(anyhow::anyhow!(
                "Unsupported or legacy session file format: expected session_meta as first record in {}",
                path.display()
            ));
        },
    };

    Ok((meta, header))
}

fn read_session_records<R: std::io::BufRead>(
    framer: &mut SessionFramer<R>,
    path: &Path,
    session_timestamp: chrono::DateTime<chrono::Utc>,
    meta: SessionRecord,
    session_id: uuid::Uuid,
) -> anyhow::Result<Vec<SessionRecord>> {
    let mut records = vec![meta];
    let session_id = session_id.to_string();

    while let Some(line) = framer
        .next_record()
        .with_context(|| format!("Failed to read session file: {}", path.display()))?
    {
        match serde_json::from_str::<SessionRecord>(&line.text) {
            Ok(mut record) => {
                record.normalize_legacy_fields(session_timestamp);
                if let SessionRecord::Compaction(checkpoint) = &record {
                    ensure_checkpoint_matches_session(checkpoint, &session_id).with_context(
                        || {
                            format!(
                                "Failed to validate compaction checkpoint in session file: {}",
                                path.display()
                            )
                        },
                    )?;
                }
                records.push(record);
            },
            Err(error) if line.partial_tail => {
                tracing::warn!(
                    "Ignoring partial final session record in {}: {}",
                    path.display(),
                    error
                );
                break;
            },
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "Failed to parse line {} of session file: {}",
                        line.line_number,
                        path.display()
                    )
                });
            },
        }
    }

    Ok(records)
}

/// Reject a checkpoint recorded for a different session.
///
/// Fork seeding must re-envelope the copied checkpoint for the new session.
/// A checkpoint that still names the source session means that step was
/// skipped, so loading fails loudly instead of projecting another session's
/// summary.
pub fn ensure_checkpoint_matches_session(
    checkpoint: &CompactionData,
    session_id: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        checkpoint.session_id == session_id,
        "Compaction checkpoint '{}' belongs to session {}, not {session_id}",
        checkpoint.checkpoint_id,
        checkpoint.session_id
    );
    Ok(())
}

fn lock_session_file(file: &File, path: &Path) -> anyhow::Result<()> {
    if try_lock_session_file_with_retries(file, path)? {
        return Ok(());
    }

    Err(session_lock_conflict_error(path))
}

fn try_lock_session_file_with_retries(file: &File, path: &Path) -> anyhow::Result<bool> {
    for _ in 0..SESSION_LOCK_RETRY_COUNT {
        if try_lock_session_file(file, path)? {
            return Ok(true);
        }
        thread::sleep(SESSION_LOCK_RETRY_DELAY);
    }

    try_lock_session_file(file, path)
}

/// The result of a non-blocking check for an active session writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionLiveness {
    /// Another process holds the session's append lock.
    Live,
    /// The lock is free, so no writer is currently appending.
    NotLive,
    /// The file could not be opened or its lock state could not be inspected.
    Unknown,
}

/// Probe whether a session file is held by an active writer.
///
/// The probe opens the file read-only and makes one non-blocking advisory lock
/// attempt. The lock is released as soon as the probe returns. A missing file,
/// unreadable file, or unsupported lock operation reports `Unknown` rather than
/// waiting or mutating the session.
pub fn probe_session_liveness(path: &Path) -> SessionLiveness {
    let Ok(file) = fs::File::open(path) else {
        return SessionLiveness::Unknown;
    };
    match FileExt::try_lock(&file) {
        Ok(()) => SessionLiveness::NotLive,
        Err(TryLockError::WouldBlock) => SessionLiveness::Live,
        Err(TryLockError::Error(_)) => SessionLiveness::Unknown,
    }
}

/// The session file is locked by another cake invocation.
///
/// This is an operational conflict rather than an input error: the caller's
/// input is valid and can succeed once the other invocation finishes. Callers
/// that report structured failures downcast this type instead of matching the
/// message.
#[derive(Debug, thiserror::Error)]
#[error(
    "Another cake invocation is currently writing to session {session_id}. Wait for it to finish or run in a different directory."
)]
pub struct SessionLockedError {
    /// Session id from the session file name, or `<unknown>`.
    pub session_id: String,
}

fn session_lock_conflict_error(path: &Path) -> anyhow::Error {
    let session_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("<unknown>")
        .to_string();
    SessionLockedError { session_id }.into()
}

fn try_lock_session_file(file: &File, path: &Path) -> anyhow::Result<bool> {
    match FileExt::try_lock(file) {
        Ok(()) => Ok(true),
        Err(TryLockError::WouldBlock) => Ok(false),
        Err(TryLockError::Error(error)) => {
            Err(error).with_context(|| format!("Failed to lock session file: {}", path.display()))
        },
    }
}
