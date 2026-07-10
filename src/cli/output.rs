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
            OutputFormat::StreamJson => Self::stream_json_exit_result(result),
        }
    }

    /// Decide whether a stream-json run's in-stream error also fails the
    /// process.
    ///
    /// Stream-json reports task failure in the `task_complete` record and
    /// historically exits 0 for in-stream errors. Output-schema exhaustion is
    /// the exception: its contract requires failure on both channels — the
    /// `error_output_schema` record and a nonzero exit — so that error
    /// propagates (mirroring how `Interrupted` propagates exit 130 regardless
    /// of output format).
    fn stream_json_exit_result(result: anyhow::Result<String>) -> anyhow::Result<()> {
        match result {
            Err(error)
                if matches!(
                    error.downcast_ref::<crate::config::OutputSchemaError>(),
                    Some(crate::config::OutputSchemaError::Unsatisfied { .. })
                ) =>
            {
                Err(error)
            },
            _ => Ok(()),
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
                // Additive discriminator so consumers can tell a cut-off from
                // other agent errors — parity with stream-json's task_complete
                // subtype.
                if e.downcast_ref::<crate::types::CutOffError>().is_some() {
                    json["subtype"] = serde_json::json!("cut_off");
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OutputSchemaError;

    #[test]
    fn stream_json_swallows_generic_errors() {
        let result: anyhow::Result<String> = Err(anyhow::anyhow!("api failure"));
        assert!(CliOutputSink::stream_json_exit_result(result).is_ok());
    }

    #[test]
    fn stream_json_swallows_success() {
        let result: anyhow::Result<String> = Ok("done".to_string());
        assert!(CliOutputSink::stream_json_exit_result(result).is_ok());
    }

    #[test]
    fn stream_json_swallows_cut_off() {
        // Stream-json reports cut-offs in the task_complete record and keeps
        // its documented in-stream-error policy of exit 0.
        let result: anyhow::Result<String> =
            Err(crate::types::CutOffError::new("cut off".to_string()).into());
        assert!(CliOutputSink::stream_json_exit_result(result).is_ok());
    }

    #[test]
    fn render_text_result_propagates_cut_off_error() {
        // Text mode must not print the cut-off detail in the assistant-output
        // position; the error propagates to stderr and a nonzero exit.
        let result: anyhow::Result<String> =
            Err(crate::types::CutOffError::new("cut off".to_string()).into());
        assert!(CliOutputSink::render_text_result(result).is_err());
    }

    #[test]
    fn stream_json_propagates_output_schema_exhaustion() {
        let result: anyhow::Result<String> = Err(OutputSchemaError::Unsatisfied {
            attempts: 3,
            detail: "\"summary\" is a required property".to_string(),
        }
        .into());
        let error = CliOutputSink::stream_json_exit_result(result).unwrap_err();
        assert_eq!(
            crate::exit_code::classify_to_u8(&error),
            crate::exit_code::code::AGENT_ERROR
        );
    }
}
