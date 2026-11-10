//! `cake debug session`: read-only diagnostics for one persisted session.
//!
//! The command correlates a session transcript, its optional telemetry
//! sidecar, and the date-rotated logs without changing any artifact or
//! starting a provider, hook, gate, MCP server, or toolbox process. It never
//! copies prompts, tool output, hook text, provider payloads, credentials, or
//! log bodies into the report or an export.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{BufReader, Write as _};
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::cli::debug::config_report::{ConfigInputs, ConfigReport};
use crate::cli::settings::try_load_settings;
use crate::cli::{
    CHECK_SESSION_CONFIG_UNAVAILABLE, CHECK_SESSION_INCOMPLETE_TASK, CHECK_SESSION_INPUT,
    CHECK_SESSION_INVOCATION_UNMATCHED, CHECK_SESSION_LIVENESS_UNKNOWN,
    CHECK_SESSION_LOGS_UNREADABLE, CHECK_SESSION_PARTIAL_TAIL, CHECK_SESSION_TELEMETRY_CORRUPT,
    CHECK_SESSION_TELEMETRY_MISSING, CHECK_SESSION_TELEMETRY_UNREADABLE,
    CHECK_SESSION_TRANSCRIPT_CORRUPT, CHECK_SESSION_TRANSCRIPT_UNREADABLE, CommandRunOptions,
    DiagnosticCheck, DiagnosticDocument, report_failure,
};
use crate::config::owner_only;
use crate::config::session::CURRENT_FORMAT_VERSION;
use crate::config::session_jsonl::{FramedSessionLine, SessionFramer};
use crate::config::{DataDir, SessionLiveness, probe_session_liveness};

/// Maximum number of slow operations retained in the report. The count of
/// everything over the threshold is still reported.
const SLOW_OPERATION_LIMIT: usize = 20;

/// Maximum number of related daily log paths retained.
const LOG_LIMIT: usize = 16;

/// Correlation window for a telemetry invocation and a transcript task that do
/// not share an observed task id.
const START_TIME_MATCH_WINDOW_MS: i64 = 120_000;

/// Local flags for the session diagnostic command.
pub(super) struct SessionInputs<'a> {
    pub uuid: &'a str,
    pub export: Option<&'a Path>,
    pub slow_ms: u64,
}

/// Failures the command reports before it can produce a diagnostic document.
#[derive(Debug, thiserror::Error)]
pub enum SessionDiagnosticError {
    /// The session UUID argument is not a valid UUID.
    #[error("invalid session UUID '{0}'")]
    InvalidUuid(String),
    /// No session file exists for the UUID.
    #[error("session not found: {0}")]
    MissingSession(String),
    /// The session artifacts could not be diagnosed completely.
    #[error("session diagnostic incomplete for {0}: {1}")]
    Incomplete(String, String),
    /// The export target could not be written.
    #[error("cannot export session diagnostic to '{0}': {1}")]
    Export(String, String),
    /// The export target already exists.
    #[error("session export target already exists: {0}")]
    ExportExists(String),
    /// The export target is one of the source artifacts.
    #[error("session export target is a source artifact: {0}")]
    ExportSource(String),
    /// The export target's parent is not an existing directory.
    #[error("session export parent is not a directory: {0}")]
    ExportParent(String),
}

impl SessionDiagnosticError {
    /// The process exit code that accompanies this failure.
    pub const fn exit_code(&self) -> u8 {
        use crate::exit_code::code;
        match self {
            Self::InvalidUuid(_)
            | Self::MissingSession(_)
            | Self::ExportExists(_)
            | Self::ExportSource(_)
            | Self::ExportParent(_) => code::INPUT_ERROR,
            Self::Incomplete(_, _) | Self::Export(_, _) => code::AGENT_ERROR,
        }
    }
}

/// Inspect one persisted session's state, artifacts, and telemetry.
pub(super) fn run_session(
    inputs: &SessionInputs<'_>,
    json: bool,
    data_dir: &DataDir,
    options: &CommandRunOptions<'_>,
) -> anyhow::Result<()> {
    let Ok(uuid) = Uuid::parse_str(inputs.uuid) else {
        return Err(fail(
            json,
            SessionDiagnosticError::InvalidUuid(inputs.uuid.to_string()),
        ));
    };

    let report = SessionReport::build(uuid, data_dir, options, inputs.slow_ms, json, inputs.export)
        .map_err(|error| fail(json, error))?;
    let document = report.render_json()?;

    if let Some(path) = inputs.export {
        write_export(path, &document, &report.data).map_err(|error| fail(json, error))?;
    }

    if json {
        print!("{document}");
    } else {
        print!("{}", report.format_text());
    }

    if report.has_error() {
        return Err(SessionDiagnosticError::Incomplete(
            uuid.to_string(),
            report.incomplete_reason(),
        )
        .into());
    }
    Ok(())
}

/// Render a pre-report failure as the documented diagnostic error document.
fn fail(json: bool, error: SessionDiagnosticError) -> anyhow::Error {
    report_failure(json, "debug session", CHECK_SESSION_INPUT, error.into())
}

/// One complete session diagnostic report.
struct SessionReport {
    data: SessionData,
    checks: Vec<DiagnosticCheck>,
}

