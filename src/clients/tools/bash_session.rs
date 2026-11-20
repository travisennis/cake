use serde::Deserialize;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use crate::clients::tools::bash_session_core::{SessionRead, SessionRegistry};
use crate::session_telemetry::{CompensationEventTelemetry, CompensationKind};
use crate::time_format::format_seconds_tenths;

const READ_WAIT_DEFAULT: u64 = 10;
const READ_WAIT_MAX: u64 = 120;
const COMMAND_PREVIEW_CHARS: usize = 100;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
enum Action {
    Read { session: String, wait: Option<u64> },
    Kill { session: String },
    List,
}

pub(super) fn bash_session_tool() -> super::Tool {
    super::Tool {
        type_: "function".to_string(),
        name: "BashSession".to_string(),
        description: include_str!("bash-session-description.txt").to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["read", "kill", "list"]},
                "session": {"type": "string", "description": "Session ID returned by Bash; required for read and kill"},
                "wait": {"type": "integer", "minimum": 0, "description": "Whole seconds to wait for new output on read (default 10, maximum 120; 0 polls immediately)"}
            },
            "required": ["action"]
        }),
    }
}

pub(super) async fn execute(
    context: &super::ToolContext,
    arguments: &str,
) -> Result<super::ToolResult, super::ToolError> {
    let action: Action = serde_json::from_str(arguments)
        .map_err(|e| super::ToolError::new(format!("Invalid BashSession arguments: {e}")))?;
    let registry = &context.bash_sessions;
    match action {
        Action::Read { session, wait } => {
            let wait = Duration::from_secs(wait.unwrap_or(READ_WAIT_DEFAULT).min(READ_WAIT_MAX));
            let read = registry
                .read_wait(&session, wait)
                .await
                .map_err(super::ToolError::new)?;
            Ok(format_read(
                &session,
                &read,
                context.limits.bash_output_max_bytes,
            ))
        },
        Action::Kill { session } => {
            registry
                .request_kill(&session)
                .map_err(super::ToolError::new)?;
            registry
                .wait_for_completion(&session, Duration::from_secs(15))
                .await
                .map_err(super::ToolError::new)?;
            let read = registry
                .read(&session, Instant::now())
                .map_err(super::ToolError::new)?;
            if read.exit_code.is_none() {
                return Err(super::ToolError::new(format!(
                    "Kill requested for Bash session {session}, but it has not exited; use BashSession read to check it"
                )));
            }
            Ok(format_read(
                &session,
                &read,
                context.limits.bash_output_max_bytes,
            ))
        },
        Action::List => Ok(super::ToolResult {
            output: format_list(registry),
            images: Vec::new(),
            compensation_events: Vec::new(),
            permission_denials: Vec::new(),
        }),
    }
}

pub(super) fn format_read(
    id: &str,
    read: &SessionRead,
    max_bytes: Option<usize>,
) -> super::ToolResult {
    let mut events = Vec::new();
    let (mut output, truncated) = cap_output(&read.output.output, max_bytes);
    if truncated {
        match super::bash::spill_output(&read.output.output) {
            Ok(path) => {
                _ = write!(output, "\nFull output saved to: {}", path.display());
            },
            Err(e) => tracing::debug!("Failed to spill BashSession output: {e}"),
        }
    }
    if truncated || read.output.dropped_bytes > 0 {
        events.push(CompensationEventTelemetry::new(
            CompensationKind::OutputTruncation,
            Some("BashSession".to_string()),
        ));
    }
    let elapsed = format_seconds_tenths(read.elapsed.as_millis());
    let footer = read.exit_code.map_or_else(
        || format!("[running | session: {id} | {elapsed}s elapsed]"),
        |code| format!("[exit:{code} | total {elapsed}s]"),
    );
    let gap = if read.output.dropped_bytes > 0 {
        format!(
            "\n[Oldest output was dropped: {} bytes since the last read.]",
            read.output.dropped_bytes
        )
    } else {
        String::new()
    };
    let termination = read
        .termination
        .map_or(String::new(), |why| format!("\n[Session {why}.]"));
    super::ToolResult {
        output: format!("{output}\n\n{footer}{gap}{termination}"),
        images: Vec::new(),
        compensation_events: events,
        permission_denials: Vec::new(),
    }
}

pub(super) fn cap_output(output: &str, max_bytes: Option<usize>) -> (String, bool) {
    let Some(max) = max_bytes else {
        return (output.to_string(), false);
    };
    if output.len() <= max {
        return (output.to_string(), false);
    }
    let end = output.floor_char_boundary(max);
    (
        format!(
            "{}\n[... output truncated at {max} bytes ...]",
            output.get(..end).unwrap_or("")
        ),
        true,
    )
}

fn format_list(registry: &SessionRegistry) -> String {
    let sessions = registry.list(Instant::now());
    if sessions.is_empty() {
        return "No Bash sessions in this Cake run.".to_string();
    }
    sessions
        .into_iter()
        .map(|session| {
            let command: String = session
                .command
                .chars()
                .take(COMMAND_PREVIEW_CHARS)
                .collect();
            let status = session
                .exit_code
                .map_or_else(|| "running".to_string(), |code| format!("exit:{code}"));
            format!(
                "{} | {} | {}s | {}",
                session.id,
                status,
                format_seconds_tenths(session.elapsed.as_millis()),
                command
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
