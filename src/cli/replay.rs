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
use crate::config::session::ensure_checkpoint_matches_session;
use crate::config::session_jsonl::{HeaderError, SessionFramer, SessionHeader};
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
        std::future::ready(()).await;
        if options.output_format != OutputFormat::StreamJson {
            return Err(fail(ReplayError::OutputFormat));
        }

        let Ok(uuid) = Uuid::parse_str(&self.uuid) else {
            return Err(fail(ReplayError::InvalidUuid(self.uuid.clone())));
        };

        let path = data_dir.session_path(uuid);
        let mut records = load_records(&path, uuid).map_err(fail)?;
        data_dir
            .session_blob_store(uuid)
            .hydrate_session(&uuid.to_string(), &mut records)
            .map_err(|error| {
                fail(ReplayError::Corrupt(
                    uuid.to_string(),
                    format!("failed to hydrate session payloads: {error:#}"),
                ))
            })?;
        emit_records(records)
    }
}

fn emit_records(records: Vec<SessionRecord>) -> anyhow::Result<()> {
    for record in records {
        if let SessionRecord::TurnUsage(data) = &record
            && data.purpose == Some(crate::types::TurnUsagePurpose::Compaction)
        {
            continue;
        }
        let mut stream = StreamRecord::from(record);
        stream.strip_content_ids();
        emit(&stream)?;
    }
    Ok(())
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
    emit(&record).err().unwrap_or_else(|| error.into())
}

/// Print one stream record as a JSON line on stdout.
fn emit(record: &StreamRecord) -> anyhow::Result<()> {
    serde_json::to_string(record)
        .inspect_err(|error| tracing::warn!("Replay serialization failed: {error}"))
        .ok()
        .map_or(Ok(()), |json| CliOutputSink::write_stream_record(&json))
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
    let file = fs::File::open(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::PermissionDenied => ReplayError::Permission(session_id.to_string()),
        std::io::ErrorKind::NotFound => ReplayError::MissingSession(session_id.to_string()),
        _ => ReplayError::Corrupt(
            session_id.to_string(),
            format!("failed to read session file: {error}"),
        ),
    })?;
    let mut framer = SessionFramer::new(BufReader::new(file));

    let header =
        SessionHeader::read_framed(&mut framer).map_err(|error| header_error(error, session_id))?;
    if header.session_id != session_id.to_string() {
        return Err(ReplayError::Corrupt(
            session_id.to_string(),
            format!(
                "session_meta session_id '{}' does not match requested session {session_id}",
                header.session_id
            ),
        ));
    }

    let mut next_record = || {
        framer.next_record().map_err(|error| {
            ReplayError::Corrupt(
                session_id.to_string(),
                format!("failed to read session file: {error}"),
            )
        })
    };

    let mut records = vec![header.meta];
    while let Some(line) = next_record()? {
        let Some(text) = line.text.as_deref() else {
            tracing::warn!(
                "Ignoring partial final session record in {}: line {} ends inside a multibyte character",
                path.display(),
                line.line_number
            );
            break;
        };
        match parse_record(text, &session_id) {
            Ok(record) => records.push(record),
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

/// Map a shared header failure onto the replay error vocabulary without
/// changing the stream record or exit class.
fn header_error(error: HeaderError, session_id: Uuid) -> ReplayError {
    match error {
        HeaderError::Empty => {
            ReplayError::Corrupt(session_id.to_string(), "session file is empty".to_string())
        },
        HeaderError::NotUtf8 => ReplayError::Corrupt(
            session_id.to_string(),
            "session file begins with a record that is not valid UTF-8".to_string(),
        ),
        HeaderError::Read(source) => ReplayError::Corrupt(
            session_id.to_string(),
            format!("failed to read session file: {source}"),
        ),
        HeaderError::NotSessionMeta => ReplayError::Corrupt(
            session_id.to_string(),
            "first record is not session_meta".to_string(),
        ),
        HeaderError::UnsupportedVersion { found, expected } => ReplayError::UnsupportedFormat {
            session_id: session_id.to_string(),
            found,
            expected,
        },
        HeaderError::Parse(source) => ReplayError::Corrupt(
            session_id.to_string(),
            format!("invalid session record: {source}"),
        ),
    }
}

/// Validate a checkpoint's or lifecycle record's session envelope while
/// replaying records.
fn validate_record_envelope(record: &SessionRecord, session_id: &Uuid) -> Result<(), ReplayError> {
    let invalid = |kind: &str, found: &str| {
        ReplayError::Corrupt(
            session_id.to_string(),
            format!(
                "invalid {kind} lifecycle record: session_id '{found}' does not match requested \
                 session {session_id}"
            ),
        )
    };
    match record {
        SessionRecord::Compaction(checkpoint) => {
            ensure_checkpoint_matches_session(checkpoint, &session_id.to_string()).map_err(
                |error| {
                    ReplayError::Corrupt(
                        session_id.to_string(),
                        format!("invalid compaction checkpoint: {error:#}"),
                    )
                },
            )
        },
        SessionRecord::CompactionStart(data) if data.session_id != session_id.to_string() => {
            Err(invalid("compaction_start", &data.session_id))
        },
        SessionRecord::CompactionEnd(data) if data.session_id != session_id.to_string() => {
            Err(invalid("compaction_end", &data.session_id))
        },
        _ => Ok(()),
    }
}

/// Parse one JSONL session record, mapping parse failures to [`ReplayError::Corrupt`].
fn parse_record(line: &str, session_id: &Uuid) -> Result<SessionRecord, ReplayError> {
    let record: SessionRecord = serde_json::from_str(line).map_err(|error| {
        ReplayError::Corrupt(
            session_id.to_string(),
            format!("invalid session record: {error}"),
        )
    })?;
    validate_record_envelope(&record, session_id)?;
    Ok(record)
}
