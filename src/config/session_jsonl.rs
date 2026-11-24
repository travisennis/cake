//! Shared framing codec for session JSONL files.
//!
//! A session file is append-only JSONL: a leading `session_meta` header line
//! followed by one `SessionRecord` per line. Four consumers (full load,
//! replay, discovery, and listing) read these files. Before this module each
//! re-encoded header extraction and line framing itself, and their
//! leading-empty-record and partial-tail semantics drifted apart.
//!
//! [`SessionFramer`] is the single framing codec. It reads a `BufRead` as raw
//! bytes, splits on the newline byte, decodes each line as UTF-8, trims it,
//! skips empty and whitespace-only lines, and yields a positioned
//! [`FramedSessionLine`]. It does not parse records, validate headers,
//! normalize fields, or decide how to handle a malformed tail; each consumer
//! does that with the position and framing information it provides.
//!
//! Framing is byte-oriented so an interrupted writer that stops inside a
//! multibyte character cannot be mistaken for a corrupt session. A
//! newline-terminated line that is not valid UTF-8 remains a read error: only
//! the unterminated final fragment is handed to consumers undecoded, and only
//! those consumers that already tolerate a partial tail may ignore it.

use std::io::{BufRead, Error, ErrorKind, Result};

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
