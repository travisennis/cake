//! Opt-in JSONL capture of judge provider outcomes for offline gate parity.
//!
//! The corpus recording mode writes one line per provider attempt in the
//! `cake-safety-judge` record format, so the gate can replay the oracle's
//! actual assistant content and HTTP status without another paid call.
//!
//! The file is sensitive: it contains command text and raw provider assistant
//! content. It is created with owner-only permissions and is never written by
//! production judge paths.

use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use serde::Serialize;

use crate::types::Usage;

/// One replayed provider outcome, matching `internal/provider.RecordEntry` in
/// `cake-safety-judge`.
#[derive(Debug, Serialize)]
pub struct JudgeRecordEntry {
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assistant_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_class: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

impl JudgeRecordEntry {
    /// A 2xx response, including one whose assistant text later fails verdict
    /// parsing: replaying the text reproduces that failure.
    pub const fn success(
        command: String,
        http_status: Option<u16>,
        assistant_content: Option<String>,
        usage: Option<Usage>,
    ) -> Self {
        Self {
            command,
            http_status,
            assistant_content,
            refusal: None,
            error_class: None,
            error_message: None,
            retryable: None,
            usage,
        }
    }

    /// A non-success HTTP response. The gate classifies the status itself, so
    /// no error class is needed.
    pub const fn http_error(command: String, http_status: u16) -> Self {
        Self {
            command,
            http_status: Some(http_status),
            assistant_content: None,
            refusal: None,
            error_class: None,
            error_message: None,
            retryable: None,
            usage: None,
        }
    }

    /// A provider refusal, which is never retried.
    pub const fn refusal(command: String) -> Self {
        Self {
            command,
            http_status: None,
            assistant_content: None,
            refusal: Some(true),
            error_class: None,
            error_message: None,
            retryable: None,
            usage: None,
        }
    }

    /// A transport-layer failure with no HTTP response.
    pub const fn transport(command: String, message: &'static str) -> Self {
        Self {
            command,
            http_status: None,
            assistant_content: None,
            refusal: None,
            error_class: Some("transport"),
            error_message: Some(message),
            retryable: Some(true),
            usage: None,
        }
    }

    /// A timeout, which the gate retries once within its operation deadline.
    pub const fn timeout(command: String) -> Self {
        Self {
            command,
            http_status: None,
            assistant_content: None,
            refusal: None,
            error_class: Some("timeout"),
            error_message: Some("judge timed out"),
            retryable: Some(true),
            usage: None,
        }
    }
}

/// A JSONL recorder shared by every attempt of one judge client.
///
/// Writes are serialized by a mutex and flushed per line, so a failed or
/// interrupted corpus run still leaves the completed attempts on disk.
pub struct JudgeRecord {
    path: PathBuf,
    writer: Mutex<BufWriter<File>>,
    entries: AtomicUsize,
}

impl std::fmt::Debug for JudgeRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JudgeRecord")
            .field("path", &self.path)
            .field("entries", &self.entries.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl JudgeRecord {
    /// Create or truncate a record file at `path` with owner-only permissions.
    #[cfg(test)]
    pub fn create(path: &Path) -> std::io::Result<Self> {
        use std::fs::OpenOptions;

        let mut options = OpenOptions::new();
        options.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self {
            path: path.to_path_buf(),
            writer: Mutex::new(BufWriter::new(file)),
            entries: AtomicUsize::new(0),
        })
    }

    /// Append one provider outcome and flush it immediately.
    pub fn append(&self, entry: &JudgeRecordEntry) -> std::io::Result<()> {
        let line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
        let mut writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        writeln!(writer, "{line}")?;
        writer.flush()?;
        drop(writer);
        self.entries.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Number of successfully written entries.
    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.entries.load(Ordering::Relaxed)
    }

    /// Flush any buffered output. `append` already flushes each line; this is
    /// the final check that the file is still writable.
    #[cfg(test)]
    pub fn flush(&self) -> std::io::Result<()> {
        self.writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .flush()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_writes_jsonl_and_truncates_existing_content() {
        let directory = tempfile::TempDir::new().unwrap();
        let path = directory.path().join("record.jsonl");
        std::fs::write(&path, "old content\n").unwrap();
        let record = JudgeRecord::create(&path).unwrap();

        record
            .append(&JudgeRecordEntry::success(
                "git status --short".to_string(),
                Some(200),
                Some("{\"verdict\":\"allow\"}".to_string()),
                None,
            ))
            .unwrap();
        record
            .append(&JudgeRecordEntry::http_error("rm -rf src".to_string(), 503))
            .unwrap();

        assert_eq!(record.count(), 2);
        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["command"], "git status --short");
        assert_eq!(first["http_status"], 200);
        assert_eq!(first["assistant_content"], "{\"verdict\":\"allow\"}");
        assert!(first.get("error_class").is_none());
        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["command"], "rm -rf src");
        assert_eq!(second["http_status"], 503);
        assert!(second.get("assistant_content").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn record_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::TempDir::new().unwrap();
        let path = directory.path().join("record.jsonl");
        let _record = JudgeRecord::create(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
