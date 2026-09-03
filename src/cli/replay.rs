//! `cake replay` — re-emit an existing session transcript as stream-json.
//!
//! Replay is a first-class client operation: it reads a session file
//! read-only (no lock, no append, no network) and emits the transcript using
//! the same `StreamRecord` vocabulary as live `--output-format stream-json`
//! output, plus the persisted metadata records live streams omit. A trailing
//! partial record from an interrupted writer is skipped with a warning, same
//! as the session loader.

use std::fs;
use std::io::BufReader;
use std::path::Path;

use clap::Parser;
use uuid::Uuid;

use crate::OutputFormat;
use crate::cli::{CliOutputSink, CmdRunner, CommandRunOptions};
use crate::config::DataDir;
use crate::config::session::CURRENT_FORMAT_VERSION;
use crate::config::session_jsonl::SessionFramer;
use crate::types::{ReplayErrorKind, SessionRecord, StreamRecord};

/// Replay an existing session transcript as stream-json events.
#[derive(Clone, Debug, Parser)]
pub struct ReplayCommand {
    /// Session UUID to replay
    #[arg(value_name = "UUID")]
    uuid: String,
}

/// Failures `cake replay` reports, each mapped to a process exit code and a
/// machine-readable `replay_error` stream record.
#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    /// `--output-format` is not `stream-json`.
    #[error("cake replay requires --output-format stream-json")]
    OutputFormat,
    /// The session UUID argument is not a valid UUID.
    #[error("invalid session UUID '{0}'")]
    InvalidUuid(String),
    /// No session file exists for the UUID.
    #[error("session not found: {0}")]
    MissingSession(String),
    /// The session file is unreadable or its records are corrupt.
    #[error("cannot replay session {0}: {1}")]
    Corrupt(String, String),
    /// The session file uses an unsupported format version.
    #[error(
        "unsupported session format_version {found} (expected {expected}) for session {session_id}"
    )]
    UnsupportedFormat {
        session_id: String,
        found: u32,
        expected: u32,
    },
    /// The session file could not be opened (permission denied).
    #[error("permission denied opening session file for {0}")]
    Permission(String),
}

impl ReplayError {
    /// The process exit code that accompanies this failure.
    pub const fn exit_code(&self) -> u8 {
        use crate::exit_code::code;
        match self {
            Self::OutputFormat | Self::InvalidUuid(_) | Self::MissingSession(_) => {
                code::INPUT_ERROR
            },
            Self::Corrupt { .. } | Self::UnsupportedFormat { .. } | Self::Permission(_) => {
                code::AGENT_ERROR
            },
        }
    }

    /// The machine-readable category for the `replay_error` stream record.
    const fn kind(&self) -> ReplayErrorKind {
        match self {
            Self::OutputFormat => ReplayErrorKind::OutputFormat,
            Self::InvalidUuid(_) => ReplayErrorKind::InvalidUuid,
            Self::MissingSession(_) => ReplayErrorKind::SessionNotFound,
            Self::Corrupt { .. } => ReplayErrorKind::Corrupt,
            Self::UnsupportedFormat { .. } => ReplayErrorKind::UnsupportedFormat,
            Self::Permission(_) => ReplayErrorKind::Permission,
        }
    }

    /// The session UUID associated with the failure, when known.
    fn session_id(&self) -> Option<String> {
        match self {
            Self::OutputFormat | Self::InvalidUuid(_) => None,
            Self::MissingSession(id)
            | Self::Corrupt(id, _)
            | Self::UnsupportedFormat { session_id: id, .. }
            | Self::Permission(id) => Some(id.clone()),
        }
    }
}

impl CmdRunner for ReplayCommand {
    async fn run(&self, data_dir: &DataDir, options: &CommandRunOptions<'_>) -> anyhow::Result<()> {
        // CmdRunner is asynchronous, although this command has no async work.
        std::future::ready(()).await;
        if options.output_format != OutputFormat::StreamJson {
            return Err(fail(ReplayError::OutputFormat));
        }

        let Ok(uuid) = Uuid::parse_str(&self.uuid) else {
            return Err(fail(ReplayError::InvalidUuid(self.uuid.clone())));
        };

        // No `exists()` pre-check: `Path::exists` reports `false` for
        // permission-denied paths, which would misclassify them as
        // `session_not_found`. `load_records` distinguishes missing files from
        // permission failures via `File::open`'s error kind.
        let path = data_dir.session_path(uuid);
        let records = load_records(&path, uuid).map_err(fail)?;
        // `turn_usage` is a session-only audit record with no stream-json
        // counterpart; replay mirrors the live stream vocabulary, so it is
        // filtered out here rather than converted.
        for record in records
            .into_iter()
            .filter(|record| !matches!(record, SessionRecord::TurnUsage(_)))
        {
            emit(&StreamRecord::from(record));
        }
        Ok(())
    }
}

