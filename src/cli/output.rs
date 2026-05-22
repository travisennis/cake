//! Output formatting and rendering for CLI responses.
//!
//! This module provides the [`CliOutputSink`] for rendering LLM responses to the
//! terminal in text, JSON, or stream-JSON format, as well as the [`TurnResult`]
//! struct used to carry a single agent-turn outcome and its duration.

use std::path::Path;
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

use crate::OutputFormat;
use crate::clients::retry::{RetryReason, RetryStatus};
use crate::clients::{Agent, summarize_tool_args};
use crate::config::{DataDir, Session};
use crate::time_format::{format_duration_tenths, format_seconds_tenths};
use crate::types::{ConversationItem, Role};

/// Outcome of a single agent turn, bundling the result with its elapsed time.
pub struct TurnResult {
    pub(crate) result: anyhow::Result<Option<String>>,
    pub(crate) duration_ms: u64,
}

/// Pure-rendering sink for CLI output.
///
/// Dispatches responses to the appropriate output format (text, JSON, or
/// stream-JSON) and manages text-mode progress reporting.
#[derive(Clone, Copy)]
pub struct CliOutputSink {
    format: OutputFormat,
}

impl CliOutputSink {
    pub(crate) const fn new(format: OutputFormat) -> Self {
        Self { format }
    }

    pub(crate) fn attach_callbacks(self, mut client: Agent) -> (Agent, Option<ProgressBar>) {
        if self.format == OutputFormat::StreamJson {
            client = client.with_streaming_json(Self::write_stream_record);
        }

        match self.format {
            OutputFormat::Text => {
                let (client, spinner) = Self::attach_text_progress(client);
                (client, Some(spinner))
            },
            OutputFormat::StreamJson | OutputFormat::Json => (client, None),
        }
    }

    /// Attach text-mode progress reporting to the agent and return its spinner.
    fn attach_text_progress(client: Agent) -> (Agent, ProgressBar) {
        let spinner = ProgressBar::new_spinner();
        let style = ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner());
        spinner.set_style(style);
        spinner.enable_steady_tick(Duration::from_millis(80));
        spinner.set_message("Thinking...");

        let spinner_clone = spinner.clone();
        let retry_spinner = spinner.clone();
        let client = client.with_progress_callback(move |item| {
            let msg = format_spinner_message(item);
            if let Some(msg) = msg {
                spinner_clone.set_message(msg);
            }
        });
        let client = client.with_retry_callback(move |status| {
            retry_spinner.set_message(format_retry_message(status));
        });

        (client, spinner)
    }

    pub(crate) fn finish_progress(
        self,
        spinner: Option<ProgressBar>,
        duration_ms: u64,
        client: &Agent,
    ) {
        if self.format == OutputFormat::Text
            && let Some(spinner) = spinner
        {
            let summary = format_done_summary(duration_ms, client);
            spinner.finish_with_message(format!("Done: {summary}"));
        }
    }

    pub(crate) fn render_turn(
        self,
        turn: TurnResult,
        client: &Agent,
        current_dir: &Path,
        data_dir: &DataDir,
        session: &Session,
        persists_session: bool,
    ) -> anyhow::Result<()> {
        let TurnResult {
            result,
            duration_ms,
        } = turn;

        match self.format {
            OutputFormat::Text => Self::render_text_result(result),
            OutputFormat::Json => {
                let json = Self::turn_result_json(
                    &result,
                    duration_ms,
                    client,
                    current_dir,
                    data_dir,
                    session,
                    persists_session,
                );
                Self::write_json_value(&json)?;
                result.map(|_| ())
            },
            OutputFormat::StreamJson => Ok(()),
        }
    }

    fn render_text_result(result: anyhow::Result<Option<String>>) -> anyhow::Result<()> {
        let response = result?;
        if let Some(response_text) = response {
            Self::write_text_response(&response_text);
        } else {
            Self::write_warning("No response received from the model. The task may be incomplete.");
        }
        Ok(())
    }

    pub(crate) fn turn_result_json(
        result: &anyhow::Result<Option<String>>,
        duration_ms: u64,
        client: &Agent,
        current_dir: &Path,
        data_dir: &DataDir,
        session: &Session,
        persists_session: bool,
    ) -> serde_json::Value {
        let session_file = if persists_session {
            serde_json::Value::String(
                data_dir
                    .session_path(session.id)
                    .to_string_lossy()
                    .to_string(),
            )
        } else {
            serde_json::Value::Null
        };
        let mut json = serde_json::json!({
            "session_id": client.session_id().to_string(),
            "usage": client.total_usage(),
            "cwd": current_dir.to_string_lossy(),
            "session_file": session_file,
            "turns": client.turn_count(),
            "elapsed_time": duration_ms,
        });

        match result {
            Ok(response_text) => {
                let result_text = response_text.as_deref().unwrap_or("");
                json["result"] = serde_json::json!(result_text);
            },
            Err(e) => {
                json["result"] = serde_json::Value::Null;
                json["error"] = serde_json::json!(e.to_string());
            },
        }

        json
    }

    pub(crate) fn write_stream_record(json: &str) {
        println!("{json}");
    }

    fn write_text_response(content: &str) {
        println!("{content}");
    }

    pub(crate) fn write_json_value(value: &serde_json::Value) -> anyhow::Result<()> {
        println!("{}", serde_json::to_string(value)?);
        Ok(())
    }

    pub(crate) fn write_warning(message: &str) {
        eprintln!("Warning: {message}");
    }

    pub(crate) fn write_error(error: &anyhow::Error) {
        eprintln!("Error: {error}");
    }
}

/// Format a completion summary with elapsed time, turns, and token usage.
pub fn format_done_summary(duration_ms: u64, client: &Agent) -> String {
    let secs = format_seconds_tenths(u128::from(duration_ms));
    let turns = client.turn_count();
    let usage = client.total_usage();
    let input_tokens = usage.input_tokens;
    let output_tokens = usage.output_tokens;
    let cached_reads_tokens = usage.input_tokens_details.cached_tokens;
    format!(
        "session {}, {secs}s, {turns} turns, {input_tokens} input tokens, {cached_reads_tokens} cached reads, {output_tokens} output tokens",
        client.session_id()
    )
}

/// Format a conversation item as a short spinner message for normal mode.
///
/// Returns `Some(message)` for items worth showing, `None` otherwise.
pub fn format_spinner_message(item: &ConversationItem) -> Option<String> {
    match item {
        ConversationItem::FunctionCall {
            name, arguments, ..
        } => {
            let summary = summarize_tool_args(name, arguments);
            Some(format!("{name}: {summary}"))
        },
        ConversationItem::Reasoning { .. } => Some("Thinking...".to_string()),
        ConversationItem::Message { role, .. } if *role == Role::Assistant => {
            Some("Responding...".to_string())
        },
        _ => None,
    }
}

pub fn format_retry_message(status: &RetryStatus) -> String {
    if status.reason == RetryReason::ContextOverflow {
        return format!(
            "Retrying once with {} after context overflow",
            status.detail
        );
    }

    let delay = format_duration_tenths(status.delay);
    format!(
        "Retrying in {delay}s after {} (attempt {}/{})",
        status.detail, status.attempt, status.max_retries
    )
}