impl SessionReport {
    #[expect(
        clippy::too_many_lines,
        reason = "one report assembly keeps scan, correlation, and payload construction in order"
    )]
    fn build(
        uuid: Uuid,
        data_dir: &DataDir,
        options: &CommandRunOptions<'_>,
        slow_ms: u64,
        json: bool,
        export: Option<&Path>,
    ) -> Result<Self, SessionDiagnosticError> {
        let mut transcript = TranscriptScan::read(&data_dir.session_path(uuid), uuid)?;
        let mut telemetry = TelemetryScan::read(&data_dir.session_telemetry_path(uuid), slow_ms);
        let liveness = if transcript.regular_file {
            probe_session_liveness(Path::new(&transcript.path))
        } else {
            SessionLiveness::Unknown
        };

        assign_task_states(&mut transcript.tasks, liveness);
        correlate_invocations(&mut transcript.tasks, &mut telemetry.invocations);

        let retry_counts =
            invocation_counter(&telemetry.invocations, |invocation| invocation.retry_count);
        let slow_counts =
            invocation_counter(&telemetry.invocations, |invocation| invocation.slow_count);
        let tasks = transcript
            .tasks
            .iter()
            .map(|task| task.row(&retry_counts, &slow_counts))
            .collect::<Vec<_>>();
        let invocations = telemetry
            .invocations
            .iter()
            .map(InvocationAccum::row)
            .collect::<Vec<_>>();
        let mut invocations = invocations;
        invocations.sort_by(|left, right| {
            left.started_at
                .cmp(&right.started_at)
                .then_with(|| left.invocation_id.cmp(&right.invocation_id))
        });

        let invocation_task = invocations
            .iter()
            .filter_map(|invocation| {
                invocation
                    .task_id
                    .as_ref()
                    .map(|task_id| (invocation.invocation_id.clone(), task_id.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        let retries = telemetry
            .retries
            .iter()
            .map(|retry| retry.row(&invocation_task))
            .collect::<Vec<_>>();
        let mut retries = retries;
        retries.sort_by(|left, right| {
            left.timestamp
                .cmp(&right.timestamp)
                .then_with(|| left.invocation_id.cmp(&right.invocation_id))
                .then_with(|| left.attempt.cmp(&right.attempt))
        });
        let mut slow_operations = telemetry
            .slow_operations
            .iter()
            .map(|operation| operation.row(&invocation_task))
            .collect::<Vec<_>>();
        sort_slow_operations(&mut slow_operations);
        let slow_operation_count = slow_operations.len();
        slow_operations.truncate(SLOW_OPERATION_LIMIT);

        let (current_config, config_checks) = current_config_context(data_dir, options, json);
        let artifacts = scan_logs(
            data_dir,
            transcript.created_at,
            latest_activity(transcript.last_activity_at, telemetry.last_activity_at),
        );
        let last_activity =
            LastActivity::new(transcript.last_activity_at, telemetry.last_activity_at);
        let state = overall_state(transcript.state, liveness, &tasks);
        let diagnosis_complete = diagnosis_complete(&transcript, &telemetry);
        let mut checks = build_checks(
            &transcript,
            &telemetry,
            liveness,
            &tasks,
            &invocations,
            &config_checks,
        );
        if let Some(warning) = &artifacts.warning {
            checks.push(DiagnosticCheck::warning(
                CHECK_SESSION_LOGS_UNREADABLE,
                warning.clone(),
            ));
        }

        let data = SessionData {
            session_id: uuid.to_string(),
            state,
            diagnosis_complete,
            liveness: LivenessReport::new(liveness),
            transcript: transcript.artifact(),
            telemetry: telemetry.artifact(),
            last_activity,
            tasks,
            invocations,
            retries,
            slow_operations,
            slow_operation_threshold_ms: slow_ms,
            slow_operation_count,
            artifacts: artifacts.report,
            current_config,
            export: ExportReport {
                requested: export.is_some(),
                redacted: true,
                format: "diagnostic-document-json",
                path: export.map(|path| path.display().to_string()),
            },
        };

        Ok(Self { data, checks })
    }

    fn render_json(&self) -> anyhow::Result<String> {
        DiagnosticDocument::new(
            "debug session",
            self.summary(),
            self.checks.clone(),
            &self.data,
        )
        .render()
    }

    fn summary(&self) -> Value {
        let completed = self
            .data
            .tasks
            .iter()
            .filter(|task| task.state == "completed")
            .count();
        let interrupted = self
            .data
            .tasks
            .iter()
            .filter(|task| task.state == "interrupted")
            .count();
        let live = self
            .data
            .tasks
            .iter()
            .filter(|task| task.state == "live")
            .count();
        let unknown = self
            .data
            .tasks
            .iter()
            .filter(|task| task.state == "unknown_liveness")
            .count();
        let matched = self
            .data
            .invocations
            .iter()
            .filter(|invocation| invocation.task_id.is_some())
            .count();
        json!({
            "session_id": self.data.session_id,
            "state": self.data.state,
            "liveness": self.data.liveness.state,
            "diagnosis_complete": self.data.diagnosis_complete,
            "task_count": self.data.tasks.len(),
            "completed_task_count": completed,
            "interrupted_task_count": interrupted,
            "live_task_count": live,
            "unknown_liveness_task_count": unknown,
            "invocation_count": self.data.invocations.len(),
            "matched_invocation_count": matched,
            "retry_count": self.data.retries.len(),
            "slow_operation_count": self.data.slow_operation_count,
            "transcript_state": self.data.transcript.artifact.state,
            "telemetry_state": self.data.telemetry.artifact.state,
            "warning_count": self.checks.len(),
        })
    }

    fn has_error(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == crate::cli::diagnostic::DiagnosticStatus::Error)
    }

    fn incomplete_reason(&self) -> String {
        if let Some(error) = &self.data.transcript.artifact.error {
            return format!("{}: {error}", self.data.transcript.artifact.state);
        }
        if let Some(error) = &self.data.telemetry.artifact.error {
            return format!("{}: {error}", self.data.telemetry.artifact.state);
        }
        "a final record was truncated".to_string()
    }

    fn format_text(&self) -> String {
        let mut output = String::from("Session diagnostics\n");
        _ = writeln!(output, "Session: {}", self.data.session_id);
        _ = writeln!(
            output,
            "State: {} (diagnosis complete: {})",
            state_label(self.data.state),
            yes_no(self.data.diagnosis_complete)
        );
        _ = writeln!(
            output,
            "Liveness: {}",
            liveness_label(self.data.liveness.state)
        );
        _ = writeln!(
            output,
            "Last activity: {} ({})",
            format_time(self.data.last_activity.at),
            self.data.last_activity.source.unwrap_or("none")
        );

        output.push_str("\nTranscript\n");
        append_artifact(
            &mut output,
            "  ",
            &self.data.transcript.artifact.path,
            self.data.transcript.artifact.state,
            self.data.transcript.artifact.bytes,
        );
        _ = writeln!(
            output,
            "    Records: {}  Tasks: {}  Partial tail: {}",
            self.data.transcript.artifact.record_count,
            self.data.transcript.task_count,
            yes_no(self.data.transcript.artifact.partial_tail)
        );
        if let Some(created) = self.data.transcript.created_at {
            _ = writeln!(output, "    Created: {}", format_time(Some(created)));
        }

        output.push_str("\nTelemetry\n");
        append_artifact(
            &mut output,
            "  ",
            &self.data.telemetry.artifact.path,
            self.data.telemetry.artifact.state,
            self.data.telemetry.artifact.bytes,
        );
        if self.data.telemetry.artifact.state != "missing" {
            _ = writeln!(
                output,
                "    Records: {}  Invocations: {}  Partial tail: {}",
                self.data.telemetry.artifact.record_count,
                self.data.telemetry.invocation_count,
                yes_no(self.data.telemetry.artifact.partial_tail)
            );
        }

        output.push_str("\nArtifacts\n");
        _ = writeln!(
            output,
            "  Log directory: {}",
            self.data.artifacts.log_directory
        );
        if self.data.artifacts.logs.is_empty() {
            output.push_str("  Logs: none related to this session\n");
        } else {
            for log in &self.data.artifacts.logs {
                _ = writeln!(
                    output,
                    "  {}  {}  {}",
                    log.date,
                    format_bytes(log.bytes),
                    log.path
                );
            }
            if self.data.artifacts.logs_truncated > 0 {
                _ = writeln!(
                    output,
                    "  ({} more logs not shown)",
                    self.data.artifacts.logs_truncated
                );
            }
        }

        append_tasks(&mut output, &self.data.tasks);
        append_invocations(&mut output, &self.data.invocations);
        append_retries(&mut output, &self.data.retries);
        append_slow_operations(
            &mut output,
            &self.data.slow_operations,
            self.data.slow_operation_threshold_ms,
            self.data.slow_operation_count,
        );
        append_current_config(&mut output, self.data.current_config.as_ref());
        append_diagnostics(&mut output, &self.checks);
        if let Some(path) = &self.data.export.path {
            _ = writeln!(output, "\nExported redacted diagnostic document to {path}");
        }
        output
    }
}

/// One compact diagnostic artifact view for the JSON payload and text output.
#[derive(Debug, Serialize)]
struct ArtifactView {
    state: &'static str,
    path: String,
    exists: bool,
    bytes: Option<u64>,
    modified_at: Option<DateTime<Utc>>,
    record_count: usize,
    record_types: BTreeMap<String, usize>,
    unknown_record_count: usize,
    partial_tail: bool,
    ignored_partial_tail: bool,
    error: Option<String>,
}

/// Transcript-specific artifact fields.
#[derive(Debug, Serialize)]
struct TranscriptView {
    #[serde(flatten)]
    artifact: ArtifactView,
    format_version: Option<u32>,
    session_id: Option<String>,
    created_at: Option<DateTime<Utc>>,
    working_directory: Option<String>,
    task_count: usize,
    tool_output_bytes: u64,
    hook_event_count: usize,
}

/// Telemetry-specific artifact fields.
#[derive(Debug, Serialize)]
struct TelemetryView {
    #[serde(flatten)]
    artifact: ArtifactView,
    invocation_count: usize,
}

/// One session's full diagnostic payload.
#[derive(Debug, Serialize)]
struct SessionData {
    session_id: String,
    state: &'static str,
    diagnosis_complete: bool,
    liveness: LivenessReport,
    transcript: TranscriptView,
    telemetry: TelemetryView,
    last_activity: LastActivity,
    tasks: Vec<TaskRow>,
    invocations: Vec<InvocationRow>,
    retries: Vec<RetryRow>,
    slow_operations: Vec<SlowOperationRow>,
    slow_operation_threshold_ms: u64,
    slow_operation_count: usize,
    artifacts: ArtifactsReport,
    current_config: Option<Value>,
    export: ExportReport,
}

#[derive(Debug, Serialize)]
struct LivenessReport {
    state: &'static str,
}

impl LivenessReport {
    const fn new(liveness: SessionLiveness) -> Self {
        Self {
            state: match liveness {
                SessionLiveness::Live => "live",
                SessionLiveness::NotLive => "not_live",
                SessionLiveness::Unknown => "unknown",
            },
        }
    }
}

#[derive(Debug, Serialize)]
struct LastActivity {
    at: Option<DateTime<Utc>>,
    source: Option<&'static str>,
    transcript_at: Option<DateTime<Utc>>,
    telemetry_at: Option<DateTime<Utc>>,
}

impl LastActivity {
    fn new(transcript: Option<DateTime<Utc>>, telemetry: Option<DateTime<Utc>>) -> Self {
        let (at, source) = match (transcript, telemetry) {
            (Some(left), Some(right)) if left > right => (Some(left), Some("transcript")),
            (Some(left), Some(right)) if right > left => (Some(right), Some("telemetry")),
            (Some(value), Some(_)) => (Some(value), Some("both")),
            (Some(value), None) => (Some(value), Some("transcript")),
            (None, Some(value)) => (Some(value), Some("telemetry")),
            (None, None) => (None, None),
        };
        Self {
            at,
            source,
            transcript_at: transcript,
            telemetry_at: telemetry,
        }
    }
}

#[derive(Debug, Serialize)]
struct ArtifactsReport {
    log_directory: String,
    logs: Vec<LogArtifact>,
    logs_truncated: usize,
}

#[derive(Debug, Serialize)]
struct LogArtifact {
    date: String,
    path: String,
    bytes: u64,
    modified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
struct ExportReport {
    requested: bool,
    redacted: bool,
    format: &'static str,
    path: Option<String>,
}

#[expect(
    clippy::struct_field_names,
    reason = "the fields mirror the provider usage vocabulary"
)]
#[derive(Debug, Clone, Copy, Serialize)]
struct UsageSummary {
    input_tokens: u64,
    cached_tokens: u64,
    cache_write_tokens: u64,
    output_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
}

impl UsageSummary {
    fn from_value(value: Option<&Value>) -> Option<Self> {
        let value = value?;
        Some(Self {
            input_tokens: u64_field(value, "input_tokens").unwrap_or(0),
            cached_tokens: nested_u64_field(value, "input_tokens_details", "cached_tokens")
                .unwrap_or(0),
            cache_write_tokens: nested_u64_field(
                value,
                "input_tokens_details",
                "cache_write_tokens",
            )
            .unwrap_or(0),
            output_tokens: u64_field(value, "output_tokens").unwrap_or(0),
            reasoning_tokens: nested_u64_field(value, "output_tokens_details", "reasoning_tokens")
                .unwrap_or(0),
            total_tokens: u64_field(value, "total_tokens").unwrap_or(0),
        })
    }
}

#[derive(Debug)]
struct TerminalInfo {
    subtype: String,
    is_error: bool,
}

#[derive(Debug)]
struct TaskAccum {
    task_id: String,
    started_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
    duration_ms: Option<u64>,
    turn_count: Option<u32>,
    tool_call_count: Option<u32>,
    usage: Option<UsageSummary>,
    model: Option<String>,
    model_config: Option<String>,
    terminal: Option<TerminalInfo>,
    record_count: usize,
    hook_events: usize,
    last_activity_at: Option<DateTime<Utc>>,
    state: &'static str,
    invocation_id: Option<String>,
    invocation_match: Option<&'static str>,
}

impl TaskAccum {
    const fn new(task_id: String) -> Self {
        Self {
            task_id,
            started_at: None,
            ended_at: None,
            duration_ms: None,
            turn_count: None,
            tool_call_count: None,
            usage: None,
            model: None,
            model_config: None,
            terminal: None,
            record_count: 0,
            hook_events: 0,
            last_activity_at: None,
            state: "interrupted",
            invocation_id: None,
            invocation_match: None,
        }
    }

    fn touch(&mut self, timestamp: Option<DateTime<Utc>>) {
        if let Some(timestamp) = timestamp
            && self
                .last_activity_at
                .is_none_or(|current| timestamp > current)
        {
            self.last_activity_at = Some(timestamp);
        }
    }

    fn row(
        &self,
        retry_counts: &BTreeMap<String, usize>,
        slow_counts: &BTreeMap<String, usize>,
    ) -> TaskRow {
        let invocation_id = self.invocation_id.clone();
        let retries = invocation_id
            .as_ref()
            .and_then(|id| retry_counts.get(id))
            .copied()
            .unwrap_or(0);
        let slow_operations = invocation_id
            .as_ref()
            .and_then(|id| slow_counts.get(id))
            .copied()
            .unwrap_or(0);
        TaskRow {
            task_id: self.task_id.clone(),
            started_at: self.started_at,
            ended_at: self.ended_at,
            state: self.state,
            termination: self
                .terminal
                .as_ref()
                .map(|terminal| terminal.subtype.clone()),
            is_error: self
                .terminal
                .as_ref()
                .is_some_and(|terminal| terminal.is_error),
            duration_ms: self.duration_ms,
            turn_count: self.turn_count,
            tool_call_count: self.tool_call_count,
            usage: self.usage,
            model: self.model.clone(),
            model_config: self.model_config.clone(),
            record_count: self.record_count,
            hook_events: self.hook_events,
            retries,
            slow_operations,
            invocation_id: self.invocation_id.clone(),
            invocation_match: self.invocation_match,
            last_activity_at: self.last_activity_at,
        }
    }
}

#[derive(Debug, Serialize)]
struct TaskRow {
    task_id: String,
    started_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
    state: &'static str,
    termination: Option<String>,
    is_error: bool,
    duration_ms: Option<u64>,
    turn_count: Option<u32>,
    tool_call_count: Option<u32>,
    usage: Option<UsageSummary>,
    model: Option<String>,
    model_config: Option<String>,
    record_count: usize,
    hook_events: usize,
    retries: usize,
    slow_operations: usize,
    invocation_id: Option<String>,
    invocation_match: Option<&'static str>,
    last_activity_at: Option<DateTime<Utc>>,
}

#[derive(Debug)]
struct InvocationAccum {
    invocation_id: String,
    mode: Option<String>,
    started_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
    model: Option<String>,
    api_type: Option<String>,
    output_format: Option<String>,
    tool_count: Option<usize>,
    record_count: usize,
    api_attempts: usize,
    failed_attempts: usize,
    retry_count: usize,
    retry_waits: usize,
    tool_calls: usize,
    tool_errors: usize,
    compensations: usize,
    gate_startups: usize,
    gate_decisions: usize,
    gate_evidence: usize,
    unknown_record_count: usize,
    summary: Option<InvocationSummary>,
    observed_task_ids: BTreeSet<String>,
    task_id: Option<String>,
    task_match: Option<&'static str>,
    last_activity_at: Option<DateTime<Utc>>,
    slow_count: usize,
}

impl InvocationAccum {
    const fn new(invocation_id: String) -> Self {
        Self {
            invocation_id,
            mode: None,
            started_at: None,
            ended_at: None,
            model: None,
            api_type: None,
            output_format: None,
            tool_count: None,
            record_count: 0,
            api_attempts: 0,
            failed_attempts: 0,
            retry_count: 0,
            retry_waits: 0,
            tool_calls: 0,
            tool_errors: 0,
            compensations: 0,
            gate_startups: 0,
            gate_decisions: 0,
            gate_evidence: 0,
            unknown_record_count: 0,
            summary: None,
            observed_task_ids: BTreeSet::new(),
            task_id: None,
            task_match: None,
            last_activity_at: None,
            slow_count: 0,
        }
    }

    fn touch(&mut self, timestamp: Option<DateTime<Utc>>) {
        if let Some(timestamp) = timestamp {
            if self.started_at.is_none() {
                self.started_at = Some(timestamp);
            }
            if self.ended_at.is_none_or(|current| timestamp > current) {
                self.ended_at = Some(timestamp);
            }
            if self
                .last_activity_at
                .is_none_or(|current| timestamp > current)
            {
                self.last_activity_at = Some(timestamp);
            }
        }
    }

    fn row(&self) -> InvocationRow {
        InvocationRow {
            invocation_id: self.invocation_id.clone(),
            mode: self.mode.clone(),
            started_at: self.started_at,
            ended_at: self.ended_at,
            record_count: self.record_count,
            model: self.model.clone(),
            api_type: self.api_type.clone(),
            output_format: self.output_format.clone(),
            tool_count: self.tool_count,
            api_attempts: self.api_attempts,
            failed_attempts: self.failed_attempts,
            retry_count: self.retry_count,
            retry_waits: self.retry_waits,
            tool_calls: self.tool_calls,
            tool_errors: self.tool_errors,
            compensations: self.compensations,
            gate_startups: self.gate_startups,
            gate_decisions: self.gate_decisions,
            gate_evidence: self.gate_evidence,
            unknown_record_count: self.unknown_record_count,
            summary: self.summary.clone(),
            task_id: self.task_id.clone(),
            task_match: self.task_match,
            last_activity_at: self.last_activity_at,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct InvocationSummary {
    success: bool,
    duration_ms: u64,
    turn_count: u32,
    usage: Option<UsageSummary>,
}

#[derive(Debug, Serialize)]
struct InvocationRow {
    invocation_id: String,
    mode: Option<String>,
    started_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
    record_count: usize,
    model: Option<String>,
    api_type: Option<String>,
    output_format: Option<String>,
    tool_count: Option<usize>,
    api_attempts: usize,
    failed_attempts: usize,
    retry_count: usize,
    retry_waits: usize,
    tool_calls: usize,
    tool_errors: usize,
    compensations: usize,
    gate_startups: usize,
    gate_decisions: usize,
    gate_evidence: usize,
    unknown_record_count: usize,
    summary: Option<InvocationSummary>,
    task_id: Option<String>,
    task_match: Option<&'static str>,
    last_activity_at: Option<DateTime<Utc>>,
}

#[derive(Debug)]
struct RawRetry {
    invocation_id: String,
    timestamp: Option<DateTime<Utc>>,
    attempt: u64,
    max_retries: u64,
    reason: String,
    delay_ms: u64,
    changed_request_overrides: bool,
}

impl RawRetry {
    fn row(&self, invocation_task: &BTreeMap<String, String>) -> RetryRow {
        RetryRow {
            invocation_id: self.invocation_id.clone(),
            task_id: invocation_task.get(&self.invocation_id).cloned(),
            timestamp: self.timestamp,
            attempt: self.attempt,
            max_retries: self.max_retries,
            reason: self.reason.clone(),
            delay_ms: self.delay_ms,
            changed_request_overrides: self.changed_request_overrides,
        }
    }
}

#[derive(Debug, Serialize)]
struct RetryRow {
    invocation_id: String,
    task_id: Option<String>,
    timestamp: Option<DateTime<Utc>>,
    attempt: u64,
    max_retries: u64,
    reason: String,
    delay_ms: u64,
    changed_request_overrides: bool,
}

#[derive(Debug)]
struct RawSlowOperation {
    invocation_id: String,
    timestamp: Option<DateTime<Utc>>,
    kind: &'static str,
    duration_ms: u64,
    subject: Option<String>,
}

impl RawSlowOperation {
    fn row(&self, invocation_task: &BTreeMap<String, String>) -> SlowOperationRow {
        SlowOperationRow {
            invocation_id: self.invocation_id.clone(),
            task_id: invocation_task.get(&self.invocation_id).cloned(),
            timestamp: self.timestamp,
            kind: self.kind,
            duration_ms: self.duration_ms,
            subject: self.subject.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
struct SlowOperationRow {
    invocation_id: String,
    task_id: Option<String>,
    timestamp: Option<DateTime<Utc>>,
    kind: &'static str,
    duration_ms: u64,
    subject: Option<String>,
}

impl TranscriptScan {
    /// Read the transcript and classify it without retaining record content.
    fn read(path: &Path, uuid: Uuid) -> Result<Self, SessionDiagnosticError> {
        let mut scan = Self::new(path);
        let Some(file) = scan.prepare(path, uuid)? else {
            return Ok(scan);
        };
        scan.scan_records(file, uuid);
        if scan.record_count == 0 && scan.state == "ok" {
            scan.mark_corrupt("session file is empty".to_string());
        }
        Ok(scan)
    }

    fn new(path: &Path) -> Self {
        Self {
            path: path.display().to_string(),
            regular_file: false,
            exists: false,
            bytes: None,
            modified_at: None,
            state: "ok",
            error: None,
            format_version: None,
            session_id: None,
            created_at: None,
            working_directory: None,
            record_count: 0,
            record_types: BTreeMap::new(),
            unknown_record_count: 0,
            partial_tail: false,
            ignored_partial_tail: false,
            tasks: Vec::new(),
            last_activity_at: None,
            tool_output_bytes: 0,
            hook_event_count: 0,
        }
    }

    fn prepare(
        &mut self,
        path: &Path,
        uuid: Uuid,
    ) -> Result<Option<fs::File>, SessionDiagnosticError> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(SessionDiagnosticError::MissingSession(uuid.to_string()));
            },
            Err(error) => {
                self.mark_unreadable(format!("cannot inspect session file: {error}"));
                return Ok(None);
            },
        };
        self.exists = true;
        self.bytes = Some(metadata.len());
        self.modified_at = metadata.modified().ok().map(DateTime::<Utc>::from);
        if metadata.file_type().is_symlink() {
            self.mark_unreadable("path is a symlink".to_string());
            return Ok(None);
        }
        if !metadata.is_file() {
            self.mark_unreadable("path is not a regular file".to_string());
            return Ok(None);
        }
        self.regular_file = true;
        match fs::File::open(path) {
            Ok(file) => Ok(Some(file)),
            Err(error) => {
                self.mark_unreadable(format!("cannot open session file: {error}"));
                Ok(None)
            },
        }
    }

    fn scan_records(&mut self, file: fs::File, uuid: Uuid) {
        let mut framer = SessionFramer::new(BufReader::new(file));
        let mut open_task = None;
        loop {
            let line = match framer.next_record() {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(error) => {
                    self.mark_unreadable(format!("cannot read session file: {error}"));
                    return;
                },
            };
            match self.scan_line(&line, uuid, &mut open_task) {
                Ok(true) => {},
                Ok(false) => break,
                Err(error) => {
                    self.mark_corrupt(error);
                    return;
                },
            }
        }
    }

    fn scan_line(
        &mut self,
        line: &FramedSessionLine,
        uuid: Uuid,
        open_task: &mut Option<usize>,
    ) -> Result<bool, String> {
        if line.partial_tail {
            self.partial_tail = true;
        }
        let value: Value = match serde_json::from_str(&line.text) {
            Ok(value) => value,
            Err(_) if line.partial_tail => {
                self.ignored_partial_tail = true;
                return Ok(false);
            },
            Err(_) => return Err(format!("invalid record on line {}", line.line_number)),
        };
        let Some(record_type) = value.get("type").and_then(Value::as_str) else {
            return Err(format!("record on line {} has no type", line.line_number));
        };
        if self.record_count == 0 {
            if record_type != "session_meta" {
                return Err("first record is not session_meta".to_string());
            }
            self.read_meta(&value, uuid);
            if self.state != "ok" {
                return Ok(false);
            }
            self.record_count += 1;
            increment(&mut self.record_types, record_type);
            return Ok(true);
        }
        self.consume_transcript_record(&value, record_type, *open_task, open_task);
        Ok(true)
    }

    fn consume_transcript_record(
        &mut self,
        value: &Value,
        record_type: &str,
        current_task: Option<usize>,
        open_task: &mut Option<usize>,
    ) {
        self.count_transcript_record(record_type, current_task);
        self.touch_transcript_task(value, record_type, current_task);
        self.classify_transcript_record(value, record_type, current_task, open_task);
    }

    fn count_transcript_record(&mut self, record_type: &str, current_task: Option<usize>) {
        self.record_count += 1;
        increment(&mut self.record_types, record_type);
        if record_type != "task_start"
            && let Some(index) = current_task
        {
            self.tasks[index].record_count += 1;
        }
    }

    fn touch_transcript_task(
        &mut self,
        value: &Value,
        record_type: &str,
        current_task: Option<usize>,
    ) {
        let timestamp = timestamp_field(value, "timestamp");
        self.last_activity_at = max_time(self.last_activity_at, timestamp);
        if record_type == "task_start" {
            return;
        }
        if let Some(index) = current_task {
            self.tasks[index].touch(timestamp);
        }
    }

    fn classify_transcript_record(
        &mut self,
        value: &Value,
        record_type: &str,
        current_task: Option<usize>,
        open_task: &mut Option<usize>,
    ) {
        match record_type {
            "task_start" => {
                *open_task = Some(self.task_start(value));
                if let Some(index) = *open_task {
                    self.tasks[index].record_count += 1;
                }
            },
            "task_complete" => self.task_complete(value, current_task),
            "function_call_output" => {
                self.tool_output_bytes += value
                    .get("output")
                    .and_then(Value::as_str)
                    .map_or(0, |output| output.len() as u64);
            },
            "hook_event" => {
                self.hook_event_count += 1;
                if let Some(index) = current_task {
                    self.tasks[index].hook_events += 1;
                }
            },
            "prompt_context"
            | "message"
            | "function_call"
            | "reasoning"
            | "skill_activated"
            | "mcp_tools_activated"
            | "turn_usage"
            | "compaction" => {},
            _ => self.unknown_record_count += 1,
        }
    }

    fn read_meta(&mut self, value: &Value, uuid: Uuid) {
        self.format_version =
            u64_field(value, "format_version").and_then(|value| u32::try_from(value).ok());
        self.session_id = string_field(value, "session_id").map(str::to_string);
        self.created_at = timestamp_field(value, "timestamp");
        self.working_directory = string_field(value, "working_directory").map(str::to_string);
        self.last_activity_at = self.created_at;
        match self.format_version {
            Some(version) if version == CURRENT_FORMAT_VERSION => {},
            Some(version) => {
                self.state = "corrupt";
                self.error = Some(format!(
                    "unsupported session format_version {version} (expected {CURRENT_FORMAT_VERSION})"
                ));
                return;
            },
            None => {
                self.state = "corrupt";
                self.error = Some("session_meta has no format_version".to_string());
                return;
            },
        }
        if self.session_id.as_deref() != Some(uuid.to_string().as_str()) {
            self.state = "corrupt";
            self.error =
                Some("session_meta session_id does not match the requested session".to_string());
        }
    }

    fn task_start(&mut self, value: &Value) -> usize {
        let task_id = string_field(value, "task_id").unwrap_or("").to_string();
        let index = self.task_index(&task_id);
        let task = &mut self.tasks[index];
        task.started_at = timestamp_field(value, "timestamp").or(task.started_at);
        task.touch(task.started_at);
        task.model = string_field(value, "model")
            .map(str::to_string)
            .or_else(|| task.model.take());
        task.model_config = string_field(value, "model_config")
            .map(str::to_string)
            .or_else(|| task.model_config.take());
        index
    }

    fn task_complete(&mut self, value: &Value, open_task: Option<usize>) {
        let task_id = string_field(value, "task_id").unwrap_or("").to_string();
        let index = open_task
            .filter(|index| self.tasks[*index].task_id == task_id)
            .unwrap_or_else(|| self.task_index(&task_id));
        let ended_at = {
            let task = &mut self.tasks[index];
            let subtype = string_field(value, "subtype")
                .unwrap_or("unknown")
                .to_string();
            task.terminal = Some(TerminalInfo {
                subtype,
                is_error: bool_field(value, "is_error").unwrap_or(false),
            });
            task.duration_ms = u64_field(value, "duration_ms");
            task.turn_count =
                u64_field(value, "turn_count").and_then(|value| u32::try_from(value).ok());
            task.tool_call_count =
                u64_field(value, "tool_call_count").and_then(|value| u32::try_from(value).ok());
            task.usage = UsageSummary::from_value(value.get("usage"));
            if let (Some(started), Some(duration)) = (task.started_at, task.duration_ms) {
                task.ended_at = Some(add_millis(started, duration));
                task.touch(task.ended_at);
            }
            task.ended_at
        };
        self.last_activity_at = max_time(self.last_activity_at, ended_at);
    }

    fn task_index(&mut self, task_id: &str) -> usize {
        if let Some(index) = self.tasks.iter().position(|task| task.task_id == task_id) {
            return index;
        }
        self.tasks.push(TaskAccum::new(task_id.to_string()));
        self.tasks.len() - 1
    }

    fn mark_corrupt(&mut self, error: String) {
        self.state = "corrupt";
        self.error = Some(error);
    }

    fn mark_unreadable(&mut self, error: String) {
        self.state = "unreadable";
        self.error = Some(error);
    }

    fn artifact(&self) -> TranscriptView {
        TranscriptView {
            artifact: ArtifactView {
                state: self.state,
                path: self.path.clone(),
                exists: self.exists,
                bytes: self.bytes,
                modified_at: self.modified_at,
                record_count: self.record_count,
                record_types: self.record_types.clone(),
                unknown_record_count: self.unknown_record_count,
                partial_tail: self.partial_tail,
                ignored_partial_tail: self.ignored_partial_tail,
                error: self.error.clone(),
            },
            format_version: self.format_version,
            session_id: self.session_id.clone(),
            created_at: self.created_at,
            working_directory: self.working_directory.clone(),
            task_count: self.tasks.len(),
            tool_output_bytes: self.tool_output_bytes,
            hook_event_count: self.hook_event_count,
        }
    }
}

/// The parsed transcript fields the report needs. No record content is kept.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag records a distinct artifact fact"
)]
struct TranscriptScan {
    path: String,
    regular_file: bool,
    exists: bool,
    bytes: Option<u64>,
    modified_at: Option<DateTime<Utc>>,
    state: &'static str,
    error: Option<String>,
    format_version: Option<u32>,
    session_id: Option<String>,
    created_at: Option<DateTime<Utc>>,
    working_directory: Option<String>,
    record_count: usize,
    record_types: BTreeMap<String, usize>,
    unknown_record_count: usize,
    partial_tail: bool,
    ignored_partial_tail: bool,
    tasks: Vec<TaskAccum>,
    last_activity_at: Option<DateTime<Utc>>,
    tool_output_bytes: u64,
    hook_event_count: usize,
}

impl TelemetryScan {
    /// Read the telemetry sidecar and classify it without retaining payloads.
    fn read(path: &Path, slow_threshold: u64) -> Self {
        let mut scan = Self::new(path, slow_threshold);
        let Some(file) = scan.prepare(path) else {
            return scan;
        };
        scan.scan_records(file);
        if scan.record_count == 0 && scan.state == "present" {
            scan.mark_corrupt("telemetry sidecar is empty".to_string());
        }
        scan
    }

    fn new(path: &Path, slow_threshold: u64) -> Self {
        Self {
            path: path.display().to_string(),
            exists: false,
            bytes: None,
            modified_at: None,
            state: "missing",
            error: None,
            record_count: 0,
            record_types: BTreeMap::new(),
            unknown_record_count: 0,
            partial_tail: false,
            ignored_partial_tail: false,
            invocations: Vec::new(),
            retries: Vec::new(),
            slow_operations: Vec::new(),
            last_activity_at: None,
            slow_threshold,
        }
    }

    fn prepare(&mut self, path: &Path) -> Option<fs::File> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => {
                self.mark_unreadable(format!("cannot inspect telemetry sidecar: {error}"));
                return None;
            },
        };
        self.exists = true;
        self.bytes = Some(metadata.len());
        self.modified_at = metadata.modified().ok().map(DateTime::<Utc>::from);
        if metadata.file_type().is_symlink() {
            self.mark_unreadable("telemetry sidecar is a symlink".to_string());
            return None;
        }
        if !metadata.is_file() {
            self.mark_unreadable("telemetry sidecar is not a regular file".to_string());
            return None;
        }
        self.state = "present";
        match fs::File::open(path) {
            Ok(file) => Some(file),
            Err(error) => {
                self.mark_unreadable(format!("cannot open telemetry sidecar: {error}"));
                None
            },
        }
    }

    fn scan_records(&mut self, file: fs::File) {
        let mut framer = SessionFramer::new(BufReader::new(file));
        loop {
            let line = match framer.next_record() {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(error) => {
                    self.mark_unreadable(format!("cannot read telemetry sidecar: {error}"));
                    return;
                },
            };
            match self.scan_line(&line) {
                Ok(true) => {},
                Ok(false) => break,
                Err(error) => {
                    self.mark_corrupt(error);
                    return;
                },
            }
        }
    }

    fn scan_line(&mut self, line: &FramedSessionLine) -> Result<bool, String> {
        if line.partial_tail {
            self.partial_tail = true;
        }
        let value: Value = match serde_json::from_str(&line.text) {
            Ok(value) => value,
            Err(_) if line.partial_tail => {
                self.ignored_partial_tail = true;
                return Ok(false);
            },
            Err(_) => {
                return Err(format!(
                    "invalid telemetry record on line {}",
                    line.line_number
                ));
            },
        };
        let Some(record_type) = value.get("type").and_then(Value::as_str) else {
            return Err(format!(
                "telemetry record on line {} has no type",
                line.line_number
            ));
        };
        self.record_count += 1;
        increment(&mut self.record_types, record_type);
        let timestamp = timestamp_field(&value, "timestamp");
        self.last_activity_at = max_time(self.last_activity_at, timestamp);
        let invocation_id = string_field(&value, "invocation_id")
            .unwrap_or("<unknown>")
            .to_string();
        let index = self.invocation_index(&invocation_id);
        self.invocations[index].record_count += 1;
        self.invocations[index].touch(timestamp);
        self.consume_record(record_type, &value, index, timestamp);
        Ok(true)
    }

    fn invocation_index(&mut self, invocation_id: &str) -> usize {
        if let Some(index) = self
            .invocations
            .iter()
            .position(|invocation| invocation.invocation_id == invocation_id)
        {
            return index;
        }
        self.invocations
            .push(InvocationAccum::new(invocation_id.to_string()));
        self.invocations.len() - 1
    }

    fn consume_record(
        &mut self,
        record_type: &str,
        value: &Value,
        index: usize,
        timestamp: Option<DateTime<Utc>>,
    ) {
        let invocation_id = self.invocations[index].invocation_id.clone();
        match record_type {
            "telemetry_init" => self.consume_init(value, index),
            "api_attempt_in_flight" | "api_attempt" => {
                self.consume_attempt(record_type, value, index, timestamp);
            },
            "retry_scheduled" | "retry_wait" => {
                self.consume_retry(record_type, value, index, timestamp, &invocation_id);
            },
            "tool_call" => self.consume_tool(value, index, timestamp),
            "compensation" | "gate_startup" | "gate_decision" | "gate_evidence" => {
                self.consume_gate(record_type, value, index, timestamp);
            },
            "judge_attempt" | "type_safe_shadow" | "session_summary" => {
                self.consume_summary(record_type, value, index, timestamp);
            },
            _ => self.invocations[index].unknown_record_count += 1,
        }
    }

    fn consume_init(&mut self, value: &Value, index: usize) {
        let invocation = &mut self.invocations[index];
        invocation.mode = string_field(value, "mode").map(str::to_string);
        invocation.model = string_field(value, "model").map(str::to_string);
        invocation.api_type = string_field(value, "api_type").map(str::to_string);
        invocation.output_format = string_field(value, "output_format").map(str::to_string);
        invocation.tool_count = value.get("tools").and_then(Value::as_array).map(Vec::len);
    }

    fn consume_attempt(
        &mut self,
        record_type: &str,
        value: &Value,
        index: usize,
        timestamp: Option<DateTime<Utc>>,
    ) {
        match record_type {
            "api_attempt_in_flight" => {
                if let Some(task_id) = string_field(value, "task_id") {
                    self.invocations[index]
                        .observed_task_ids
                        .insert(task_id.to_string());
                }
            },
            "api_attempt" => {
                self.invocations[index].api_attempts += 1;
                let terminal = string_field(value, "terminal_class");
                if terminal.is_some_and(|terminal| terminal != "completed") {
                    self.invocations[index].failed_attempts += 1;
                }
                if let Some(duration) = u64_field(value, "total_ms") {
                    self.maybe_slow(index, timestamp, "api_attempt", duration, None);
                }
            },
            _ => {},
        }
    }

    fn consume_retry(
        &mut self,
        record_type: &str,
        value: &Value,
        index: usize,
        timestamp: Option<DateTime<Utc>>,
        invocation_id: &str,
    ) {
        match record_type {
            "retry_scheduled" => {
                self.invocations[index].retry_count += 1;
                self.retries.push(RawRetry {
                    invocation_id: invocation_id.to_string(),
                    timestamp,
                    attempt: u64_field(value, "attempt").unwrap_or(0),
                    max_retries: u64_field(value, "max_retries").unwrap_or(0),
                    reason: string_field(value, "reason")
                        .unwrap_or("unknown")
                        .to_string(),
                    delay_ms: u64_field(value, "delay_ms").unwrap_or(0),
                    changed_request_overrides: bool_field(value, "changed_request_overrides")
                        .unwrap_or(false),
                });
            },
            "retry_wait" => {
                self.invocations[index].retry_waits += 1;
                if let Some(duration) = u64_field(value, "duration_ms") {
                    self.maybe_slow(index, timestamp, "retry_wait", duration, None);
                }
            },
            _ => {},
        }
    }

    fn consume_tool(&mut self, value: &Value, index: usize, timestamp: Option<DateTime<Utc>>) {
        self.invocations[index].tool_calls += 1;
        if bool_field(value, "was_error").unwrap_or(false) {
            self.invocations[index].tool_errors += 1;
        }
        if let Some(duration) = u64_field(value, "duration_ms") {
            self.maybe_slow(
                index,
                timestamp,
                "tool_call",
                duration,
                string_field(value, "name").map(str::to_string),
            );
        }
    }

    fn consume_gate(
        &mut self,
        record_type: &str,
        value: &Value,
        index: usize,
        timestamp: Option<DateTime<Utc>>,
    ) {
        match record_type {
            "compensation" => self.invocations[index].compensations += 1,
            "gate_startup" => {
                self.invocations[index].gate_startups += 1;
                if let Some(duration) = u64_field(value, "latency_ms") {
                    self.maybe_slow(
                        index,
                        timestamp,
                        "gate_startup",
                        duration,
                        string_field(value, "gate").map(str::to_string),
                    );
                }
            },
            "gate_decision" => {
                self.invocations[index].gate_decisions += 1;
                if let Some(duration) = u64_field(value, "latency_ms") {
                    self.maybe_slow(
                        index,
                        timestamp,
                        "gate_decision",
                        duration,
                        string_field(value, "gate").map(str::to_string),
                    );
                }
            },
            "gate_evidence" => self.invocations[index].gate_evidence += 1,
            _ => {},
        }
    }

    fn consume_summary(
        &mut self,
        record_type: &str,
        value: &Value,
        index: usize,
        timestamp: Option<DateTime<Utc>>,
    ) {
        match record_type {
            "judge_attempt" => {
                if let Some(duration) = u64_field(value, "total_ms") {
                    self.maybe_slow(index, timestamp, "judge_attempt", duration, None);
                }
            },
            "type_safe_shadow" => {
                if let Some(duration) = u64_field(value, "elapsed_ms") {
                    self.maybe_slow(index, timestamp, "type_safe_shadow", duration, None);
                }
            },
            "session_summary" => {
                let summary = InvocationSummary {
                    success: bool_field(value, "success").unwrap_or(false),
                    duration_ms: u64_field(value, "duration_ms").unwrap_or(0),
                    turn_count: u32::try_from(u64_field(value, "turn_count").unwrap_or(0))
                        .unwrap_or(u32::MAX),
                    usage: UsageSummary::from_value(value.get("usage")),
                };
                self.invocations[index].summary = Some(summary);
            },
            _ => {},
        }
    }

    fn maybe_slow(
        &mut self,
        index: usize,
        timestamp: Option<DateTime<Utc>>,
        kind: &'static str,
        duration_ms: u64,
        subject: Option<String>,
    ) {
        if duration_ms < self.slow_threshold {
            return;
        }
        self.invocations[index].slow_count += 1;
        self.slow_operations.push(RawSlowOperation {
            invocation_id: self.invocations[index].invocation_id.clone(),
            timestamp,
            kind,
            duration_ms,
            subject,
        });
    }

    fn mark_corrupt(&mut self, error: String) {
        self.state = "corrupt";
        self.error = Some(error);
    }

    fn mark_unreadable(&mut self, error: String) {
        self.state = "unreadable";
        self.error = Some(error);
    }

    fn artifact(&self) -> TelemetryView {
        TelemetryView {
            artifact: ArtifactView {
                state: self.state,
                path: self.path.clone(),
                exists: self.exists,
                bytes: self.bytes,
                modified_at: self.modified_at,
                record_count: self.record_count,
                record_types: self.record_types.clone(),
                unknown_record_count: self.unknown_record_count,
                partial_tail: self.partial_tail,
                ignored_partial_tail: self.ignored_partial_tail,
                error: self.error.clone(),
            },
            invocation_count: self.invocations.len(),
        }
    }
}

