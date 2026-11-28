//! Shared persistence codec for session JSONL files.
//!
//! A session file is append-only JSONL: a leading `session_meta` header line
//! followed by one `SessionRecord` per line. Full load, replay, discovery, and
//! listing share this module for framing, header decoding, append framing, and
//! the backward scan used by tail repair.
//!
//! [`SessionFramer`] reads a `BufRead` as raw bytes, splits on the newline
//! byte, decodes each line as UTF-8, trims it, skips empty and whitespace-only
//! lines, and yields a positioned [`FramedSessionLine`]. [`SessionHeader`]
//! decodes the first record and enforces the `session_meta` and format-version
//! contract once. [`append_json_record`] owns the serialize-newline-flush
//! sequence. [`read_unterminated_tail`] owns the backward scan; each writer
//! keeps its own recovery policy because session and telemetry crash edges
//! differ deliberately.
//!
//! Framing is byte-oriented so an interrupted writer that stops inside a
//! multibyte character cannot be mistaken for a corrupt session. A
//! newline-terminated line that is not valid UTF-8 remains a read error: only
//! the unterminated final fragment is handed to consumers undecoded, and only
//! those consumers that already tolerate a partial tail may ignore it.

use std::fs::File;
use std::io::{BufRead, Error, ErrorKind, Read, Result, Seek, SeekFrom, Write};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::types::{ContextIdentity, GitState, ProviderIdentity, SessionRecord, SessionRunConfig};

/// Current on-disk session format version.
pub const CURRENT_FORMAT_VERSION: u32 = 4;

/// One framed (non-empty, trimmed) record line of a session file.
#[derive(Debug, Clone)]
pub struct FramedSessionLine {
    /// 1-based physical line number of this record in the file, counting
    /// blank lines too, so consumers can report accurate file positions.
    pub line_number: usize,
    /// The trimmed record text, or `None` when the final physical line is an
    /// unterminated fragment that is not valid UTF-8. That is the shape an
    /// interrupted writer leaves when it stops inside a multibyte character.
    /// A newline-terminated line that is not valid UTF-8 is a read error
    /// rather than a framed line, so `None` always implies
    /// [`Self::partial_tail`]. Only a consumer that tolerates a partial tail
    /// may ignore it.
    pub text: Option<String>,
    /// True when this is the final physical line of the stream and it was not
    /// terminated by a newline, i.e. a partial tail left by an interrupted
    /// writer. Only meaningful when the consumer fails to parse the record;
    /// the consumer decides whether to tolerate it.
    pub partial_tail: bool,
}

/// Streaming framer that splits a session file into positioned record lines.
///
/// Construction is cheap; the underlying reader is not read until the first
/// call to [`next_record`](Self::next_record). The first framed line is the
/// header; the codec does not interpret or validate it.
pub struct SessionFramer<R: BufRead> {
    inner: R,
    next_line_number: usize,
}

impl<R: BufRead> SessionFramer<R> {
    /// Begin framing over a byte stream.
    pub const fn new(inner: R) -> Self {
        Self {
            inner,
            next_line_number: 1,
        }
    }

    /// Read the next non-empty, trimmed record line.
    ///
    /// Returns `Ok(None)` at end of stream, and `Ok(None)` is also returned
    /// when the input is empty. I/O errors from the underlying reader are
    /// returned as-is. A newline-terminated line that is not valid UTF-8 is
    /// returned as an [`ErrorKind::InvalidData`] error naming its line.
    pub fn next_record(&mut self) -> Result<Option<FramedSessionLine>> {
        loop {
            let line_number = self.next_line_number;
            self.next_line_number += 1;

            let mut raw = Vec::new();
            if self.inner.read_until(b'\n', &mut raw)? == 0 {
                return Ok(None);
            }
            let partial_tail = !raw.ends_with(b"\n");

            match std::str::from_utf8(&raw) {
                Ok(decoded) => {
                    let text = decoded.trim();
                    if text.is_empty() {
                        continue;
                    }
                    return Ok(Some(FramedSessionLine {
                        line_number,
                        text: Some(text.to_owned()),
                        partial_tail,
                    }));
                },
                Err(_) if partial_tail => {
                    return Ok(Some(FramedSessionLine {
                        line_number,
                        text: None,
                        partial_tail,
                    }));
                },
                Err(_) => {
                    return Err(Error::new(
                        ErrorKind::InvalidData,
                        format!("session record on line {line_number} is not valid UTF-8"),
                    ));
                },
            }
        }
    }
}

/// A decoded `session_meta` header plus the fields needed to continue a load.
///
/// Every session reader decodes the first line through this type, so the
/// required `type`, format version, and header fields have one owner. Callers
/// that need a specific session UUID validate [`Self::parsed_session_id`]
/// themselves to preserve their own error vocabulary.
#[derive(Debug)]
pub struct SessionHeader {
    /// The complete `session_meta` record, retained as the first loaded record.
    pub meta: SessionRecord,
    /// Session UUID exactly as written in the header.
    pub session_id: String,
    /// Session creation timestamp from the header.
    pub timestamp: DateTime<Utc>,
    /// Directory the session is bound to.
    pub working_directory: PathBuf,
    /// Provider model ID used by the session.
    pub model: Option<String>,
    /// Configured `[[models]]` entry name used by the session.
    pub model_config: Option<String>,
    /// Sanitized provider destination pinned when the session was created.
    pub provider_identity: Option<ProviderIdentity>,
    /// Full system prompt captured at creation.
    pub system_prompt: Option<String>,
    /// Run choices restored on resume.
    pub run_config: Option<SessionRunConfig>,
    /// Context identity captured at creation.
    pub context_identity: Option<ContextIdentity>,
    /// Git state captured at creation.
    pub git: GitState,
}

