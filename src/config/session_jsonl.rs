//! Shared framing codec for session JSONL files.
//!
//! A session file is append-only JSONL: a leading `session_meta` header line
//! followed by one `SessionRecord` per line. Four consumers (full load,
//! replay, discovery, and listing) read these files. Before this module each
//! re-encoded header extraction and line framing itself, and their
//! leading-empty-record and partial-tail semantics drifted apart.
//!
//! [`SessionFramer`] is the single framing codec. It reads a `BufRead` line by
//! line, trims each line, skips empty and whitespace-only lines, and yields a
//! positioned [`FramedSessionLine`]. It does not parse records, validate
//! headers, normalize fields, or decide how to handle a malformed tail; each
//! consumer does that with the position and framing information it provides.

use std::io::{BufRead, Result};

/// One framed (non-empty, trimmed) record line of a session file.
#[derive(Debug, Clone)]
pub struct FramedSessionLine {
    /// 1-based physical line number of this record in the file, counting
    /// blank lines too, so consumers can report accurate file positions.
    pub line_number: usize,
    /// The trimmed record text. Empty and whitespace-only lines are skipped
    /// before this is produced.
    pub text: String,
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
    /// returned as-is.
    pub fn next_record(&mut self) -> Result<Option<FramedSessionLine>> {
        loop {
            let line_number = self.next_line_number;
            self.next_line_number += 1;

            let mut raw = String::new();
            if self.inner.read_line(&mut raw)? == 0 {
                return Ok(None);
            }

            let text = raw.trim();
            if text.is_empty() {
                continue;
            }

            // A line not terminated by a '\n' necessarily read the rest of the
            // stream: it is the final line and a partial tail.
            return Ok(Some(FramedSessionLine {
                line_number,
                text: text.to_string(),
                partial_tail: !raw.ends_with('\n'),
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Frame `content` and return the `(line_number, text, partial_tail)`
    /// tuples for every produced line.
    fn frame(content: &str) -> Vec<(usize, String, bool)> {
        let mut framer = SessionFramer::new(Cursor::new(content.as_bytes()));
        let mut out = Vec::new();
        while let Some(line) = framer.next_record().unwrap() {
            out.push((line.line_number, line.text, line.partial_tail));
        }
        out
    }

    #[test]
    fn yields_lines_in_order_with_physical_line_numbers() {
        let lines = frame("a\nb\nc\n");
        assert_eq!(
            lines,
            vec![
                (1, "a".to_string(), false),
                (2, "b".to_string(), false),
                (3, "c".to_string(), false),
            ]
        );
    }

    #[test]
    fn trims_each_line_and_skips_blank_lines() {
        let lines = frame("  a  \n\n   \n\t\nb\n");
        assert_eq!(
            lines,
            vec![(1, "a".to_string(), false), (5, "b".to_string(), false),]
        );
    }

    #[test]
    fn skips_leading_empty_lines_before_the_header() {
        // The drift this codec fixes: the header is the first non-empty line,
        // so a leading blank line must not hide it.
        let lines = frame("\n\n{\"type\":\"session_meta\"}\n");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].0, 3);
        assert_eq!(lines[0].1, "{\"type\":\"session_meta\"}");
    }

    #[test]
    fn marks_a_final_line_without_newline_as_partial_tail() {
        let lines = frame("a\nb");
        assert_eq!(
            lines,
            vec![(1, "a".to_string(), false), (2, "b".to_string(), true),]
        );
    }

    #[test]
    fn does_not_mark_newline_terminated_final_line_as_partial() {
        let lines = frame("a\nb\n");
        assert!(!lines[1].2);
    }

    #[test]
    fn does_not_mark_a_trailing_blank_line_after_a_partial_as_partial() {
        // The final non-empty line is terminated by '\n' (there is a blank
        // line after it), so it is not a partial tail.
        let lines = frame("a\nb\n\n");
        assert_eq!(lines.len(), 2);
        assert!(!lines[1].2);
    }

    #[test]
    fn empty_input_yields_no_lines() {
        assert!(frame("").is_empty());
    }

    #[test]
    fn single_line_without_newline_is_a_partial_tail() {
        let lines = frame("a");
        assert_eq!(lines, vec![(1, "a".to_string(), true)]);
    }
}