/// The parsed telemetry fields the report needs. No payload is retained.
struct TelemetryScan {
    path: String,
    exists: bool,
    bytes: Option<u64>,
    modified_at: Option<DateTime<Utc>>,
    state: &'static str,
    error: Option<String>,
    record_count: usize,
    record_types: BTreeMap<String, usize>,
    unknown_record_count: usize,
    partial_tail: bool,
    ignored_partial_tail: bool,
    invocations: Vec<InvocationAccum>,
    retries: Vec<RawRetry>,
    slow_operations: Vec<RawSlowOperation>,
    last_activity_at: Option<DateTime<Utc>>,
    slow_threshold: u64,
}

fn assign_task_states(tasks: &mut [TaskAccum], liveness: SessionLiveness) {
    for task in tasks {
        task.state = match task
            .terminal
            .as_ref()
            .map(|terminal| terminal.subtype.as_str())
        {
            Some("interrupted") => "interrupted",
            Some(_) => "completed",
            None => match liveness {
                SessionLiveness::Live => "live",
                SessionLiveness::Unknown => "unknown_liveness",
                SessionLiveness::NotLive => "interrupted",
            },
        };
    }
}

fn correlate_invocations(tasks: &mut [TaskAccum], invocations: &mut [InvocationAccum]) {
    let known_task_ids = tasks
        .iter()
        .map(|task| task.task_id.clone())
        .collect::<BTreeSet<_>>();
    let mut matched_tasks = BTreeSet::new();
    match_by_task_id(tasks, invocations, &known_task_ids, &mut matched_tasks);
    match_by_start_time(tasks, invocations, &mut matched_tasks);
}