/// Emit a `replay_error` stream record, then convert the failure into the
/// `anyhow::Error` the CLI reports on stderr with a non-zero exit.
fn fail(error: ReplayError) -> anyhow::Error {
    let record = StreamRecord::ReplayError {
        session_id: error.session_id(),
        kind: error.kind(),
        error: error.to_string(),
        exit_code: error.exit_code(),
    };
    emit(&record);
    error.into()
}

/// Print one stream record as a JSON line on stdout.
fn emit(record: &StreamRecord) {
    match serde_json::to_string(record) {
        Ok(json) => CliOutputSink::write_stream_record(&json),
        Err(error) => tracing::warn!("Replay serialization failed: {error}"),
    }
}

/// Read a session file read-only and return its records, mapping every
/// failure to a precise [`ReplayError`]. No lock is taken and nothing is
/// appended or mutated.
///
/// A malformed final line without a trailing newline is treated as an
/// interrupted write and skipped with a warning, mirroring
/// [`crate::config::session::Session::load`]; any other malformed line is a
/// [`ReplayError::Corrupt`] failure.
fn load_records(path: &Path, session_id: Uuid) -> Result<Vec<SessionRecord>, ReplayError> {
    // Replay reads the file through the shared framing codec. Open errors are
    // mapped here; mid-read errors surface during framing and are mapped to
    // `Corrupt` by `next_record`.
    let file = fs::File::open(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::PermissionDenied => ReplayError::Permission(session_id.to_string()),
        std::io::ErrorKind::NotFound => ReplayError::MissingSession(session_id.to_string()),
        _ => ReplayError::Corrupt(
            session_id.to_string(),
            format!("failed to read session file: {error}"),
        ),
    })?;
    let mut framer = SessionFramer::new(BufReader::new(file));

    let mut next_record = || {
        framer.next_record().map_err(|error| {
            ReplayError::Corrupt(
                session_id.to_string(),
                format!("failed to read session file: {error}"),
            )
        })
    };

    let Some(first) = next_record()? else {
        return Err(ReplayError::Corrupt(
            session_id.to_string(),
            "session file is empty".to_string(),
        ));
    };
    let meta = parse_record(&first.text, &session_id)?;
    validate_meta(&meta, session_id)?;

    let mut records = vec![meta];
    while let Some(line) = next_record()? {
        match parse_record(&line.text, &session_id) {
            Ok(record) => records.push(record),
            // A writer interrupted mid-record leaves a partial final line
            // without a trailing newline; tolerate it like `Session::load`.
            Err(error) if line.partial_tail => {
                tracing::warn!(
                    "Ignoring partial final session record in {}: {error}",
                    path.display()
                );
                break;
            },
            Err(error) => return Err(error),
        }
    }
    Ok(records)
}

/// Validate the header record's format version and identity against the
/// requested session UUID.
fn validate_meta(meta: &SessionRecord, session_id: Uuid) -> Result<(), ReplayError> {
    match meta {
        SessionRecord::SessionMeta {
            format_version,
            session_id: meta_session_id,
            ..
        } => {
            if *format_version != CURRENT_FORMAT_VERSION {
                return Err(ReplayError::UnsupportedFormat {
                    session_id: session_id.to_string(),
                    found: *format_version,
                    expected: CURRENT_FORMAT_VERSION,
                });
            }
            if meta_session_id != &session_id.to_string() {
                return Err(ReplayError::Corrupt(
                    session_id.to_string(),
                    format!(
                        "session_meta session_id '{meta_session_id}' does not match requested session {session_id}"
                    ),
                ));
            }
            Ok(())
        },
        _ => Err(ReplayError::Corrupt(
            session_id.to_string(),
            "first record is not session_meta".to_string(),
        )),
    }
}

