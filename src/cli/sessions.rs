use std::fmt::Write;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::cli::checkpoint::checkpoint_command;
use crate::cli::{
    CHECK_SESSIONS_DIRECTORY, CHECK_SESSIONS_DISCOVERY, CmdRunner, CommandRunOptions,
    DiagnosticCheck, DiagnosticDocument, report_failure,
};
use crate::config::DataDir;
use crate::config::session::CURRENT_FORMAT_VERSION;
use crate::config::session_jsonl::SessionFramer;

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
        // CmdRunner is asynchronous, although this command has no async work.
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

    // Sort newest first, with the session id as the tiebreaker so two sessions
    // written in the same second always list in the same order.
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

/// Minimal header struct parsed from the first line of a session file.
#[derive(serde::Deserialize)]
struct SessionFileHeader {
    format_version: u32,
    session_id: String,
    working_directory: PathBuf,
    timestamp: DateTime<Utc>,
}

/// Read a session file's metadata and first user prompt. `Ok(None)` means the
/// file is valid but belongs to another working directory; every other
/// discovery failure is returned with its cause.
fn read_session_info(path: &Path, working_dir: &Path) -> anyhow::Result<Option<SessionInfo>> {
    let file = std::fs::File::open(path)
        .map_err(|error| anyhow::anyhow!("failed to open session file: {error}"))?;
    let mut reader = std::io::BufReader::new(file);

    let header = read_header(&mut reader)?;
    if header.working_directory != working_dir {
        return Ok(None);
    }

    // Scan subsequent lines for the first user message to get the first prompt.
    let first_prompt = find_first_user_prompt(&mut reader);

    Ok(Some(SessionInfo {
        session_id: header.session_id,
        timestamp: header.timestamp,
        first_prompt: first_prompt.unwrap_or_default(),
    }))
}