fn match_by_task_id(
    tasks: &mut [TaskAccum],
    invocations: &mut [InvocationAccum],
    known_task_ids: &BTreeSet<String>,
    matched_tasks: &mut BTreeSet<usize>,
) {
    for invocation in invocations {
        let Some((task_id, index)) = direct_task_match(tasks, invocation, known_task_ids) else {
            continue;
        };
        invocation.task_id = Some(task_id.clone());
        invocation.task_match = Some("task_id");
        tasks[index].invocation_id = Some(invocation.invocation_id.clone());
        tasks[index].invocation_match = Some("task_id");
        matched_tasks.insert(index);
    }
}

fn direct_task_match(
    tasks: &[TaskAccum],
    invocation: &InvocationAccum,
    known_task_ids: &BTreeSet<String>,
) -> Option<(String, usize)> {
    let observed = invocation
        .observed_task_ids
        .iter()
        .filter(|task_id| known_task_ids.contains(*task_id))
        .cloned()
        .collect::<Vec<_>>();
    if observed.len() != 1 {
        return None;
    }
    let task_id = observed[0].clone();
    tasks
        .iter()
        .position(|task| task.task_id == task_id)
        .map(|index| (task_id, index))
}

fn match_by_start_time(
    tasks: &mut [TaskAccum],
    invocations: &mut [InvocationAccum],
    matched_tasks: &mut BTreeSet<usize>,
) {
    let mut candidates = start_time_candidates(tasks, invocations, matched_tasks);
    candidates.sort_by_key(|(delta, invocation_index, task_index)| {
        (*delta, *invocation_index, *task_index)
    });
    for (_, invocation_index, task_index) in candidates {
        if invocations[invocation_index].task_id.is_some() || matched_tasks.contains(&task_index) {
            continue;
        }
        invocations[invocation_index].task_id = Some(tasks[task_index].task_id.clone());
        invocations[invocation_index].task_match = Some("start_time");
        tasks[task_index].invocation_id = Some(invocations[invocation_index].invocation_id.clone());
        tasks[task_index].invocation_match = Some("start_time");
        matched_tasks.insert(task_index);
    }
}

