use std::fmt::Write;
use std::io::BufRead;
use std::path::Path;

use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::cli::checkpoint::checkpoint_command;
use crate::cli::{
    CHECK_SESSIONS_DIRECTORY, CHECK_SESSIONS_DISCOVERY, CmdRunner, CommandRunOptions,
    DiagnosticCheck, DiagnosticDocument, report_failure,
};
use crate::config::DataDir;
use crate::config::session_jsonl::{HeaderError, SessionHeader};

/// Maximum number of discovery failures reported by one listing. The last
/// retained warning names any that were omitted.
const MAX_DISCOVERY_WARNINGS: usize = 8;

/// Session browsing commands.
#[derive(Clone, Debug, Parser)]
#[command(after_help = "\
Examples:
  cake sessions list --json | jq -r '.data.sessions[].session_id'
  cake --resume \"$(cake sessions list --json | jq -r '.data.sessions[0].session_id')\" \"continue\"")]
pub struct SessionsCommand {
    #[command(subcommand)]
    command: SessionsSubcommand,
}

#[derive(Clone, Debug, Subcommand)]
enum SessionsSubcommand {
    /// List sessions for the current working directory
    #[command(after_help = "\
Examples:
  cake sessions list
  cake sessions list --json | jq -r '.data.sessions[].session_id'")]
    List {
        /// Output the session list as one diagnostic JSON document
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Append one context-compaction checkpoint to an existing session
    #[command(after_help = "\
Examples:
  cake sessions checkpoint 550e8400-e29b-41d4-a716-446655440000 < checkpoint.json
  cake-compact compact --json | cake sessions checkpoint \"$SESSION\" --json

Reads one JSON payload from stdin. Cake owns the record envelope, the
checkpoint and session ids, the timestamp, the boundary, the lock, validation,
and the append. The caller owns the summary, the retained tail, the trigger,
the token estimates, and the optional summary_usage and summary_time_ms
measurement fields.")]
    Checkpoint {
        /// Session UUID to append the checkpoint to
        #[arg(value_name = "UUID")]
        uuid: String,
        /// Output the append as one diagnostic JSON document
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

impl CmdRunner for SessionsCommand {
    async fn run(
        &self,
        data_dir: &DataDir,
        _options: &CommandRunOptions<'_>,
    ) -> anyhow::Result<()> {
        std::future::ready(()).await;
        match &self.command {
            SessionsSubcommand::List { json } => list_sessions_command(data_dir, *json),
            SessionsSubcommand::Checkpoint { uuid, json } => {
                checkpoint_command(data_dir, uuid, *json)
            },
        }
    }
}

/// List the sessions for the current working directory as a table, or as one
/// diagnostic document when the machine flag is set.
fn list_sessions_command(data_dir: &DataDir, json: bool) -> anyhow::Result<()> {
    let current_dir = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("Failed to get current directory: {e}"))?;
    let listing = list_sessions(data_dir, &current_dir)
        .map_err(|error| report_failure(json, "sessions list", CHECK_SESSIONS_DIRECTORY, error))?;
    if !json {
        for warning in listing.warning_messages() {
            eprintln!("warning: {warning}");
        }
    }
    print!("{}", render_sessions(&listing, json)?);
    Ok(())
}

/// Lightweight session info extracted from a session file without loading
/// the full conversation history.
#[derive(Debug, Serialize)]
struct SessionInfo {
    /// Session UUID
    session_id: String,
    /// Timestamp from `session_meta`
    timestamp: DateTime<Utc>,
    /// The first user message content (truncated to a single line)
    first_prompt: String,
}

/// The sessions found for one working directory, plus discovery warnings.
#[derive(Debug, Default)]
struct SessionListing {
    sessions: Vec<SessionInfo>,
    warnings: Vec<String>,
}

impl SessionListing {
    fn warn_file(&mut self, path: &Path, error: &anyhow::Error) {
        self.warnings.push(format!(
            "could not inspect session file {}: {error:#}",
            path.display()
        ));
    }

    fn warn_entry(&mut self, directory: &Path, error: &std::io::Error) {
        self.warnings.push(format!(
            "could not inspect sessions directory entry from {}: {error}",
            directory.display()
        ));
    }

    /// Sorted, bounded warnings. The last retained warning names any omitted
    /// failures so a truncated list is still diagnosable.
    fn warning_messages(&self) -> Vec<String> {
        let mut messages = self.warnings.clone();
        messages.sort();
        if messages.len() > MAX_DISCOVERY_WARNINGS {
            let omitted = messages.len() - MAX_DISCOVERY_WARNINGS;
            messages.truncate(MAX_DISCOVERY_WARNINGS);
            let omitted_note = format!("; {omitted} more discovery failures not shown");
            if let Some(last) = messages.last_mut() {
                last.push_str(&omitted_note);
            }
        }
        messages
    }
}

/// Scan session files, filter by working directory, and return sorted session
/// info plus warnings for files that could not be inspected. Working-directory
/// filtering is intentional and stays quiet.
fn list_sessions(data_dir: &DataDir, working_dir: &Path) -> anyhow::Result<SessionListing> {
    let sessions_dir = data_dir.sessions_dir();
    let mut listing = SessionListing::default();

    let entries = match std::fs::read_dir(&sessions_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(listing),
        Err(error) => {
            return Err(anyhow::anyhow!(
                "Failed to read sessions directory: {error}"
            ));
        },
    };

    for entry in entries {
        match entry {
            Ok(entry) => collect_session(&mut listing, &entry.path(), working_dir),
            Err(error) => listing.warn_entry(&sessions_dir, &error),
        }
    }

    listing.sessions.sort_by(|left, right| {
        right
            .timestamp
            .cmp(&left.timestamp)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });

    Ok(listing)
}

/// Inspect one directory entry, recording a session or a discovery warning.
fn collect_session(listing: &mut SessionListing, path: &Path, working_dir: &Path) {
    if path.extension().is_none_or(|ext| ext != "jsonl") {
        return;
    }
    match read_session_info(path, working_dir) {
        Ok(Some(info)) => listing.sessions.push(info),
        Ok(None) => {},
        Err(error) => listing.warn_file(path, &error),
    }
}

/// Read a session file's metadata and first user prompt. `Ok(None)` means the
/// file is valid but belongs to another working directory; every other
/// discovery failure is returned with its cause.
fn read_session_info(path: &Path, working_dir: &Path) -> anyhow::Result<Option<SessionInfo>> {
    let file = std::fs::File::open(path)
        .map_err(|error| anyhow::anyhow!("failed to open session file: {error}"))?;
    let mut reader = std::io::BufReader::new(file);

    let header = SessionHeader::read(&mut reader).map_err(header_error)?;
    if header.working_directory != working_dir {
        return Ok(None);
    }

    let first_prompt = find_first_user_prompt(&mut reader);

    Ok(Some(SessionInfo {
        session_id: header.session_id,
        timestamp: header.timestamp,
        first_prompt: first_prompt.unwrap_or_default(),
    }))
}

fn header_error(error: HeaderError) -> anyhow::Error {
    match error {
        HeaderError::Empty => anyhow::anyhow!("session file is empty"),
        HeaderError::NotUtf8 => anyhow::anyhow!("session file header is not valid UTF-8"),
        HeaderError::Read(source) => {
            anyhow::anyhow!("failed to read session header: {source}")
        },
        HeaderError::NotSessionMeta => anyhow::anyhow!(
            "unsupported or legacy session file format: expected session_meta as first record"
        ),
        HeaderError::UnsupportedVersion { found, expected } => {
            anyhow::anyhow!("Unsupported session format_version: {found} (expected {expected})")
        },
        HeaderError::Parse(source) => {
            anyhow::anyhow!("failed to parse session header: {source}")
        },
    }
}

/// Read lines after the `session_meta` to find the first `message` with `role: "user"`.
/// Returns the content text (first line only, trimmed).
fn find_first_user_prompt(reader: &mut std::io::BufReader<std::fs::File>) -> Option<String> {
    let mut line = String::new();
    while reader.read_line(&mut line).ok()? > 0 {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            line.clear();
            continue;
        }

        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed)
            && value.get("type").and_then(|v| v.as_str()) == Some("message")
            && value.get("role").and_then(|v| v.as_str()) == Some("user")
            && let Some(content) = value.get("content").and_then(|v| v.as_str())
        {
            let first_line = content.lines().next().unwrap_or(content).to_string();
            return Some(first_line);
        }

        line.clear();
    }
    None
}

