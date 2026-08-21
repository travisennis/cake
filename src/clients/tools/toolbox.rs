//! Execute protocol for user-defined toolbox tools.
//!
//! Discovery and describe-protocol parsing live in `config::toolbox`; this
//! module turns a parsed [`ToolboxTool`] into a [`ToolEntry`] whose executor
//! spawns the tool's executable with `TOOLBOX_ACTION=execute`, writes the
//! call arguments to its stdin, and returns its stdout as the tool result.
//!
//! Toolbox tools run as separate processes without cake's OS sandbox: they
//! are user-provided and trusted. They are never registered under the
//! read-only sandbox policy (see `ToolRegistry::retain_read_safe_tools`).

use std::process::Stdio;
use std::sync::Arc;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::{Duration, timeout};
use tracing::warn;

use crate::clients::tools::{Tool, ToolEntry, ToolError, ToolResult, repair_json_args};
use crate::config::toolbox::{
    BoundedStreams, ToolboxFormat, ToolboxProcessGuard, ToolboxTool, place_in_own_process_group,
    read_streams_bounded, validate_text_argument_name,
};

/// Cap on output returned to the model, matching the Bash tool's inline
/// limit. Reading stops and the tool is killed once stdout exceeds this;
/// the captured output is truncated with a marker.
const MAX_OUTPUT_BYTES: usize = 50_000;

/// Cap on captured stderr; beyond this stderr is drained but discarded.
const MAX_STDERR_BYTES: usize = 10_000;

/// Cap on stderr echoed into an error message.
const MAX_STDERR_IN_ERROR: usize = 2_000;

/// Build a registry entry for a toolbox tool.
///
/// The executor captures the tool definition and the session id so each
/// invocation can expose `CAKE_THREAD_ID`/`AGENT_THREAD_ID` to the tool
/// process.
pub fn toolbox_tool_entry(tool: ToolboxTool, session_id: uuid::Uuid) -> ToolEntry {
    let definition = Tool {
        type_: "function".to_string(),
        name: tool.registered_name.clone(),
        description: tool.description.clone(),
        parameters: tool.parameters.clone(),
    };
    let tool = Arc::new(tool);
    let session_id = session_id.to_string();
    // Toolbox executors repair arguments before parsing (#185) but are never
    // read-safe: they run as unsandboxed external processes and may mutate
    // host state, so they are dropped under the read-only sandbox policy.
    ToolEntry::new(definition, move |context, _call_id, arguments| {
        let tool = Arc::clone(&tool);
        let session_id = session_id.clone();
        Box::pin(
            async move { execute_toolbox_tool(&tool, &session_id, &context.cwd, &arguments).await },
        )
    })
    .repairs_arguments()
}

/// Run a toolbox tool's `execute` action and capture its output.
async fn execute_toolbox_tool(
    tool: &ToolboxTool,
    session_id: &str,
    cwd: &std::path::Path,
    arguments: &str,
) -> Result<ToolResult, ToolError> {
    let stdin_payload = prepare_stdin_payload(tool.format, &tool.registered_name, arguments)?;

    let mut command = Command::new(&tool.path);
    command
        .env("TOOLBOX_ACTION", "execute")
        .env("AGENT", "cake")
        .env("CAKE_THREAD_ID", session_id)
        .env("AGENT_THREAD_ID", session_id)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    place_in_own_process_group(&mut command);

    let mut child = command.spawn().map_err(|e| {
        format!(
            "Failed to run toolbox tool '{}' ({}): {e}",
            tool.registered_name,
            tool.path.display()
        )
    })?;

    // Kill the whole process group on timeout, output cap, cancellation, or
    // any early error so descendants cannot outlive cake's reported result.
    let mut guard = ToolboxProcessGuard::new(child.id());

    feed_toolbox_stdin(&mut child, stdin_payload);
    let (streams, status) = capture_toolbox_process(
        &mut child,
        &tool.registered_name,
        Duration::from_secs(tool.timeout_secs),
    )
    .await?;

    // A normally completed tool no longer needs the drop guard. Keep it
    // armed after an output-cap kill so it also terminates descendants that
    // survived the direct child's kill and reap.
    guard.defuse_if_completed(status.is_some());

    finish_toolbox_result(&tool.registered_name, &streams, status)
}

/// Feed arguments from a detached task so a tool that never reads stdin
/// cannot block the call. Write errors are ignored because exit status is
/// authoritative and the enclosing process timeout bounds the writer.
fn feed_toolbox_stdin(child: &mut tokio::process::Child, stdin_payload: String) {
    if let Some(mut stdin) = child.stdin.take() {
        tokio::spawn(async move {
            _ = stdin.write_all(stdin_payload.as_bytes()).await;
            _ = stdin.shutdown().await;
        });
    }
}