fn start_time_candidates(
    tasks: &[TaskAccum],
    invocations: &[InvocationAccum],
    matched_tasks: &BTreeSet<usize>,
) -> Vec<(i64, usize, usize)> {
    let mut candidates = Vec::new();
    for (invocation_index, invocation) in invocations.iter().enumerate() {
        if invocation.task_id.is_some() {
            continue;
        }
        let Some(started) = invocation.started_at else {
            continue;
        };
        for (task_index, task) in tasks.iter().enumerate() {
            if matched_tasks.contains(&task_index) {
                continue;
            }
            let Some(task_started) = task.started_at else {
                continue;
            };
            let delta = (started - task_started).num_milliseconds().abs();
            if delta <= START_TIME_MATCH_WINDOW_MS {
                candidates.push((delta, invocation_index, task_index));
            }
        }
    }
    candidates
}

fn invocation_counter(
    invocations: &[InvocationAccum],
    selector: impl Fn(&InvocationAccum) -> usize,
) -> BTreeMap<String, usize> {
    invocations
        .iter()
        .map(|invocation| (invocation.invocation_id.clone(), selector(invocation)))
        .collect()
}

fn build_checks(
    transcript: &TranscriptScan,
    telemetry: &TelemetryScan,
    liveness: SessionLiveness,
    tasks: &[TaskRow],
    invocations: &[InvocationRow],
    config_checks: &[DiagnosticCheck],
) -> Vec<DiagnosticCheck> {
    let mut checks = transcript_checks(transcript);
    checks.extend(telemetry_checks(telemetry));
    if liveness == SessionLiveness::Unknown {
        checks.push(DiagnosticCheck::warning(
            CHECK_SESSION_LIVENESS_UNKNOWN,
            "the session lock state could not be determined",
        ));
    }
    checks.extend(task_checks(tasks));
    checks.extend(invocation_checks(invocations));
    checks.extend(config_checks.iter().cloned());
    checks
}