/// Parse the `session_meta` header line once, rejecting unsupported versions.
fn read_header(reader: &mut impl std::io::BufRead) -> anyhow::Result<SessionFileHeader> {
    let header_line = {
        let mut framer = SessionFramer::new(&mut *reader);
        framer
            .next_record()
            .map_err(|error| anyhow::anyhow!("failed to read session header: {error}"))?
            .ok_or_else(|| anyhow::anyhow!("session file is empty"))?
            .text
    };

    let header: SessionFileHeader = serde_json::from_str(&header_line)
        .map_err(|error| anyhow::anyhow!("failed to parse session header: {error}"))?;
    if header.format_version != CURRENT_FORMAT_VERSION {
        return Err(anyhow::anyhow!(
            "Unsupported session format_version: {} (expected {})",
            header.format_version,
            CURRENT_FORMAT_VERSION
        ));
    }

    Ok(header)
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

        // Quick check: is this a message record with role "user"?
        // We use serde_json::Value to avoid needing the full SessionRecord deserialization.
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed)
            && value.get("type").and_then(|v| v.as_str()) == Some("message")
            && value.get("role").and_then(|v| v.as_str()) == Some("user")
            && let Some(content) = value.get("content").and_then(|v| v.as_str())
        {
            // Return the first line of the content
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    use crate::config::session::CURRENT_FORMAT_VERSION;

    /// Create a temporary `DataDir` and return it along with the temp dir guard.
    fn make_data_dir() -> (DataDir, TempDir) {
        let tmp = TempDir::new().unwrap();
        let dd = DataDir::new_in_dir(tmp.path());
        (dd, tmp)
    }

    fn write_session(
        sessions_dir: &Path,
        session_id: &str,
        working_dir: &Path,
        timestamp: &str,
        user_prompt: Option<&str>,
    ) {
        let path = sessions_dir.join(format!("{session_id}.jsonl"));
        let meta = serde_json::json!({
            "type": "session_meta",
            "format_version": CURRENT_FORMAT_VERSION,
            "session_id": session_id,
            "timestamp": timestamp,
            "working_directory": working_dir,
            "model": "test-model",
            "tools": ["bash", "read"],
            "cake_version": "0.1.0",
            "system_prompt": "test system prompt",
            "git": {
                "repository_url": null,
                "branch": null,
                "commit_hash": null
            }
        });

        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "{}", serde_json::to_string(&meta).unwrap()).unwrap();

        // Write task start
        let task_start = serde_json::json!({
            "type": "task_start",
            "session_id": session_id,
            "task_id": "test-task",
            "timestamp": timestamp
        });
        writeln!(file, "{}", serde_json::to_string(&task_start).unwrap()).unwrap();

        // Write user prompt if given
        if let Some(prompt) = user_prompt {
            let msg = serde_json::json!({
                "type": "message",
                "role": "user",
                "content": prompt,
            });
            writeln!(file, "{}", serde_json::to_string(&msg).unwrap()).unwrap();
        }
    }

    #[test]
    fn list_sessions_skips_leading_blank_lines() {
        // Regression for #275: a blank line before `session_meta` must not hide
        // the header during listing, matching full load, replay, and discovery.
        let (dd, tmp) = make_data_dir();
        let sessions_dir = dd.sessions_dir();
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let session_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        write_session(
            &sessions_dir,
            session_id,
            tmp.path(),
            "2026-07-09T12:00:00Z",
            Some("Leading blank header"),
        );
        let path = sessions_dir.join(format!("{session_id}.jsonl"));
        let content = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("\n\n{content}")).unwrap();

        let listing = list_sessions(&dd, tmp.path()).unwrap();
        assert_eq!(listing.sessions.len(), 1);
        assert_eq!(listing.sessions[0].session_id, session_id);
        assert_eq!(listing.sessions[0].first_prompt, "Leading blank header");
        assert!(listing.warnings.is_empty());
    }

    #[test]
    fn list_sessions_filters_by_working_directory() {
        let (dd, tmp) = make_data_dir();
        let sessions_dir = dd.sessions_dir();
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let ts = "2026-07-09T12:00:00Z";
        write_session(
            &sessions_dir,
            "11111111-1111-4111-8111-111111111111",
            tmp.path(),
            ts,
            Some("Implement feature X"),
        );
        write_session(
            &sessions_dir,
            "22222222-2222-4222-8222-222222222222",
            &tmp.path().join("other"),
            ts,
            Some("Other dir prompt"),
        );

        let listing = list_sessions(&dd, tmp.path()).unwrap();
        assert_eq!(listing.sessions.len(), 1);
        assert_eq!(
            listing.sessions[0].session_id,
            "11111111-1111-4111-8111-111111111111"
        );
        assert!(
            listing.warnings.is_empty(),
            "working-directory filtering must stay quiet: {:?}",
            listing.warnings
        );
    }

    #[test]
    fn list_sessions_reports_discovery_failures_with_path_and_reason() {
        let (dd, tmp) = make_data_dir();
        let sessions_dir = dd.sessions_dir();
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let ts = "2026-07-09T12:00:00Z";
        write_session(
            &sessions_dir,
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            tmp.path(),
            ts,
            Some("valid session"),
        );
        write_session(
            &sessions_dir,
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            tmp.path(),
            ts,
            Some("unsupported session"),
        );

        let unsupported_path = sessions_dir.join("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb.jsonl");
        let unsupported = std::fs::read_to_string(&unsupported_path).unwrap().replace(
            &format!("\"format_version\":{CURRENT_FORMAT_VERSION}"),
            &format!("\"format_version\":{}", CURRENT_FORMAT_VERSION + 1),
        );
        std::fs::write(&unsupported_path, unsupported).unwrap();

        let malformed_path = sessions_dir.join("malformed.jsonl");
        std::fs::write(&malformed_path, "{\"type\":\"session_meta\"\n").unwrap();

        let unreadable_path = sessions_dir.join("unreadable.jsonl");
        std::fs::create_dir_all(&unreadable_path).unwrap();

        let listing = list_sessions(&dd, tmp.path()).unwrap();
        assert_eq!(listing.sessions.len(), 1);
        let warnings = listing.warning_messages();
        assert_eq!(warnings.len(), 3);

        let malformed_warning = warnings
            .iter()
            .find(|warning| warning.contains(&malformed_path.display().to_string()))
            .expect("malformed session warning");
        assert!(
            malformed_warning.contains("failed to parse session header"),
            "malformed warning must name the parse failure: {malformed_warning}"
        );

        let unsupported_warning = warnings
            .iter()
            .find(|warning| warning.contains(&unsupported_path.display().to_string()))
            .expect("unsupported session warning");
        assert!(
            unsupported_warning.contains("Unsupported session format_version"),
            "unsupported warning must name the format failure: {unsupported_warning}"
        );

        let unreadable_warning = warnings
            .iter()
            .find(|warning| warning.contains(&unreadable_path.display().to_string()))
            .expect("unreadable session warning");
        assert!(
            unreadable_warning.contains("failed to open session file")
                || unreadable_warning.contains("failed to read session header"),
            "unreadable warning must name the read failure: {unreadable_warning}"
        );
    }

    #[test]
    fn list_sessions_bounds_discovery_warnings() {
        let (dd, tmp) = make_data_dir();
        let sessions_dir = dd.sessions_dir();
        std::fs::create_dir_all(&sessions_dir).unwrap();

        for index in 0..(MAX_DISCOVERY_WARNINGS + 2) {
            std::fs::write(
                sessions_dir.join(format!("broken-{index:02}.jsonl")),
                "not a session header\n",
            )
            .unwrap();
        }

        let listing = list_sessions(&dd, tmp.path()).unwrap();
        assert_eq!(listing.warnings.len(), MAX_DISCOVERY_WARNINGS + 2);
        let warnings = listing.warning_messages();
        assert_eq!(warnings.len(), MAX_DISCOVERY_WARNINGS);
        assert!(
            warnings
                .last()
                .unwrap()
                .contains("2 more discovery failures not shown"),
            "the bounded list must name the omitted count: {warnings:?}"
        );
        assert_eq!(
            warnings,
            list_sessions(&dd, tmp.path()).unwrap().warning_messages(),
            "warning order must not depend on directory iteration"
        );
    }

    #[test]
    fn list_sessions_returns_empty_when_no_sessions_dir() {
        let (dd, _tmp) = make_data_dir();
        let fake_dir = PathBuf::from("/nonexistent");
        let listing = list_sessions(&dd, &fake_dir).unwrap();
        assert!(listing.sessions.is_empty());
        assert!(listing.warnings.is_empty());
    }

    #[test]
    fn list_sessions_returns_empty_when_no_sessions_match() {
        let (dd, tmp) = make_data_dir();
        let sessions_dir = dd.sessions_dir();
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let ts = "2026-07-09T12:00:00Z";
        write_session(
            &sessions_dir,
            "11111111-1111-4111-8111-111111111111",
            &tmp.path().join("different"),
            ts,
            None,
        );

        let listing = list_sessions(&dd, tmp.path()).unwrap();
        assert!(listing.sessions.is_empty());
        assert!(listing.warnings.is_empty());
    }

    #[test]
    fn list_sessions_sorts_newest_first() {
        let (dd, tmp) = make_data_dir();
        let sessions_dir = dd.sessions_dir();
        std::fs::create_dir_all(&sessions_dir).unwrap();

        write_session(
            &sessions_dir,
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            tmp.path(),
            "2026-07-08T12:00:00Z",
            Some("Older prompt"),
        );
        write_session(
            &sessions_dir,
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            tmp.path(),
            "2026-07-09T12:00:00Z",
            Some("Newer prompt"),
        );

        let listing = list_sessions(&dd, tmp.path()).unwrap();
        assert_eq!(listing.sessions.len(), 2);
        assert_eq!(
            listing.sessions[0].session_id,
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        );
        assert_eq!(
            listing.sessions[1].session_id,
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        );
    }

    #[test]
    fn list_sessions_finds_first_user_prompt() {
        let (dd, tmp) = make_data_dir();
        let sessions_dir = dd.sessions_dir();
        std::fs::create_dir_all(&sessions_dir).unwrap();

        write_session(
            &sessions_dir,
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            tmp.path(),
            "2026-07-09T12:00:00Z",
            Some("First line\nSecond line\nThird line"),
        );

        let listing = list_sessions(&dd, tmp.path()).unwrap();
        assert_eq!(listing.sessions.len(), 1);
        assert_eq!(listing.sessions[0].first_prompt, "First line");
    }

    #[test]
    fn list_sessions_handles_session_without_user_prompt() {
        let (dd, tmp) = make_data_dir();
        let sessions_dir = dd.sessions_dir();
        std::fs::create_dir_all(&sessions_dir).unwrap();

        write_session(
            &sessions_dir,
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            tmp.path(),
            "2026-07-09T12:00:00Z",
            None,
        );

        let listing = list_sessions(&dd, tmp.path()).unwrap();
        assert_eq!(listing.sessions.len(), 1);
        assert_eq!(listing.sessions[0].first_prompt, "");
    }

    #[test]
    fn format_sessions_table_empty() {
        let output = format_sessions_table(&[]);
        assert_eq!(output, "No sessions found for this directory.\n");
    }

    #[test]
    fn format_sessions_table_non_empty() {
        let sessions = vec![SessionInfo {
            session_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_string(),
            timestamp: DateTime::parse_from_rfc3339("2026-07-09T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            first_prompt: "Implement feature X".to_string(),
        }];
        let output = format_sessions_table(&sessions);
        assert!(output.contains("2026-07-09 12:00:00 UTC"));
        assert!(output.contains("Implement feature X"));
        assert!(output.contains("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"));
    }

    #[test]
    fn render_sessions_json() {
        let sessions = vec![SessionInfo {
            session_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_string(),
            timestamp: DateTime::parse_from_rfc3339("2026-07-09T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            first_prompt: "Test prompt".to_string(),
        }];
        let output = super::render_sessions_json(&SessionListing {
            sessions,
            warnings: Vec::new(),
        })
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(parsed["schema_version"], 1);
        assert_eq!(parsed["command"], "sessions list");
        assert_eq!(parsed["status"], "ok");
        assert_eq!(parsed["summary"]["count"], 1);
        assert_eq!(parsed["checks"], serde_json::json!([]));
        let listed = parsed["data"]["sessions"].as_array().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0]["session_id"],
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        );
        assert_eq!(listed[0]["first_prompt"], "Test prompt");
    }

    #[test]
    fn render_sessions_json_reports_discovery_warnings() {
        let listing = SessionListing {
            sessions: Vec::new(),
            warnings: vec![
                "could not inspect session file /tmp/bad.jsonl: failed to parse session header"
                    .to_string(),
            ],
        };

        let output = super::render_sessions_json(&listing).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(parsed["status"], "warning");
        assert_eq!(parsed["summary"]["count"], 0);
        assert_eq!(parsed["checks"][0]["id"], CHECK_SESSIONS_DISCOVERY);
        assert_eq!(parsed["checks"][0]["status"], "warning");
        assert_eq!(
            parsed["checks"][0]["message"],
            "could not inspect session file /tmp/bad.jsonl: failed to parse session header"
        );
    }

    #[test]
    fn list_sessions_orders_equal_timestamps_by_session_id() {
        // Directory iteration order is arbitrary, so two sessions written in
        // the same second need a tiebreaker for a stable listing.
        let (dd, tmp) = make_data_dir();
        let sessions_dir = dd.sessions_dir();
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let ts = "2026-07-09T12:00:00Z";
        for (id, prompt) in [
            ("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", "Second"),
            ("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "First"),
        ] {
            write_session(&sessions_dir, id, tmp.path(), ts, Some(prompt));
        }

        let listing = list_sessions(&dd, tmp.path()).unwrap();
        assert_eq!(listing.sessions.len(), 2);
        assert_eq!(
            listing.sessions[0].session_id,
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        );
        assert_eq!(
            listing.sessions[1].session_id,
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        );
    }

    #[test]
    fn truncate_prompt_long_ascii() {
        let result = truncate_prompt("this is a very long prompt string", 10);
        assert_eq!(result, "this is a…");
        assert_eq!(result.chars().count(), 10);
    }

    #[test]
    fn truncate_prompt_short_prompt_is_unchanged() {
        let result = truncate_prompt("short", 10);
        assert_eq!(result, "short");
        assert!(!result.contains('…'));
    }

    #[test]
    fn truncate_prompt_at_exact_character_boundary_is_unchanged() {
        // Exactly the limit in characters, but well over it in bytes: the
        // character limit must not append an ellipsis here.
        let prompt = "日本語のプロンプト";
        assert_eq!(prompt.chars().count(), 9);
        assert!(prompt.len() > 9);
        let result = truncate_prompt(prompt, 9);
        assert_eq!(result, prompt);
        assert!(!result.contains('…'));
    }

    #[test]
    fn truncate_prompt_multibyte_cjk_uses_character_limit() {
        let prompt = "日本語のプロンプトです";
        let result = truncate_prompt(prompt, 5);
        assert_eq!(result, "日本語の…");
        assert_eq!(result.chars().count(), 5);
        // Truncation stops at a valid UTF-8 boundary.
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
    }

    #[test]
    fn truncate_prompt_multibyte_emoji_uses_character_limit() {
        let prompt = "🦀🦀🦀🦀🦀🦀";
        assert!(prompt.len() > 4);
        let result = truncate_prompt(prompt, 4);
        assert_eq!(result, "🦀🦀🦀…");
        assert_eq!(result.chars().count(), 4);
    }

    #[test]
    fn format_sessions_table_truncates_multibyte_prompt_without_breaking_alignment() {
        let long_prompt = "日本語のとても長いプロンプト".repeat(10);
        let sessions = vec![SessionInfo {
            session_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_string(),
            timestamp: DateTime::parse_from_rfc3339("2026-07-09T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            first_prompt: long_prompt,
        }];
        let output = format_sessions_table(&sessions);
        let prompt_line = output
            .lines()
            .find(|line| line.starts_with("2026-07-09"))
            .expect("timestamped prompt line");
        let prompt_part = prompt_line
            .strip_prefix("2026-07-09 12:00:00 UTC  ")
            .expect("date prefix");
        assert_eq!(prompt_part.chars().count(), 72);
        assert!(prompt_part.ends_with('…'));
    }

    #[test]
    fn render_sessions_json_keeps_full_multibyte_prompt() {
        let prompt = "日本語のとても長いプロンプト".repeat(10);
        let sessions = vec![SessionInfo {
            session_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_string(),
            timestamp: DateTime::parse_from_rfc3339("2026-07-09T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            first_prompt: prompt.clone(),
        }];
        let output = super::render_sessions_json(&SessionListing {
            sessions,
            warnings: Vec::new(),
        })
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["data"]["sessions"][0]["first_prompt"], prompt);
    }
}