/// Render the session list as a formatted table or one diagnostic document.
fn render_sessions(listing: &SessionListing, json: bool) -> anyhow::Result<String> {
    if json {
        render_sessions_json(listing)
    } else {
        Ok(format_sessions_table(&listing.sessions))
    }
}

fn format_sessions_table(sessions: &[SessionInfo]) -> String {
    if sessions.is_empty() {
        return "No sessions found for this directory.\n".to_string();
    }

    let mut output = String::new();
    output.push_str("Sessions\n");
    output.push_str("-------\n");
    output.push('\n');

    for session in sessions {
        let date = session.timestamp.format("%Y-%m-%d %H:%M:%S UTC");
        let prompt_line = truncate_prompt(&session.first_prompt, 72);
        let _ = writeln!(output, "{date}  {prompt_line}").ok();
        let _ = writeln!(output, "        {}", session.session_id).ok();
        output.push('\n');
    }

    output
}

/// Render the session list as one diagnostic document.
///
/// The order is the listing order, so the document is stable for a given set of
/// session files.
fn render_sessions_json(listing: &SessionListing) -> anyhow::Result<String> {
    let checks = listing
        .warning_messages()
        .into_iter()
        .map(|message| DiagnosticCheck::warning(CHECK_SESSIONS_DISCOVERY, message))
        .collect();
    DiagnosticDocument::new(
        "sessions list",
        serde_json::json!({ "count": listing.sessions.len() }),
        checks,
        serde_json::json!({ "sessions": &listing.sessions }),
    )
    .render()
}

/// Truncate a prompt string to fit within the given limit, appending an
/// ellipsis when truncated.
///
/// `max_chars` counts Unicode scalar values, not bytes, so multi-byte
/// prompts (emoji, CJK) are cut to the same number of characters as ASCII
/// ones and the result is always valid UTF-8.
fn truncate_prompt(prompt: &str, max_chars: usize) -> String {
    if prompt.chars().count() <= max_chars {
        prompt.to_string()
    } else {
        let mut truncated = prompt
            .chars()
            .take(max_chars.saturating_sub(1))
            .collect::<String>();
        truncated.push('…');
        truncated
    }
}