fn transcript_checks(transcript: &TranscriptScan) -> Vec<DiagnosticCheck> {
    let mut checks = Vec::new();
    match transcript.state {
        "corrupt" => checks.push(DiagnosticCheck::error(
            CHECK_SESSION_TRANSCRIPT_CORRUPT,
            format!(
                "session transcript is corrupt: {}",
                transcript.error.as_deref().unwrap_or("unknown cause")
            ),
        )),
        "unreadable" => checks.push(DiagnosticCheck::error(
            CHECK_SESSION_TRANSCRIPT_UNREADABLE,
            format!(
                "session transcript is unreadable: {}",
                transcript.error.as_deref().unwrap_or("unknown cause")
            ),
        )),
        _ => {},
    }
    if transcript.ignored_partial_tail {
        checks.push(DiagnosticCheck::warning(
            CHECK_SESSION_PARTIAL_TAIL,
            "session transcript has a truncated final record that was ignored",
        ));
    } else if transcript.partial_tail {
        checks.push(DiagnosticCheck::warning(
            CHECK_SESSION_PARTIAL_TAIL,
            "session transcript has an unterminated final record",
        ));
    }
    checks
}

fn telemetry_checks(telemetry: &TelemetryScan) -> Vec<DiagnosticCheck> {
    let mut checks = Vec::new();
    match telemetry.state {
        "missing" => checks.push(DiagnosticCheck::warning(
            CHECK_SESSION_TELEMETRY_MISSING,
            format!("no telemetry sidecar exists at {}", telemetry.path),
        )),
        "corrupt" => checks.push(DiagnosticCheck::error(
            CHECK_SESSION_TELEMETRY_CORRUPT,
            format!(
                "telemetry sidecar is corrupt: {}",
                telemetry.error.as_deref().unwrap_or("unknown cause")
            ),
        )),
        "unreadable" => checks.push(DiagnosticCheck::error(
            CHECK_SESSION_TELEMETRY_UNREADABLE,
            format!(
                "telemetry sidecar is unreadable: {}",
                telemetry.error.as_deref().unwrap_or("unknown cause")
            ),
        )),
        _ => {},
    }
    if telemetry.ignored_partial_tail {
        checks.push(DiagnosticCheck::warning(
            CHECK_SESSION_PARTIAL_TAIL,
            "telemetry sidecar has a truncated final record that was ignored",
        ));
    } else if telemetry.partial_tail {
        checks.push(DiagnosticCheck::warning(
            CHECK_SESSION_PARTIAL_TAIL,
            "telemetry sidecar has an unterminated final record",
        ));
    }
    checks
}

