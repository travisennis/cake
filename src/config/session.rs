use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use crate::config::session_jsonl::SessionFramer;

use anyhow::Context;
use fs4::{FileExt, TryLockError};

use crate::types::{ConversationItem, GitState, SessionRecord, TurnUsageData, Usage};

/// Shared writer that holds the locked append handle for a session JSONL file.
///
/// Both the agent persistence callback and `HookRunner` clone a `SessionWriter`
/// so all writers append through the same file handle and advisory lock,
/// avoiding lock contention between hook records and conversation records.
///
/// The inner `File` is wrapped in a `BufWriter` so that each JSONL record
/// serialized by `serde_json::to_writer` issues a single `write()` syscall
/// instead of many small fragment writes.
#[derive(Clone)]
pub struct SessionWriter {
    file: Arc<Mutex<BufWriter<File>>>,
}

impl SessionWriter {
    /// Create a writer that owns the locked append handle.
    pub fn new(file: File) -> Self {
        Self {
            file: Arc::new(Mutex::new(BufWriter::new(file))),
        }
    }

    /// Append a single record using the shared file handle.
    pub fn append_record(&self, record: &SessionRecord) -> anyhow::Result<()> {
        let mut guard = self
            .file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Session::append_record(&mut *guard, record)
    }
}

/// Session format version for append-only task event logs.
pub const CURRENT_FORMAT_VERSION: u32 = 4;
const SESSION_LOCK_RETRY_COUNT: usize = 2;
const SESSION_LOCK_RETRY_DELAY: Duration = Duration::from_millis(10);

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
    /// Model used for the session
    pub model: Option<String>,
    /// Full system prompt used when the session was created.
    pub system_prompt: Option<String>,
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
            system_prompt: None,
            git: None,
            records: Vec::new(),
        }
    }

    /// Returns the conversation items from this session's records,
    /// filtering out session metadata and task boundary records.
    pub fn messages(&self) -> Vec<ConversationItem> {
        self.records
            .iter()
            .filter_map(SessionRecord::to_conversation_item)
            .collect()
    }

    /// Returns the usage of the most recent provider attempt that reported it.
    ///
    /// Scans from the end of the record history so later task-boundary
    /// records (for example `TaskComplete`) do not hide the last usage audit.
    /// Lets a resumed session know its current context size before the
    /// first new provider request.
    pub fn last_turn_usage(&self) -> Option<Usage> {
        self.records.iter().rev().find_map(|record| match record {
            SessionRecord::TurnUsage(TurnUsageData { usage, .. }) => Some(*usage),
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

    /// Loads a v4 append-only session from a JSONL file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened, or if any line
    /// cannot be parsed as valid JSON.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let file = fs::File::open(path)
            .with_context(|| format!("Failed to open session file: {}", path.display()))?;
        let mut framer = SessionFramer::new(BufReader::new(file));
        let (meta, header) = read_session_header(&mut framer, path)?;
        let records = read_session_records(&mut framer, path, header.timestamp, meta)?;

        Ok(Self {
            id: header.id,
            working_dir: header.working_dir,
            model: header.model,
            system_prompt: header.system_prompt,
            git: header.git,
            records,
        })
    }

    /// Create a new session file, write its metadata record, and return a locked append handle.
    pub fn create_on_disk(path: &Path, meta: &SessionRecord) -> anyhow::Result<File> {
        if !matches!(meta, SessionRecord::SessionMeta { .. }) {
            anyhow::bail!("First record in a new session file must be session_meta");
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create session directory: {}", parent.display())
            })?;
        }

        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create_new(true)
            .open(path)
            .with_context(|| format!("Failed to create session file: {}", path.display()))?;
        lock_session_file(&file, path)?;
        Self::append_record(&mut file, meta)?;
        Ok(file)
    }

    /// Open an existing session file for append and acquire an advisory lock.
    pub fn open_for_append(path: &Path) -> anyhow::Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .open(path)
            .with_context(|| format!("Failed to open session file: {}", path.display()))?;
        lock_session_file(&file, path)?;
        Ok(file)
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