/// Parse one JSONL session record, mapping parse failures to [`ReplayError::Corrupt`].
fn parse_record(line: &str, session_id: &Uuid) -> Result<SessionRecord, ReplayError> {
    serde_json::from_str(line).map_err(|error| {
        ReplayError::Corrupt(
            session_id.to_string(),
            format!("invalid session record: {error}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::session::CURRENT_FORMAT_VERSION;
    use crate::types::GitState;

    const SESSION_ID: &str = "550e8400-e29b-41d4-a716-446655440000";

    fn session_path(dir: &std::path::Path) -> std::path::PathBuf {
        dir.join(format!("{SESSION_ID}.jsonl"))
    }

    fn write_session(dir: &std::path::Path, lines: &[&str]) -> std::path::PathBuf {
        let path = session_path(dir);
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        path
    }

    fn meta_line() -> String {
        serde_json::to_string(&SessionRecord::SessionMeta {
            format_version: CURRENT_FORMAT_VERSION,
            session_id: SESSION_ID.to_string(),
            timestamp: chrono::Utc::now(),
            working_directory: std::path::PathBuf::from("/work"),
            model: Some("test-model".to_string()),
            model_config: Some("test".to_string()),
            tools: vec!["bash".to_string(), "read".to_string()],
            cake_version: None,
            system_prompt: None,
            git: GitState::default(),
        })
        .unwrap()
    }

    fn task_start_line() -> String {
        serde_json::to_string(&SessionRecord::TaskStart(crate::types::TaskStartData {
            session_id: SESSION_ID.to_string(),
            task_id: "550e8400-e29b-41d4-a716-446655440001".to_string(),
            timestamp: chrono::Utc::now(),
        }))
        .unwrap()
    }

    #[test]
    fn load_records_skips_leading_empty_lines() {
        // Regression for #275: a blank line before `session_meta` must not hide
        // the header, matching `Session::load`, discovery, and listing.
        let dir = tempfile::tempdir().unwrap();
        let path = write_session(dir.path(), &[&meta_line(), &task_start_line()]);
        let content = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("\n\n{content}")).unwrap();

        let records = load_records(&path, Uuid::parse_str(SESSION_ID).unwrap()).unwrap();
        assert_eq!(records.len(), 2);
        assert!(matches!(records[0], SessionRecord::SessionMeta { .. }));
        assert!(matches!(records[1], SessionRecord::TaskStart(_)));
    }

    #[test]
    fn load_records_returns_all_lines_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_session(
            dir.path(),
            &[&meta_line(), &task_start_line(), &meta_line()],
        );
        let records = load_records(&path, Uuid::parse_str(SESSION_ID).unwrap()).unwrap();
        assert_eq!(records.len(), 3);
        assert!(matches!(records[0], SessionRecord::SessionMeta { .. }));
        assert!(matches!(records[1], SessionRecord::TaskStart(_)));
    }

    #[test]
    fn load_records_rejects_unsupported_format_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_session(dir.path(), &[&meta_line()]);
        let content = std::fs::read_to_string(&path).unwrap();
        let unsupported = content.replace(
            &format!("\"format_version\":{CURRENT_FORMAT_VERSION}"),
            "\"format_version\":99",
        );
        std::fs::write(&path, unsupported).unwrap();

        let error = load_records(&path, Uuid::parse_str(SESSION_ID).unwrap()).unwrap_err();
        assert!(matches!(
            error,
            ReplayError::UnsupportedFormat { found: 99, .. }
        ));
        assert_eq!(error.exit_code(), crate::exit_code::code::AGENT_ERROR);
    }

    #[test]
    fn load_records_rejects_corrupt_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_session(dir.path(), &[&meta_line(), "{ not json"]);
        let error = load_records(&path, Uuid::parse_str(SESSION_ID).unwrap()).unwrap_err();
        assert!(matches!(error, ReplayError::Corrupt(_, _)));
    }

    #[test]
    fn load_records_rejects_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let error = load_records(
            &session_path(dir.path()),
            Uuid::parse_str(SESSION_ID).unwrap(),
        )
        .unwrap_err();
        assert!(matches!(error, ReplayError::MissingSession(_)));
        assert_eq!(error.exit_code(), crate::exit_code::code::INPUT_ERROR);
    }

    #[test]
    fn load_records_rejects_read_error_as_corrupt() {
        // Opening a directory succeeds but reading it fails with an I/O error,
        // exercising the mid-read `Corrupt` mapping in `load_records`.
        let dir = tempfile::tempdir().unwrap();
        let error = load_records(dir.path(), Uuid::parse_str(SESSION_ID).unwrap()).unwrap_err();
        assert!(matches!(error, ReplayError::Corrupt(_, _)));
    }

    #[test]
    fn load_records_rejects_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_session(dir.path(), &[]);
        let error = load_records(&path, Uuid::parse_str(SESSION_ID).unwrap()).unwrap_err();
        assert!(matches!(error, ReplayError::Corrupt(_, _)));
    }

    #[test]
    fn load_records_tolerates_partial_final_line_without_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = session_path(dir.path());
        // No trailing newline: the final line is a partial JSON fragment left
        // by an interrupted writer, which `Session::load` also tolerates.
        std::fs::write(&path, format!("{}\n{}\n{{", meta_line(), task_start_line())).unwrap();

        let records = load_records(&path, Uuid::parse_str(SESSION_ID).unwrap()).unwrap();
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn load_records_rejects_partial_line_in_middle_of_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_session(
            dir.path(),
            &[&meta_line(), "{ not json", &task_start_line()],
        );
        let error = load_records(&path, Uuid::parse_str(SESSION_ID).unwrap()).unwrap_err();
        assert!(matches!(error, ReplayError::Corrupt(_, _)));
    }

    #[test]
    fn load_records_rejects_mismatched_header_session_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_session(dir.path(), &[&meta_line()]);
        let content = std::fs::read_to_string(&path).unwrap();
        let mismatched = content.replace(SESSION_ID, "550e8400-e29b-41d4-a716-4466554400ff");
        std::fs::write(&path, mismatched).unwrap();

        let error = load_records(&path, Uuid::parse_str(SESSION_ID).unwrap()).unwrap_err();
        assert!(matches!(error, ReplayError::Corrupt(_, _)));
    }
}