fn task_checks(tasks: &[TaskRow]) -> Vec<DiagnosticCheck> {
    tasks
        .iter()
        .filter(|task| matches!(task.state, "interrupted" | "unknown_liveness" | "corrupt"))
        .map(|task| {
            DiagnosticCheck::warning(
                CHECK_SESSION_INCOMPLETE_TASK,
                format!(
                    "task {} has no terminal completion record (state {})",
                    task.task_id,
                    state_label(task.state)
                ),
            )
        })
        .collect()
}

fn invocation_checks(invocations: &[InvocationRow]) -> Vec<DiagnosticCheck> {
    invocations
        .iter()
        .filter(|invocation| invocation.task_id.is_none())
        .map(|invocation| {
            DiagnosticCheck::warning(
                CHECK_SESSION_INVOCATION_UNMATCHED,
                format!(
                    "telemetry invocation {} could not be correlated with a transcript task",
                    invocation.invocation_id
                ),
            )
        })
        .collect()
}

fn overall_state(
    transcript_state: &'static str,
    liveness: SessionLiveness,
    tasks: &[TaskRow],
) -> &'static str {
    if transcript_state != "ok" {
        return "corrupt";
    }
    if liveness == SessionLiveness::Live {
        return "live";
    }
    match tasks.last().map(|task| task.state) {
        Some("completed") => "completed",
        Some("interrupted") => "interrupted",
        Some("unknown_liveness") => "unknown_liveness",
        Some("corrupt") => "corrupt",
        _ => {
            if liveness == SessionLiveness::Unknown {
                "unknown_liveness"
            } else {
                "interrupted"
            }
        },
    }
}

fn diagnosis_complete(transcript: &TranscriptScan, telemetry: &TelemetryScan) -> bool {
    transcript.state == "ok"
        && !transcript.partial_tail
        && !is_broken_artifact(telemetry.state)
        && !telemetry.partial_tail
}

fn is_broken_artifact(state: &str) -> bool {
    matches!(state, "corrupt" | "unreadable")
}

fn current_config_context(
    data_dir: &DataDir,
    options: &CommandRunOptions<'_>,
    json: bool,
) -> (Option<Value>, Vec<DiagnosticCheck>) {
    let unavailable = || {
        vec![DiagnosticCheck::warning(
            CHECK_SESSION_CONFIG_UNAVAILABLE,
            "current configuration context is unavailable",
        )]
    };
    let Ok(load) = try_load_settings(options) else {
        if !json {
            eprintln!("warning: current configuration context is unavailable");
        }
        return (None, unavailable());
    };
    if !json {
        load.settings.print_warnings();
        crate::cli::debug::print_project_trust_warning(&load.trust);
    }
    let inputs = ConfigInputs {
        sandbox: None,
        add_dir: &[],
        no_skills: false,
        skills: None,
        no_tools: false,
        tools: None,
        toolbox: &[],
    };
    ConfigReport::build(
        &load.settings,
        &load.trust,
        &load.hook_files,
        data_dir,
        &inputs,
        options,
    )
    .map_or_else(
        |_| {
            if !json {
                eprintln!("warning: current configuration context is unavailable");
            }
            (None, unavailable())
        },
        |report| (Some(report.session_context()), report.session_checks()),
    )
}

fn scan_logs(
    data_dir: &DataDir,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> LogScan {
    let directory = data_dir.get_cache_dir();
    let mut report = ArtifactsReport {
        log_directory: directory.display().to_string(),
        logs: Vec::new(),
        logs_truncated: 0,
    };
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return LogScan {
                report,
                warning: None,
            };
        },
        Err(error) => {
            return LogScan {
                report,
                warning: Some(format!(
                    "could not read log directory {}: {error}",
                    directory.display()
                )),
            };
        },
    };
    let (lower, upper) = log_date_range(from, to);
    let (mut logs, warning) = collect_logs(entries, &directory, lower, upper);
    logs.sort_by(|left, right| {
        left.date
            .cmp(&right.date)
            .then_with(|| left.path.cmp(&right.path))
    });
    if logs.len() > LOG_LIMIT {
        report.logs_truncated = logs.len() - LOG_LIMIT;
        logs.truncate(LOG_LIMIT);
    }
    report.logs = logs;
    LogScan { report, warning }
}

fn log_date_range(
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> (Option<NaiveDate>, Option<NaiveDate>) {
    let lower = from
        .map(|time| time.date_naive())
        .and_then(|date| date.checked_sub_signed(chrono::Duration::days(1)));
    let upper = to
        .map(|time| time.date_naive())
        .and_then(|date| date.checked_add_signed(chrono::Duration::days(1)));
    (lower, upper)
}

fn collect_logs(
    entries: impl Iterator<Item = std::io::Result<fs::DirEntry>>,
    directory: &Path,
    lower: Option<NaiveDate>,
    upper: Option<NaiveDate>,
) -> (Vec<LogArtifact>, Option<String>) {
    let mut logs = Vec::new();
    let mut warning = None;
    for entry in entries {
        if let Some(message) = collect_log_entry(entry, directory, lower, upper, &mut logs)
            && warning.is_none()
        {
            warning = Some(message);
        }
    }
    (logs, warning)
}

fn collect_log_entry(
    entry: std::io::Result<fs::DirEntry>,
    directory: &Path,
    lower: Option<NaiveDate>,
    upper: Option<NaiveDate>,
    logs: &mut Vec<LogArtifact>,
) -> Option<String> {
    let entry = match entry {
        Ok(entry) => entry,
        Err(error) => {
            return Some(format!(
                "could not inspect a log directory entry in {}: {error}",
                directory.display()
            ));
        },
    };
    let path = entry.path();
    let date = log_date(&entry.file_name().to_string_lossy())?;
    if lower.is_some_and(|lower| date < lower) || upper.is_some_and(|upper| date > upper) {
        return None;
    }
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => {
            return Some(format!(
                "could not inspect log file {}: {error}",
                path.display()
            ));
        },
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return None;
    }
    logs.push(LogArtifact {
        date: date.format("%Y-%m-%d").to_string(),
        path: path.display().to_string(),
        bytes: metadata.len(),
        modified_at: metadata.modified().ok().map(DateTime::<Utc>::from),
    });
    None
}

struct LogScan {
    report: ArtifactsReport,
    warning: Option<String>,
}

fn log_date(file_name: &str) -> Option<NaiveDate> {
    let date = file_name.strip_prefix("cake.")?.strip_suffix(".log")?;
    NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()
}

