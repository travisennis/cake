//! Output formatting and rendering for CLI responses.
//!
//! This module provides the [`CliOutputSink`] for rendering LLM responses to the
//! terminal in text, JSON, or stream-JSON format, as well as the [`TurnResult`]
//! struct used to carry a single agent-turn outcome and its duration.

use std::io::{self, Write};
use std::path::Path;

use crate::OutputFormat;
use crate::clients::Agent;
use crate::config::{DataDir, Session};

/// Signal that stdout's consumer has closed the output stream.
#[derive(Debug, thiserror::Error)]
#[error("stdout closed")]
struct ClosedOutput;

/// Outcome of a single agent turn, bundling the result with its elapsed time.
pub struct TurnResult {
    pub(crate) result: anyhow::Result<String>,
    pub(crate) duration_ms: u64,
    /// Assistant responses that preceded a `Stop`-hook follow-up turn. Text
    /// already printed them when they completed; JSON reports them.
    pub(crate) intermediate: Vec<String>,
}

/// Pure-rendering sink for CLI output.
///
/// Dispatches responses to the appropriate output format (text, JSON, or
/// stream-JSON).
#[derive(Clone, Copy)]
pub struct CliOutputSink {
    format: OutputFormat,
    fail_on_error: bool,
}

impl CliOutputSink {
    pub(crate) const fn new(format: OutputFormat, fail_on_error: bool) -> Self {
        Self {
            format,
            fail_on_error,
        }
    }

    pub(crate) fn attach_callbacks(self, mut client: Agent) -> Agent {
        if self.format == OutputFormat::Text {
            client = client.with_progress_callback(Self::write_progress);
        }

        if self.format == OutputFormat::StreamJson {
            client = client.with_fallible_streaming_json(Self::write_stream_record);
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
            intermediate,
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
                let json = Self::with_intermediate_results(json, &intermediate);
                Self::write_json_value(&json)?;
                result.map(|_| ())
            },
            OutputFormat::StreamJson => Self::stream_json_exit_result(result, self.fail_on_error),
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
    ///
    /// `--fail-on-error` opts the run into the other rule: every in-stream
    /// failure propagates, so `finish_run` classifies it into the documented
    /// exit class instead of the process exiting 0. Broken-pipe cancellation
    /// still exits 0, because `finish_run` maps `ClosedOutput` to success.
    fn stream_json_exit_result(
        result: anyhow::Result<String>,
        fail_on_error: bool,
    ) -> anyhow::Result<()> {
        match result {
            Err(error) if Self::should_propagate_stream_error(&error, fail_on_error) => Err(error),
            _ => Ok(()),
        }
    }

    fn should_propagate_stream_error(error: &anyhow::Error, fail_on_error: bool) -> bool {
        fail_on_error
            || Self::is_closed_output(error)
            || matches!(
                error.downcast_ref::<crate::config::OutputSchemaError>(),
                Some(crate::config::OutputSchemaError::Unsatisfied { .. })
            )
    }

    fn render_text_result(result: anyhow::Result<String>) -> anyhow::Result<()> {
        let response_text = result?;
        Self::write_text_response(&response_text)
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
                json["error"] = serde_json::json!(format!("{e:#}"));
                // Additive discriminator so consumers can tell a cut-off from
                // other agent errors — parity with stream-json's task_complete
                // subtype.
                if e.downcast_ref::<crate::types::CutOffError>().is_some() {
                    json["subtype"] = serde_json::json!("cut_off");
                } else if let Some(limit_exceeded) =
                    e.downcast_ref::<crate::types::LimitExceededError>()
                {
                    json["subtype"] = serde_json::json!("limit_exceeded");
                    // Match the task_complete record: the error carries the
                    // concise detail and the partial result is surfaced in the
                    // result position, not embedded in the error text. An
                    // absent partial result serializes as null, like other
                    // errors.
                    json["error"] = serde_json::json!(limit_exceeded.detail);
                    json["result"] = serde_json::json!(limit_exceeded.result);
                }
            },
        }

        json
    }

    /// Return whether an error represents normal cancellation by a closed stdout
    /// consumer. The CLI handles this after all run-local guards leave scope.
    pub(crate) fn is_closed_output(error: &anyhow::Error) -> bool {
        error.downcast_ref::<ClosedOutput>().is_some()
    }

    /// Add the intermediate responses to a completion document only when a
    /// `Stop` follow-up superseded them.
    pub(crate) fn with_intermediate_results(
        mut json: serde_json::Value,
        intermediate: &[String],
    ) -> serde_json::Value {
        if !intermediate.is_empty() {
            json["intermediate_results"] = serde_json::json!(intermediate);
        }
        json
    }

    /// Print an intermediate assistant response before a follow-up turn.
    ///
    /// Text mode prints it as soon as it completes so the user does not wait
    /// for the follow-up; JSON keeps it for the final document; stream-json has
    /// already emitted the assistant message as a record.
    pub(crate) fn render_intermediate(self, text: &str, marker: &str) -> anyhow::Result<()> {
        if self.format == OutputFormat::Text {
            Self::write_text_response(text)?;
            eprintln!("\n{marker}");
        }
        Ok(())
    }

    /// Write one stream-json record and treat a closed consumer as a normal
    /// successful termination. A pipe reader such as `head` is allowed to stop
    /// after the records it needs; it must not turn that choice into a panic.
    pub(crate) fn write_stream_record(json: &str) -> anyhow::Result<()> {
        let mut stdout = io::stdout().lock();
        Self::write_stream_record_to(&mut stdout, json)
    }

    fn write_stream_record_to<W: Write>(writer: &mut W, json: &str) -> anyhow::Result<()> {
        Self::write_line(writer, json)
    }

    fn write_line<W: Write>(writer: &mut W, content: &str) -> anyhow::Result<()> {
        writeln!(writer, "{content}").map_err(Self::map_output_error)
    }

    fn map_output_error(error: io::Error) -> anyhow::Error {
        if error.kind() == io::ErrorKind::BrokenPipe {
            ClosedOutput.into()
        } else {
            anyhow::Error::new(error).context("failed to write stdout")
        }
    }

    fn write_text_response(content: &str) -> anyhow::Result<()> {
        let mut stdout = io::stdout().lock();
        Self::write_text_response_to(&mut stdout, content)
    }

    fn write_text_response_to<W: Write>(writer: &mut W, content: &str) -> anyhow::Result<()> {
        Self::write_line(writer, content)
    }

    fn write_progress(message: &str) {
        eprintln!("{message}");
    }

    pub(crate) fn write_json_value(value: &serde_json::Value) -> anyhow::Result<()> {
        let mut stdout = io::stdout().lock();
        Self::write_json_value_to(&mut stdout, value)
    }

    fn write_json_value_to<W: Write>(
        writer: &mut W,
        value: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let content = serde_json::to_string(value)?;
        Self::write_line(writer, &content)
    }

    pub(crate) fn write_error(error: &anyhow::Error) {
        eprintln!("Error: {error:#}");
    }
}
