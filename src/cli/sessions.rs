use std::fmt::Write;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::cli::CmdRunner;
use crate::config::DataDir;
use crate::config::session::CURRENT_FORMAT_VERSION;

/// Session browsing commands.
#[derive(Clone, Debug, Parser)]
pub struct SessionsCommand {
    #[command(subcommand)]
    command: SessionsSubcommand,
}

#[derive(Clone, Debug, Subcommand)]
enum SessionsSubcommand {
    /// List sessions for the current working directory
    List {
        /// Output session list as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

impl CmdRunner for SessionsCommand {
    async fn run(&self, data_dir: &DataDir) -> anyhow::Result<()> {
        match &self.command {
            SessionsSubcommand::List { json } => {
                let current_dir = std::env::current_dir()
                    .map_err(|e| anyhow::anyhow!("Failed to get current directory: {e}"))?;
                let sessions = list_sessions(data_dir, &current_dir)?;
                print!("{}", render_sessions(&sessions, *json)?);
                Ok(())
            },
        }
    }
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

/// Scan session files, filter by working directory, and return sorted session info.
fn list_sessions(data_dir: &DataDir, working_dir: &Path) -> anyhow::Result<Vec<SessionInfo>> {
    let sessions_dir = data_dir.sessions_dir();
    if !sessions_dir.exists() {
        return Ok(Vec::new());
    }

    let mut sessions: Vec<SessionInfo> = Vec::new();

    let entries = std::fs::read_dir(&sessions_dir)
        .map_err(|e| anyhow::anyhow!("Failed to read sessions directory: {e}"))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "jsonl")
            && let Some(info) = read_session_info(&path, working_dir)
        {
            sessions.push(info);
        }
    }

    // Sort newest first
    sessions.sort_by_key(|b| std::cmp::Reverse(b.timestamp));

    Ok(sessions)
}

/// Minimal header struct parsed from the first line of a session file.
#[derive(serde::Deserialize)]
struct SessionFileHeader {
    format_version: u32,
    working_directory: PathBuf,
    timestamp: DateTime<Utc>,
}

/// Read a session file's metadata and first user prompt, returning `None`
/// when the working directory does not match or the file is unreadable.
fn read_session_info(path: &Path, working_dir: &Path) -> Option<SessionInfo> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let mut first_line = String::new();
    reader.read_line(&mut first_line).ok()?;

    let header: SessionFileHeader = serde_json::from_str(first_line.trim()).ok()?;
    if header.format_version != CURRENT_FORMAT_VERSION {
        return None;
    }
    if header.working_directory != working_dir {
        return None;
    }

    // Extract session_id from the first line (it's in the JSON but SessionFileHeader
    // doesn't carry it — re-parse as serde_json::Value to get it without changing the header).
    let first_value: serde_json::Value = serde_json::from_str(first_line.trim()).ok()?;
    let session_id = first_value
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(ToString::to_string)?;

    // Scan subsequent lines for the first user message to get the first prompt.
    let first_prompt = find_first_user_prompt(&mut reader);

    Some(SessionInfo {
        session_id,
        timestamp: header.timestamp,
        first_prompt: first_prompt.unwrap_or_default(),
    })
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

/// Render the session list as a formatted table or JSON.
fn render_sessions(sessions: &[SessionInfo], json: bool) -> anyhow::Result<String> {
    if json {
        render_sessions_json(sessions)
    } else {
        Ok(format_sessions_table(sessions))
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

fn render_sessions_json(sessions: &[SessionInfo]) -> anyhow::Result<String> {
    serde_json::to_string_pretty(sessions)
        .map_err(|e| anyhow::anyhow!("Failed to serialize sessions: {e}"))
}

/// Truncate a prompt string to fit within the given character limit,
/// appending an ellipsis when truncated.
fn truncate_prompt(prompt: &str, max_len: usize) -> String {
    if prompt.len() <= max_len {
        prompt.to_string()
    } else {
        let mut truncated = prompt
            .chars()
            .take(max_len.saturating_sub(1))
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

        let sessions = list_sessions(&dd, tmp.path()).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].session_id,
            "11111111-1111-4111-8111-111111111111"
        );
    }

    #[test]
    fn list_sessions_returns_empty_when_no_sessions_dir() {
        let (dd, _tmp) = make_data_dir();
        let fake_dir = PathBuf::from("/nonexistent");
        let sessions = list_sessions(&dd, &fake_dir).unwrap();
        assert!(sessions.is_empty());
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

        let sessions = list_sessions(&dd, tmp.path()).unwrap();
        assert!(sessions.is_empty());
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

        let sessions = list_sessions(&dd, tmp.path()).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(
            sessions[0].session_id,
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        );
        assert_eq!(
            sessions[1].session_id,
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

        let sessions = list_sessions(&dd, tmp.path()).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].first_prompt, "First line");
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

        let sessions = list_sessions(&dd, tmp.path()).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].first_prompt, "");
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
        let output = super::render_sessions_json(&sessions).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["session_id"], "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
        assert_eq!(arr[0]["first_prompt"], "Test prompt");
    }

    #[test]
    fn truncate_prompt_short() {
        assert_eq!(truncate_prompt("hello", 10), "hello");
    }

    #[test]
    fn truncate_prompt_long() {
        let result = truncate_prompt("this is a very long prompt string", 10);
        assert_eq!(result, "this is a…");
        assert!(result.len() <= 13); // 9 chars + 3-byte ellipsis
    }

    #[test]
    fn truncate_prompt_empty() {
        assert_eq!(truncate_prompt("", 10), "");
    }
}