fn write_export(
    path: &Path,
    document: &str,
    data: &SessionData,
) -> Result<(), SessionDiagnosticError> {
    let rendered = path.display().to_string();
    if is_source_artifact(path, data) {
        return Err(SessionDiagnosticError::ExportSource(rendered));
    }
    match fs::symlink_metadata(path) {
        Ok(_) => return Err(SessionDiagnosticError::ExportExists(rendered)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => {
            return Err(SessionDiagnosticError::Export(
                rendered,
                format!("cannot inspect target: {error}"),
            ));
        },
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return Err(SessionDiagnosticError::ExportParent(
            parent.display().to_string(),
        ));
    }

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    owner_only::apply_session_options(&mut options);
    let mut file = options.open(path).map_err(|error| {
        SessionDiagnosticError::Export(rendered.clone(), format!("cannot create file: {error}"))
    })?;
    owner_only::set_file_owner_only(&file).map_err(|error| {
        SessionDiagnosticError::Export(rendered.clone(), format!("cannot set permissions: {error}"))
    })?;
    file.write_all(document.as_bytes()).map_err(|error| {
        SessionDiagnosticError::Export(rendered.clone(), format!("cannot write file: {error}"))
    })?;
    file.flush().map_err(|error| {
        SessionDiagnosticError::Export(rendered.clone(), format!("cannot flush file: {error}"))
    })?;
    Ok(())
}

fn is_source_artifact(path: &Path, data: &SessionData) -> bool {
    let target = normalized_path(path);
    [
        &data.transcript.artifact.path,
        &data.telemetry.artifact.path,
    ]
    .into_iter()
    .any(|source| normalized_path(Path::new(source)) == target)
}

fn normalized_path(path: &Path) -> PathBuf {
    let Some(file_name) = path.file_name() else {
        return path.to_path_buf();
    };
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::canonicalize(parent).map_or_else(|_| path.to_path_buf(), |parent| parent.join(file_name))
}

fn append_artifact(output: &mut String, indent: &str, path: &str, state: &str, bytes: Option<u64>) {
    match bytes {
        Some(bytes) => _ = writeln!(output, "{indent}{path} ({state}, {})", format_bytes(bytes)),
        None => _ = writeln!(output, "{indent}{path} ({state})"),
    }
}

fn append_tasks(output: &mut String, tasks: &[TaskRow]) {
    output.push_str("\nTasks\n");
    if tasks.is_empty() {
        output.push_str("  (none)\n");
        return;
    }
    for task in tasks {
        _ = writeln!(
            output,
            "  {}  {} ({})",
            task.task_id,
            state_label(task.state),
            task.termination.as_deref().unwrap_or("unknown")
        );
        _ = writeln!(
            output,
            "    Start: {}  End: {}",
            format_time(task.started_at),
            format_time(task.ended_at)
        );
        _ = writeln!(
            output,
            "    Records: {}  Hook events: {}  Retries: {}  Slow operations: {}",
            task.record_count, task.hook_events, task.retries, task.slow_operations
        );
        if let Some(usage) = task.usage {
            _ = writeln!(
                output,
                "    Usage: {} input, {} output, {} total",
                usage.input_tokens, usage.output_tokens, usage.total_tokens
            );
        }
        match &task.invocation_id {
            Some(id) => {
                _ = writeln!(
                    output,
                    "    Invocation: {id} ({})",
                    task.invocation_match.unwrap_or("unmatched")
                );
            },
            None => output.push_str("    Invocation: none\n"),
        }
    }
}

fn append_invocations(output: &mut String, invocations: &[InvocationRow]) {
    output.push_str("\nInvocations\n");
    if invocations.is_empty() {
        output.push_str("  (none)\n");
        return;
    }
    for invocation in invocations {
        _ = writeln!(
            output,
            "  {}  {}  model={}",
            invocation.invocation_id,
            invocation.mode.as_deref().unwrap_or("unknown"),
            invocation.model.as_deref().unwrap_or("unknown")
        );
        _ = writeln!(
            output,
            "    Start: {}  End: {}  Records: {}",
            format_time(invocation.started_at),
            format_time(invocation.ended_at),
            invocation.record_count
        );
        _ = writeln!(
            output,
            "    Attempts: {} (failed {})  Retries: {}  Tool calls: {} (errors {})  Gates: {}",
            invocation.api_attempts,
            invocation.failed_attempts,
            invocation.retry_count,
            invocation.tool_calls,
            invocation.tool_errors,
            invocation.gate_decisions + invocation.gate_startups
        );
        if let Some(summary) = &invocation.summary {
            _ = writeln!(
                output,
                "    Summary: success={} duration={} ms turns={}",
                yes_no(summary.success),
                summary.duration_ms,
                summary.turn_count
            );
        }
        match &invocation.task_id {
            Some(id) => {
                _ = writeln!(
                    output,
                    "    Task: {id} ({})",
                    invocation.task_match.unwrap_or("unmatched")
                );
            },
            None => output.push_str("    Task: unmatched\n"),
        }
    }
}

fn append_retries(output: &mut String, retries: &[RetryRow]) {
    output.push_str("\nRetries\n");
    if retries.is_empty() {
        output.push_str("  (none)\n");
        return;
    }
    for retry in retries {
        _ = writeln!(
            output,
            "  {}  {}  attempt {}/{}  {}  delay {} ms  overrides changed: {}",
            format_time(retry.timestamp),
            retry.invocation_id,
            retry.attempt,
            retry.max_retries,
            retry.reason,
            retry.delay_ms,
            yes_no(retry.changed_request_overrides)
        );
    }
}

fn append_slow_operations(
    output: &mut String,
    operations: &[SlowOperationRow],
    threshold_ms: u64,
    total: usize,
) {
    _ = writeln!(
        output,
        "\nSlow operations (threshold {threshold_ms} ms; showing {} of {total})",
        operations.len()
    );
    if operations.is_empty() {
        output.push_str("  (none)\n");
        return;
    }
    for operation in operations {
        _ = writeln!(
            output,
            "  {} ms  {}  {}  {}  {}",
            operation.duration_ms,
            operation.kind,
            operation.subject.as_deref().unwrap_or("-"),
            format_time(operation.timestamp),
            operation.invocation_id
        );
    }
}

fn append_current_config(output: &mut String, config: Option<&Value>) {
    output.push_str("\nCurrent configuration\n");
    let Some(config) = config else {
        output.push_str("  unavailable\n");
        return;
    };
    let model = &config["model"];
    _ = writeln!(
        output,
        "  Model: {} (configured: {})",
        model["name"].as_str().unwrap_or("(none)"),
        yes_no(model["configured"].as_bool().unwrap_or(false))
    );
    let grants = &config["grants"];
    _ = writeln!(
        output,
        "  Grants: {} read-write, {} read-only, {} denied",
        grants["read_write_count"].as_u64().unwrap_or(0),
        grants["read_only_count"].as_u64().unwrap_or(0),
        grants["denied_count"].as_u64().unwrap_or(0)
    );
    _ = writeln!(
        output,
        "  Project trust: {}",
        config["project_trust"]["state"]
            .as_str()
            .unwrap_or("unknown")
    );
}

fn append_diagnostics(output: &mut String, checks: &[DiagnosticCheck]) {
    let text_checks = checks.iter().filter(|check| {
        !matches!(
            check.id.as_str(),
            "settings.unknown_key" | "config.project_trust" | "session.current_config_unavailable"
        )
    });
    let mut wrote_header = false;
    for check in text_checks {
        if !wrote_header {
            output.push_str("\nDiagnostics\n");
            wrote_header = true;
        }
        _ = writeln!(output, "  {}: {}", check.status.label(), check.message);
    }
}

fn sort_slow_operations(operations: &mut [SlowOperationRow]) {
    operations.sort_by(|left, right| {
        right
            .duration_ms
            .cmp(&left.duration_ms)
            .then_with(|| left.timestamp.cmp(&right.timestamp))
            .then_with(|| left.kind.cmp(right.kind))
            .then_with(|| left.invocation_id.cmp(&right.invocation_id))
            .then_with(|| left.subject.cmp(&right.subject))
    });
}

fn increment(map: &mut BTreeMap<String, usize>, key: &str) {
    *map.entry(key.to_string()).or_insert(0) += 1;
}

fn string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn bool_field(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

fn u64_field(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn nested_u64_field(value: &Value, parent: &str, key: &str) -> Option<u64> {
    value.get(parent).and_then(|parent| u64_field(parent, key))
}

fn timestamp_field(value: &Value, key: &str) -> Option<DateTime<Utc>> {
    let raw = string_field(value, key)?;
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn max_time(
    current: Option<DateTime<Utc>>,
    candidate: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.max(candidate)),
        (Some(current), None) => Some(current),
        (None, candidate) => candidate,
    }
}

fn add_millis(started: DateTime<Utc>, milliseconds: u64) -> DateTime<Utc> {
    let milliseconds = i64::try_from(milliseconds).unwrap_or(i64::MAX);
    started + chrono::Duration::milliseconds(milliseconds)
}

fn latest_activity(
    transcript: Option<DateTime<Utc>>,
    telemetry: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    max_time(transcript, telemetry)
}

fn state_label(state: &str) -> &str {
    match state {
        "unknown_liveness" => "unknown-liveness",
        other => other,
    }
}

fn liveness_label(state: &str) -> &str {
    match state {
        "not_live" => "not held",
        "live" => "active writer",
        "unknown" => "unknown",
        other => other,
    }
}

const fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn format_time(value: Option<DateTime<Utc>>) -> String {
    value.map_or_else(|| "-".to_string(), |value| value.to_rfc3339())
}

#[expect(
    clippy::cast_precision_loss,
    reason = "human-readable byte sizes do not need the full u64 mantissa"
)]
fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_liveness_is_a_distinct_state() {
        assert_eq!(
            LivenessReport::new(SessionLiveness::Unknown).state,
            "unknown"
        );
        assert_eq!(
            overall_state("ok", SessionLiveness::Unknown, &[]),
            "unknown_liveness"
        );
        assert_eq!(overall_state("ok", SessionLiveness::Live, &[]), "live");
        assert_eq!(
            overall_state("ok", SessionLiveness::NotLive, &[]),
            "interrupted"
        );
    }

    #[test]
    fn log_names_parse_only_daily_rotation() {
        assert_eq!(
            log_date("cake.2026-01-02.log"),
            NaiveDate::from_ymd_opt(2026, 1, 2)
        );
        assert_eq!(log_date("cake.log"), None);
        assert_eq!(log_date("other.2026-01-02.log"), None);
        assert_eq!(log_date("cake.2026-01-02.txt"), None);
    }

    #[test]
    fn last_activity_names_the_newer_and_tied_sources() {
        let earlier = DateTime::parse_from_rfc3339("2026-01-02T03:00:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);
        let later = DateTime::parse_from_rfc3339("2026-01-02T03:01:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);
        assert_eq!(
            LastActivity::new(Some(earlier), Some(later)).source,
            Some("telemetry")
        );
        assert_eq!(
            LastActivity::new(Some(later), Some(earlier)).source,
            Some("transcript")
        );
        assert_eq!(
            LastActivity::new(Some(earlier), Some(earlier)).source,
            Some("both")
        );
        assert_eq!(LastActivity::new(None, None).source, None);
    }
}