impl SessionHeader {
    /// Read and decode the header from a session stream.
    ///
    /// The reader is left positioned immediately after the header line, so a
    /// discovery caller can continue scanning the same stream for a prompt.
    pub fn read(reader: &mut impl BufRead) -> std::result::Result<Self, HeaderError> {
        let mut framer = SessionFramer::new(reader);
        Self::read_framed(&mut framer)
    }

    /// Read and decode the header from an existing framer.
    pub fn read_framed(
        framer: &mut SessionFramer<impl BufRead>,
    ) -> std::result::Result<Self, HeaderError> {
        let Some(line) = framer.next_record().map_err(HeaderError::Read)? else {
            return Err(HeaderError::Empty);
        };
        let Some(text) = line.text.as_deref() else {
            return Err(HeaderError::NotUtf8);
        };
        let value: serde_json::Value = serde_json::from_str(text).map_err(HeaderError::Parse)?;
        if value.get("type").and_then(serde_json::Value::as_str) != Some("session_meta") {
            return Err(HeaderError::NotSessionMeta);
        }
        let meta: SessionRecord = serde_json::from_str(text).map_err(HeaderError::Parse)?;
        let SessionRecord::SessionMeta {
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
        } = &meta
        else {
            return Err(HeaderError::NotSessionMeta);
        };
        if *format_version != CURRENT_FORMAT_VERSION {
            return Err(HeaderError::UnsupportedVersion {
                found: *format_version,
                expected: CURRENT_FORMAT_VERSION,
            });
        }
        let session_id_value = session_id.clone();
        let timestamp_value = *timestamp;
        let working_directory_value = working_directory.clone();
        let model_value = model.clone();
        let model_config_value = model_config.clone();
        let provider_identity_value = provider_identity.as_deref().cloned();
        let system_prompt_value = system_prompt.clone();
        let run_config_value = run_config.clone();
        let context_identity_value = context_identity.as_deref().cloned();
        let git_value = git.clone();
        Ok(Self {
            meta,
            session_id: session_id_value,
            timestamp: timestamp_value,
            working_directory: working_directory_value,
            model: model_value,
            model_config: model_config_value,
            provider_identity: provider_identity_value,
            system_prompt: system_prompt_value,
            run_config: run_config_value,
            context_identity: context_identity_value,
            git: git_value,
        })
    }

    /// Parse the header's session UUID for callers whose identity contract
    /// requires a valid UUID.
    pub fn parsed_session_id(&self) -> std::result::Result<Uuid, uuid::Error> {
        Uuid::parse_str(&self.session_id)
    }
}

/// A session header that cannot be decoded.
#[derive(Debug, thiserror::Error)]
pub enum HeaderError {
    /// The stream ended before a header record.
    #[error("Session file is empty")]
    Empty,
    /// The header fragment was not valid UTF-8.
    #[error("Session file begins with a record that is not valid UTF-8")]
    NotUtf8,
    /// The underlying stream could not be read.
    #[error("Failed to read session header")]
    Read(#[source] std::io::Error),
    /// The first record was not a `session_meta` record.
    #[error("Unsupported or legacy session file format: expected session_meta as first record")]
    NotSessionMeta,
    /// The header declared an unsupported format version.
    #[error("Unsupported session format_version: {found} (expected {expected})")]
    UnsupportedVersion {
        /// Version found in the file.
        found: u32,
        /// Version this build supports.
        expected: u32,
    },
    /// The header record was not valid JSON for `session_meta`.
    #[error("Failed to parse session_meta record")]
    Parse(#[source] serde_json::Error),
}

/// Append one JSON object as a newline-terminated JSONL record and flush it.
pub fn append_json_record(
    writer: &mut impl Write,
    record: &impl Serialize,
) -> std::result::Result<(), JsonlWriteError> {
    serde_json::to_writer(&mut *writer, record).map_err(JsonlWriteError::Serialize)?;
    writer.write_all(b"\n").map_err(JsonlWriteError::Newline)?;
    writer.flush().map_err(JsonlWriteError::Flush)
}

/// A failed JSONL append, split by the operation that failed.
#[derive(Debug, thiserror::Error)]
pub enum JsonlWriteError {
    /// The record could not be serialized.
    #[error("Failed to serialize JSONL record")]
    Serialize(#[source] serde_json::Error),
    /// The terminating newline could not be written.
    #[error("Failed to write JSONL newline")]
    Newline(#[source] std::io::Error),
    /// The record could not be flushed.
    #[error("Failed to flush JSONL file")]
    Flush(#[source] std::io::Error),
}

/// Bytes inspected per backward window when locating a final record boundary.
const TAIL_WINDOW_BYTES: u64 = 64 * 1024;

/// Read the bytes after the last newline of a JSONL file.
///
/// Returns `None` when the file is empty or already ends at a record boundary.
/// The scan reads backwards in windows, so a large file is not buffered whole.
/// Consumers apply their own recovery policy to the returned fragment.
pub fn read_unterminated_tail(file: &File, len: u64) -> Result<Option<Vec<u8>>> {
    if len == 0 {
        return Ok(None);
    }

    let mut window = len.min(TAIL_WINDOW_BYTES);
    loop {
        let start = len - window;
        let mut buffer = vec![0; usize::try_from(window).unwrap_or(usize::MAX)];
        let mut handle = file;
        handle.seek(SeekFrom::Start(start))?;
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
