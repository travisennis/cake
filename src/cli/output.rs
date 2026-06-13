//! Output formatting and rendering for CLI responses.
//!
//! This module provides the [`CliOutputSink`] for rendering LLM responses to the
//! terminal in text, JSON, or stream-JSON format, as well as the [`TurnResult`]
//! struct used to carry a single agent-turn outcome and its duration.

use std::path::Path;

use crate::OutputFormat;
use crate::clients::Agent;
use crate::config::{DataDir, Session};

/// Outcome of a single agent turn, bundling the result with its elapsed time.
pub struct TurnResult {
    pub(crate) result: anyhow::Result<String>,
    pub(crate) duration_ms: u64,
}

/// Pure-rendering sink for CLI output.
///
/// Dispatches responses to the appropriate output format (text, JSON, or
/// stream-JSON).
#[derive(Clone, Copy)]
pub struct CliOutputSink {
    format: OutputFormat,
}

impl CliOutputSink {
    pub(crate) const fn new(format: OutputFormat) -> Self {
        Self { format }
    }

    pub(crate) fn attach_callbacks(self, mut client: Agent) -> Agent {
        if self.format == OutputFormat::StreamJson {
            client = client.with_streaming_json(Self::write_stream_record);
        }

        client
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

    fn render_text_result(result: anyhow::Result<String>) -> anyhow::Result<()> {
        let response_text = result?;
        Self::write_text_response(&response_text);
        Ok(())
    }

    pub(crate) fn turn_result_json(
        result: &anyhow::Result<String>,
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
                json["result"] = serde_json::json!(response_text);
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

    pub(crate) fn write_error(error: &anyhow::Error) {
        eprintln!("Error: {error}");
    }
}