struct SessionHeader {
    id: uuid::Uuid,
    working_dir: PathBuf,
    model: Option<String>,
    system_prompt: Option<String>,
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
            system_prompt,
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
                system_prompt: system_prompt.clone(),
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
) -> anyhow::Result<Vec<SessionRecord>> {
    let mut records = vec![meta];

    while let Some(line) = framer
        .next_record()
        .with_context(|| format!("Failed to read session file: {}", path.display()))?
    {
        match serde_json::from_str::<SessionRecord>(&line.text) {
            Ok(mut record) => {
                record.normalize_legacy_fields(session_timestamp);
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

fn session_lock_conflict_error(path: &Path) -> anyhow::Error {
    let id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("<unknown>");
    anyhow::anyhow!(
        "Another cake invocation is currently writing to session {id}. Wait for it to finish or run in a different directory."
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ReasoningSummary;
    use crate::types::session::{
        FunctionCallData, FunctionCallOutputData, MessageData, ReasoningData,
    };
    use crate::types::{
        ReasoningContent, ReasoningContentKind, Role, TaskCompleteData, TaskOutcome, TaskStartData,
        Usage,
    };
    use tempfile::TempDir;

    /// Helper to create a minimal v4 session for testing.
    fn make_test_session() -> Session {
        let id = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let mut session = Session::new(id, PathBuf::from("/work"));
        session.model = Some("test-model".to_string());
        session
    }

    fn meta_record(session: &Session) -> SessionRecord {
        SessionRecord::SessionMeta {
            format_version: CURRENT_FORMAT_VERSION,
            session_id: session.id.to_string(),
            timestamp: chrono::Utc::now(),
            working_directory: session.working_dir.clone(),
            model: session.model.clone(),
            tools: vec!["bash".to_string(), "read".to_string()],
            cake_version: Some("test".to_string()),
            system_prompt: Some("test system prompt".to_string()),
            git: GitState {
                repository_url: Some("https://example.com/repo.git".to_string()),
                branch: Some("main".to_string()),
                commit_hash: Some("abc123".to_string()),
            },
        }
    }

    fn task_start(session: &Session, task_id: &str) -> SessionRecord {
        SessionRecord::TaskStart(TaskStartData {
            session_id: session.id.to_string(),
            task_id: task_id.to_string(),
            timestamp: chrono::Utc::now(),
        })
    }

    fn task_complete(session: &Session, task_id: &str) -> SessionRecord {
        SessionRecord::TaskComplete(TaskCompleteData {
            outcome: TaskOutcome::Success {
                result: Some("Done".to_string()),
            },
            duration_ms: 100,
            turn_count: 1,
            tool_call_count: 0,
            session_id: session.id.to_string(),
            task_id: task_id.to_string(),
            usage: Usage::default(),
            permission_denials: None,
        })
    }

    fn turn_usage(session: &Session, task_id: &str, turn: u32) -> SessionRecord {
        SessionRecord::TurnUsage(TurnUsageData {
            session_id: session.id.to_string(),
            task_id: task_id.to_string(),
            turn,
            usage: Usage {
                input_tokens: u64::from(turn) * 1000,
                ..Usage::default()
            },
            timestamp: chrono::Utc::now(),
            attempt: None,
            terminal_class: None,
        })
    }

    #[test]
    fn last_turn_usage_returns_most_recent_turn_past_task_boundaries() {
        let mut session = make_test_session();
        session.records.push(meta_record(&session));
        session.records.push(task_start(&session, "task-1"));
        session.records.push(turn_usage(&session, "task-1", 1));
        session.records.push(turn_usage(&session, "task-1", 2));
        // A later task-boundary record must not hide the last per-turn usage.
        session.records.push(task_complete(&session, "task-1"));

        let last = session.last_turn_usage().unwrap();
        assert_eq!(last.input_tokens, 2000);
    }

    #[test]
    fn last_turn_usage_is_none_without_turn_usage_records() {
        let mut session = make_test_session();
        session.records.push(meta_record(&session));
        session.records.push(task_start(&session, "task-1"));
        session.records.push(task_complete(&session, "task-1"));

        assert!(session.last_turn_usage().is_none());
    }

    #[test]
    fn test_session_new_defaults() {
        let id = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440001").unwrap();
        let session = Session::new(id, PathBuf::from("/tmp/test"));
        assert_eq!(session.id, id);
        assert_eq!(session.working_dir, PathBuf::from("/tmp/test"));
        assert!(session.records.is_empty());
        assert!(session.model.is_none());
    }

    #[test]
    fn test_session_load_rejects_empty_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("empty.jsonl");
        fs::write(&path, "\n  \n").unwrap();

        let error = Session::load(&path).unwrap_err().to_string();
        assert!(error.contains("Session file is empty"));
    }

    #[test]
    fn test_session_load_rejects_invalid_session_uuid() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("invalid-uuid.jsonl");
        fs::write(
            &path,
            concat!(
                r#"{"type":"session_meta","format_version":4,"session_id":"not-a-uuid","timestamp":"2026-04-04T15:51:54Z","working_directory":"/tmp/test","tools":[],"git":{"repository_url":null,"branch":null,"commit_hash":null}}"#,
                "\n"
            ),
        )
        .unwrap();

        let error = Session::load(&path).unwrap_err().to_string();
        assert!(error.contains("Invalid session UUID 'not-a-uuid'"));
    }

    #[test]
    fn test_session_load_rejects_malformed_nonfinal_record() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("malformed.jsonl");
        let session = make_test_session();
        let header = serde_json::to_string(&meta_record(&session)).unwrap();
        fs::write(&path, format!("{header}\n{{\"type\":\"message\"\n")).unwrap();

        let error = Session::load(&path).unwrap_err().to_string();
        assert!(error.contains("Failed to parse line 2 of session file"));
    }

    #[test]
    fn test_session_load_ignores_partial_final_record() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("partial-tail.jsonl");
        let session = make_test_session();
        let header = serde_json::to_string(&meta_record(&session)).unwrap();
        fs::write(&path, format!("{header}\n{{\"type\":\"message\"")).unwrap();

        let loaded = Session::load(&path).unwrap();
        assert_eq!(loaded.records.len(), 1);
        assert!(matches!(
            loaded.records[0],
            SessionRecord::SessionMeta { .. }
        ));
    }

    #[test]
    fn test_session_load_skips_leading_empty_lines() {
        // Regression for #275: a blank line before `session_meta` must not hide
        // the header. Full load, replay, discovery, and listing share the same
        // framing, so all four accept leading empty lines.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let session = make_test_session();
        let mut file = Session::create_on_disk(&path, &meta_record(&session)).unwrap();
        Session::append_record(&mut file, &task_start(&session, "task-1")).unwrap();
        drop(file);

        let content = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("\n\n{content}")).unwrap();

        let loaded = Session::load(&path).unwrap();
        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.records.len(), 2);
        assert!(matches!(
            loaded.records[0],
            SessionRecord::SessionMeta { .. }
        ));
        assert!(matches!(
            loaded.records[1],
            SessionRecord::TaskStart(TaskStartData { .. })
        ));
    }

    #[test]
    fn test_session_create_and_load_roundtrip_v4() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");

        let session = make_test_session();
        let mut file = Session::create_on_disk(&path, &meta_record(&session)).unwrap();
        Session::append_record(&mut file, &task_start(&session, "task-1")).unwrap();
        Session::append_record(
            &mut file,
            &SessionRecord::Message(MessageData {
                role: Role::User,
                content: "Hello".to_string(),
                id: None,
                status: None,
                timestamp: None,
            }),
        )
        .unwrap();
        Session::append_record(
            &mut file,
            &SessionRecord::Message(MessageData {
                role: Role::Assistant,
                content: "Hi".to_string(),
                id: Some("msg-1".to_string()),
                status: Some("completed".to_string()),
                timestamp: None,
            }),
        )
        .unwrap();
        Session::append_record(&mut file, &task_complete(&session, "task-1")).unwrap();
        drop(file);

        let loaded = Session::load(&path).unwrap();

        assert_eq!(loaded.id, session.id);
        assert_eq!(loaded.working_dir, session.working_dir);
        assert_eq!(loaded.model, session.model);
        assert_eq!(loaded.records.len(), 5);
        assert_eq!(loaded.messages().len(), 2);
    }

    #[test]
    fn test_session_jsonl_v4_format() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");

        let session = make_test_session();
        let mut file = Session::create_on_disk(&path, &meta_record(&session)).unwrap();
        Session::append_record(
            &mut file,
            &SessionRecord::Message(MessageData {
                role: Role::User,
                content: "Hello".to_string(),
                id: None,
                status: None,
                timestamp: None,
            }),
        )
        .unwrap();

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);

        let init: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(init["type"], "session_meta");
        assert_eq!(init["format_version"], CURRENT_FORMAT_VERSION);
        assert_eq!(init["session_id"], "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(init["working_directory"], "/work");
        assert_eq!(init["model"], "test-model");
        assert!(init["tools"].is_array());

        // Second line is the message
        let msg: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(msg["type"], "message");
        assert_eq!(msg["role"], "user");
        assert_eq!(msg["content"], "Hello");
    }

    #[test]
    fn test_session_multiple_item_types() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");

        let session = make_test_session();
        let mut file = Session::create_on_disk(&path, &meta_record(&session)).unwrap();
        let records = vec![
            SessionRecord::Message(MessageData {
                role: Role::User,
                content: "list files".to_string(),
                id: None,
                status: None,
                timestamp: None,
            }),
            SessionRecord::FunctionCall(FunctionCallData {
                id: "fc-1".to_string(),
                call_id: "call-1".to_string(),
                name: "bash".to_string(),
                arguments: r#"{"cmd":"ls"}"#.to_string(),
                arguments_parse_error: None,
                replay: None,
                timestamp: None,
            }),
            SessionRecord::FunctionCallOutput(FunctionCallOutputData {
                call_id: "call-1".to_string(),
                output: "file.txt".to_string(),
                replay: None,
                timestamp: None,
            }),
            SessionRecord::Reasoning(ReasoningData {
                id: "r-1".to_string(),
                summary: Some(vec![ReasoningSummary::summary_text("thinking...")]),
                encrypted_content: None,
                content: None,
                timestamp: None,
            }),
        ];

        Session::append_records(&mut file, &records).unwrap();
        drop(file);
        let loaded = Session::load(&path).unwrap();

        assert_eq!(loaded.records.len(), 5);
        assert_eq!(loaded.messages().len(), 4);
    }

    #[test]
    fn test_skill_activated_records_are_metadata_not_messages() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");

        let session = make_test_session();
        let mut file = Session::create_on_disk(&path, &meta_record(&session)).unwrap();
        Session::append_records(
            &mut file,
            &[
                SessionRecord::FunctionCallOutput(FunctionCallOutputData {
                    call_id: "call-1".to_string(),
                    output: "echoed text: Skill 'not-real' activated".to_string(),
                    replay: None,
                    timestamp: None,
                }),
                SessionRecord::SkillActivated {
                    session_id: session.id.to_string(),
                    task_id: "task-1".to_string(),
                    timestamp: chrono::Utc::now(),
                    name: "real-skill".to_string(),
                    path: PathBuf::from("/work/.agents/skills/real-skill/SKILL.md"),
                },
            ],
        )
        .unwrap();
        drop(file);

        let loaded = Session::load(&path).unwrap();

        assert_eq!(loaded.messages().len(), 1);
        assert_eq!(
            loaded.activated_skills(),
            HashSet::from(["real-skill".to_string()])
        );
    }

    #[test]
    fn test_session_append_second_task_does_not_add_second_meta() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");

        let session = make_test_session();
        let mut file = Session::create_on_disk(&path, &meta_record(&session)).unwrap();
        Session::append_records(
            &mut file,
            &[
                task_start(&session, "task-1"),
                task_complete(&session, "task-1"),
            ],
        )
        .unwrap();
        drop(file);

        let mut file = Session::open_for_append(&path).unwrap();
        Session::append_records(
            &mut file,
            &[
                task_start(&session, "task-2"),
                task_complete(&session, "task-2"),
            ],
        )
        .unwrap();
        drop(file);

        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content.matches(r#""type":"session_meta""#).count(), 1);
        assert_eq!(content.matches(r#""type":"task_start""#).count(), 2);
        assert_eq!(content.matches(r#""type":"task_complete""#).count(), 2);
    }

    #[test]
    fn test_session_no_model() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");

        let session = Session::new(uuid::Uuid::new_v4(), PathBuf::from("/tmp"));
        let mut file = Session::create_on_disk(&path, &meta_record(&session)).unwrap();
        Session::append_record(
            &mut file,
            &SessionRecord::Message(MessageData {
                role: Role::User,
                content: "Hello".to_string(),
                id: None,
                status: None,
                timestamp: None,
            }),
        )
        .unwrap();

        let content = fs::read_to_string(&path).unwrap();
        let line = content.lines().next().unwrap();
        let val: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(val.get("model").is_none());

        let loaded = Session::load(&path).unwrap();
        assert!(loaded.model.is_none());
    }

    #[test]
    fn test_session_legacy_first_record_errors() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(
            &path,
            r#"{"format_version":3,"session_id":"550e8400-e29b-41d4-a716-446655440000","timestamp":"2026-04-04T15:51:54Z","working_directory":"/tmp/test","type":"init","tools":[]}"#,
        )
        .unwrap();

        let error = Session::load(&path).unwrap_err().to_string();
        assert!(error.contains("Unsupported or legacy session file format"));
    }

    #[test]
    fn test_session_format_version_mismatch_errors() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(
            &path,
            r#"{"type":"session_meta","format_version":5,"session_id":"550e8400-e29b-41d4-a716-446655440000","timestamp":"2026-04-04T15:51:54Z","working_directory":"/tmp/test","tools":[]}"#,
        )
        .unwrap();

        let error = Session::load(&path).unwrap_err().to_string();
        assert!(error.contains("Unsupported session format_version: 5 (expected 4)"));
    }

    #[test]
    fn test_session_loads_trailing_task_start() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let session = make_test_session();
        let mut file = Session::create_on_disk(&path, &meta_record(&session)).unwrap();
        Session::append_record(&mut file, &task_start(&session, "task-1")).unwrap();
        drop(file);

        let loaded = Session::load(&path).unwrap();
        assert!(matches!(
            loaded.records.last(),
            Some(SessionRecord::TaskStart(TaskStartData { .. }))
        ));
    }

    #[test]
    fn test_session_v4_roundtrip_with_reasoning() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");

        let session = make_test_session();
        let mut file = Session::create_on_disk(&path, &meta_record(&session)).unwrap();
        Session::append_record(
            &mut file,
            &SessionRecord::Reasoning(ReasoningData {
                id: "r-1".to_string(),
                summary: Some(vec![ReasoningSummary::summary_text("thinking...")]),
                encrypted_content: Some("gAAAAABencrypted...".to_string()),
                content: Some(vec![ReasoningContent {
                    content_type: ReasoningContentKind::ReasoningText,
                    text: Some("deep thoughts".to_string()),
                }]),
                timestamp: None,
            }),
        )
        .unwrap();
        drop(file);

        let loaded = Session::load(&path).unwrap();

        assert_eq!(loaded.records.len(), 2);
        match &loaded.records[1] {
            SessionRecord::Reasoning(ReasoningData {
                encrypted_content, ..
            }) => {
                assert_eq!(encrypted_content.as_deref(), Some("gAAAAABencrypted..."));
            },
            _ => panic!("Expected Reasoning record"),
        }
    }

    #[test]
    fn test_session_loads_rfc3339_conversation_timestamp() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(
            &path,
            concat!(
                r#"{"type":"session_meta","format_version":4,"session_id":"550e8400-e29b-41d4-a716-446655440000","timestamp":"2026-04-04T15:51:54Z","working_directory":"/tmp/test","tools":[],"git":{"repository_url":null,"branch":null,"commit_hash":null}}"#,
                "\n",
                r#"{"type":"message","role":"user","content":"Hello","timestamp":"2026-05-10T00:00:00Z"}"#,
                "\n"
            ),
        )
        .unwrap();

        let loaded = Session::load(&path).unwrap();

        match &loaded.records[1] {
            SessionRecord::Message(MessageData { timestamp, .. }) => {
                let expected = chrono::DateTime::parse_from_rfc3339("2026-05-10T00:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc);
                assert_eq!(*timestamp, Some(expected));
            },
            _ => panic!("Expected Message record"),
        }
    }

    #[test]
    fn test_session_load_fills_missing_conversation_timestamp_from_meta() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        fs::write(
            &path,
            concat!(
                r#"{"type":"session_meta","format_version":4,"session_id":"550e8400-e29b-41d4-a716-446655440000","timestamp":"2026-04-04T15:51:54Z","working_directory":"/tmp/test","tools":[],"git":{"repository_url":null,"branch":null,"commit_hash":null}}"#,
                "\n",
                r#"{"type":"message","role":"user","content":"Hello"}"#,
                "\n"
            ),
        )
        .unwrap();

        let loaded = Session::load(&path).unwrap();
        let expected = chrono::DateTime::parse_from_rfc3339("2026-04-04T15:51:54Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        match &loaded.records[1] {
            SessionRecord::Message(MessageData { timestamp, .. }) => {
                assert_eq!(*timestamp, Some(expected));
            },
            _ => panic!("Expected Message record"),
        }
        match &loaded.messages()[0] {
            ConversationItem::Message { timestamp, .. } => {
                assert_eq!(*timestamp, Some(expected));
            },
            _ => panic!("Expected Message item"),
        }
    }

    #[test]
    fn test_session_create_writes_v4() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");

        let session = make_test_session();
        let _file = Session::create_on_disk(&path, &meta_record(&session)).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        let first_line = content.lines().next().unwrap();
        let val: serde_json::Value = serde_json::from_str(first_line).unwrap();
        assert_eq!(val["type"], "session_meta");
        assert_eq!(val["format_version"], CURRENT_FORMAT_VERSION);
    }

    #[test]
    fn test_session_lock_error_is_user_facing() {
        let dir = TempDir::new().unwrap();
        let path = dir
            .path()
            .join("550e8400-e29b-41d4-a716-446655440000.jsonl");
        let session = make_test_session();
        let _locked = Session::create_on_disk(&path, &meta_record(&session)).unwrap();

        let error = Session::open_for_append(&path).unwrap_err().to_string();
        assert!(error.contains(
            "Another cake invocation is currently writing to session 550e8400-e29b-41d4-a716-446655440000"
        ));
    }
}