/// Read both streams and reap the process under one timeout envelope.
async fn capture_toolbox_process(
    child: &mut tokio::process::Child,
    tool_name: &str,
    timeout_duration: Duration,
) -> Result<(BoundedStreams, Option<std::process::ExitStatus>), String> {
    let mut stdout = child.stdout.take().ok_or("Failed to capture stdout")?;
    let mut stderr = child.stderr.take().ok_or("Failed to capture stderr")?;
    timeout(timeout_duration, async {
        let streams =
            read_streams_bounded(&mut stdout, &mut stderr, MAX_OUTPUT_BYTES, MAX_STDERR_BYTES)
                .await?;
        let status = if streams.stdout_capped {
            _ = child.kill().await;
            _ = child.wait().await;
            None
        } else {
            Some(
                child
                    .wait()
                    .await
                    .map_err(|e| format!("Failed to wait for toolbox tool: {e}"))?,
            )
        };
        Ok::<_, String>((streams, status))
    })
    .await
    .map_err(|_elapsed| {
        format!(
            "Toolbox tool '{tool_name}' timed out after {} seconds",
            timeout_duration.as_secs()
        )
    })?
}

fn finish_toolbox_result(
    tool_name: &str,
    streams: &BoundedStreams,
    status: Option<std::process::ExitStatus>,
) -> Result<ToolResult, ToolError> {
    let stderr = String::from_utf8_lossy(&streams.stderr);
    if !stderr.trim().is_empty() {
        // Tool diagnostics go to cake's log file, not the model (the
        // describe/execute protocol reserves stdout for tool output).
        warn!(
            target: "cake",
            "toolbox tool '{}' stderr: {}",
            tool_name,
            stderr.trim(),
        );
    }

    if let Some(status) = status
        && !status.success()
    {
        let detail = if stderr.trim().is_empty() {
            String::from_utf8_lossy(&streams.stdout)
        } else {
            stderr
        };
        let detail: String = detail.trim().chars().take(MAX_STDERR_IN_ERROR).collect();
        return Err(ToolError::new(format!(
            "Toolbox tool '{tool_name}' exited with {status}: {detail}"
        )));
    }

    // A capped tool was killed after partial output; that is an output
    // truncation compensation even though the result is an error.
    let mut compensation_events = Vec::new();
    crate::clients::tools::push_output_truncation_event_if(
        &mut compensation_events,
        tool_name,
        streams.stdout_capped,
    );

    Ok(ToolResult {
        output: truncate_output(String::from_utf8_lossy(&streams.stdout).into_owned()),
        compensation_events,
    })
}

/// Parse and serialize arguments according to the tool's detected protocol.
fn prepare_stdin_payload(
    format: ToolboxFormat,
    registered_name: &str,
    arguments: &str,
) -> Result<String, String> {
    let args = parse_arguments(registered_name, arguments)?;
    match format {
        ToolboxFormat::Json => Ok(args.to_string()),
        ToolboxFormat::Text => arguments_to_key_value_lines(&args),
    }
}

/// Parse tool-call arguments into a JSON object, applying the same
/// conservative JSON repair pass as the built-in tools. An empty argument
/// string (some models send none for parameterless tools) parses as `{}`.
fn parse_arguments(registered_name: &str, arguments: &str) -> Result<serde_json::Value, String> {
    if arguments.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    let value: serde_json::Value = serde_json::from_str(arguments)
        .or_else(|_| serde_json::from_str(&repair_json_args(arguments)))
        .map_err(|e| format!("Invalid {registered_name} arguments: {e}"))?;
    if !value.is_object() {
        return Err(format!(
            "Invalid {registered_name} arguments: expected a JSON object"
        ));
    }
    Ok(value)
}

/// Serialize a JSON argument object as `key=value` lines for text-format
/// tools. String values are written verbatim unless they contain a line
/// break, which the protocol cannot represent without creating another
/// argument record. Other values use compact JSON, which escapes line
/// breaks within nested strings.
fn arguments_to_key_value_lines(args: &serde_json::Value) -> Result<String, String> {
    let Some(map) = args.as_object() else {
        return Ok(String::new());
    };
    let mut lines = String::new();
    for (key, value) in map {
        let rendered = render_text_argument(key, value)?;
        lines.push_str(key);
        lines.push('=');
        lines.push_str(&rendered);
        lines.push('\n');
    }
    Ok(lines)
}

/// Validate and render one text-protocol argument without allowing it to
/// create additional `key=value` records.
fn render_text_argument(key: &str, value: &serde_json::Value) -> Result<String, String> {
    validate_text_argument_name(key)?;
    match value {
        serde_json::Value::String(s) if s.contains(['\r', '\n']) => Err(format!(
            "Invalid text toolbox argument '{key}': multiline string values are not supported"
        )),
        serde_json::Value::String(s) => Ok(s.clone()),
        other => Ok(other.to_string()),
    }
}

/// Truncate oversized output at a char boundary with an explicit marker.
fn truncate_output(mut output: String) -> String {
    if output.len() <= MAX_OUTPUT_BYTES {
        return output;
    }
    let mut cut = MAX_OUTPUT_BYTES;
    while !output.is_char_boundary(cut) {
        cut -= 1;
    }
    output.truncate(cut);
    output.push_str("\n[... toolbox output truncated at ");
    output.push_str(&MAX_OUTPUT_BYTES.to_string());
    output.push_str(" bytes ...]");
    output
}

#[cfg(test)]
#[path = "toolbox_tests.rs"]
mod toolbox_tests;
