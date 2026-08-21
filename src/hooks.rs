use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::time::timeout;

use crate::config::SessionWriter;
use crate::config::hooks::{HookCommand, HookEvent, HookSource, LoadedHooks};
use crate::config::settings::DEFAULT_HOOK_OUTPUT_LIMIT;
use crate::types::{HookEventData, SessionRecord, StreamRecord};

#[derive(Clone)]
pub struct HookRunner {
    loaded: LoadedHooks,
    context: HookContext,
    /// Per-hook stdout/stderr byte cap from `[limits] hook_output_limit`;
    /// `None` means unlimited.
    output_limit: Option<usize>,
}

#[derive(Clone)]
pub struct HookContext {
    pub session_id: uuid::Uuid,
    pub task_id: uuid::Uuid,
    pub transcript_path: Option<PathBuf>,
    pub session_writer: Option<SessionWriter>,
    pub hook_event_sink: Option<Arc<dyn Fn(StreamRecord) + Send + Sync>>,
    pub cwd: PathBuf,
    pub model: String,
}

#[derive(Debug, Clone)]
pub enum ToolHookPlan {
    Execute {
        arguments: String,
        prefix_notice: Option<String>,
        additional_context: Vec<String>,
    },
    Block {
        reason: String,
        additional_context: Vec<String>,
    },
}

/// The decision communicated by a hook's stdout JSON.
///
/// Hooks return one of three outcomes:
/// - [`HookDecision::Continue`]: proceed normally (the default when no
///   stop/deny signal is present).
/// - [`HookDecision::Deny`]: block the action (permission denied). On
///   `PreToolUse` this produces a [`ToolHookPlan::Block`]; on other events it
///   terminates with an error.
/// - [`HookDecision::Stop`]: request the session to stop. On `PreToolUse` this
///   also produces a [`ToolHookPlan::Block`]; on other events it terminates
///   with an error.
///
/// This replaces the previous combination of optional `continue`,
/// `stop_reason`, `decision`, `permission`, and `reason` fields with a
/// single sum type so that every combination is explicit and documented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookDecision {
    Continue,
    Deny { reason: String },
    Stop { reason: String },
}

impl HookDecision {
    /// Human-readable label used in hook transcript records.
    pub const fn decision_label(&self) -> &'static str {
        match self {
            Self::Continue => "none",
            Self::Deny { .. } => "deny",
            Self::Stop { .. } => "stop",
        }
    }

    /// Derive a decision from the raw JSON fields a hook produces.
    ///
    /// This preserves backward compatibility with every output shape that the
    /// existing hook protocol supports.
    fn from_raw(
        r#continue: Option<bool>,
        stop_reason: Option<&str>,
        permission: Option<&str>,
        decision_field: Option<&str>,
        reason: Option<&str>,
    ) -> Self {
        if r#continue == Some(false) {
            let reason = stop_reason
                .or(reason)
                .unwrap_or("hook requested stop")
                .to_owned();
            return Self::Stop { reason };
        }

        let permission = permission.or(decision_field);
        if let Some("deny" | "block" | "ask") = permission {
            let reason = reason.map_or_else(
                || {
                    if permission == Some("ask") {
                        "interactive ask is not supported yet".to_owned()
                    } else {
                        "hook denied action".to_owned()
                    }
                },
                ToOwned::to_owned,
            );
            return Self::Deny { reason };
        }

        Self::Continue
    }
}

#[derive(Debug, Default)]
struct AggregatedHookResult {
    deny_reasons: Vec<String>,
    updated_inputs: Vec<(Value, PathBuf)>,
    additional_context: Vec<String>,
}

#[derive(Debug, Clone)]
struct ToolHookMetadata {
    call_id: String,
    tool_name: String,
    input_summary: String,
}

#[derive(Debug)]
struct InvocationOutcome {
    command: HookCommand,
    exit_code: Option<i32>,
    duration: Duration,
    stdout: String,
    stderr: String,
    parsed: Option<ParsedHookOutput>,
    error: Option<String>,
}

/// Parsed hook stdout, carrying a decision plus optional auxiliary fields.
#[derive(Debug)]
struct ParsedHookOutput {
    decision: HookDecision,
    explicit_allow: bool,
    updated_input: Option<Value>,
    additional_context: Option<String>,
}

/// Raw wire shape a hook script emits on stdout.  Private; callers work
/// with [`ParsedHookOutput`] (and therefore [`HookDecision`]) instead.
#[derive(Debug, Deserialize)]
struct RawHookOutput {
    #[serde(default)]
    r#continue: Option<bool>,
    stop_reason: Option<String>,
    decision: Option<String>,
    permission: Option<String>,
    reason: Option<String>,
    updated_input: Option<Value>,
    additional_context: Option<String>,
}

impl From<RawHookOutput> for ParsedHookOutput {
    fn from(raw: RawHookOutput) -> Self {
        let decision = HookDecision::from_raw(
            raw.r#continue,
            raw.stop_reason.as_deref(),
            raw.permission.as_deref(),
            raw.decision.as_deref(),
            raw.reason.as_deref(),
        );
        let explicit_allow = decision == HookDecision::Continue
            && (raw.r#continue == Some(true)
                || matches!(raw.permission.as_deref(), Some("allow"))
                || (raw.permission.is_none() && matches!(raw.decision.as_deref(), Some("allow"))));
        Self {
            decision,
            explicit_allow,
            updated_input: raw.updated_input,
            additional_context: raw.additional_context,
        }
    }
}

#[derive(Debug, Serialize)]
struct HookRecord<'a> {
    version: u8,
    session_id: String,
    task_id: String,
    transcript_path: Option<&'a PathBuf>,
    cwd: &'a PathBuf,
    hook_event_name: &'static str,
    model: &'a str,
    timestamp: String,
}

impl ToolHookMetadata {
    fn new(call_id: &str, tool_name: &str, repairs_arguments: bool, arguments: &str) -> Self {
        Self {
            call_id: call_id.to_owned(),
            tool_name: tool_name.to_owned(),
            input_summary: tool_input_summary(repairs_arguments, arguments),
        }
    }
}

impl HookRunner {
    pub const fn new(loaded: LoadedHooks, context: HookContext) -> Self {
        Self {
            loaded,
            context,
            output_limit: Some(DEFAULT_HOOK_OUTPUT_LIMIT as usize),
        }
    }

    /// Override the per-hook stdout/stderr byte cap from `[limits]
    /// hook_output_limit`. `None` disables truncation.
    pub const fn with_output_limit(mut self, output_limit: Option<usize>) -> Self {
        self.output_limit = output_limit;
        self
    }

    pub async fn session_start(
        &self,
        source: &HookSource,
        initial_prompt: &str,
    ) -> anyhow::Result<Vec<String>> {
        let payload = self.payload(
            HookEvent::SessionStart,
            json!({
                "source": source.as_display_str(),
                "initial_prompt": initial_prompt,
            }),
        );
        let result = self
            .run_and_aggregate(HookEvent::SessionStart, source, payload, None)
            .await?;
        Ok(result.additional_context)
    }

    pub async fn user_prompt_submit(&self, prompt: &str) -> anyhow::Result<Vec<String>> {
        let payload = self.payload(
            HookEvent::UserPromptSubmit,
            json!({
                "prompt": prompt,
            }),
        );
        let result = self
            .run_and_aggregate(
                HookEvent::UserPromptSubmit,
                &HookSource::None,
                payload,
                None,
            )
            .await?;
        Ok(result.additional_context)
    }

    pub async fn pre_tool_use(
        &self,
        tool_name: &str,
        tool_use_id: &str,
        arguments: &str,
        repairs_arguments: bool,
    ) -> anyhow::Result<ToolHookPlan> {
        let source = HookSource::Tool(tool_name.to_owned());
        let tool_input = parse_tool_input(repairs_arguments, arguments);
        let metadata = ToolHookMetadata::new(tool_use_id, tool_name, repairs_arguments, arguments);
        let payload = self.payload(
            HookEvent::PreToolUse,
            json!({
                "tool_name": tool_name,
                "tool_use_id": tool_use_id,
                "tool_input": tool_input,
                "tool_input_json": arguments,
            }),
        );
        let result = self
            .run_and_aggregate(HookEvent::PreToolUse, &source, payload, Some(metadata))
            .await?;

        if !result.deny_reasons.is_empty() {
            return Ok(ToolHookPlan::Block {
                reason: result.deny_reasons.join("; "),
                additional_context: result.additional_context,
            });
        }

        if let Some((updated, source_file)) = result.updated_inputs.first() {
            if result.updated_inputs.len() > 1 {
                tracing::warn!(
                    target: "cake::hooks",
                    event = "PreToolUse",
                    source = tool_name,
                    first_source_file = %source_file.display(),
                    "Multiple hooks returned updated_input; using first in load order"
                );
            }
            if !updated.is_object() {
                return Ok(ToolHookPlan::Block {
                    reason: format!(
                        "Hook updated_input from {} must be a JSON object",
                        source_file.display()
                    ),
                    additional_context: result.additional_context,
                });
            }
            let new_arguments = serde_json::to_string(updated)
                .context("failed to serialize hook updated_input as JSON")?;
            let notice = format!(
                "Hook updated tool input.\nOriginal arguments: {arguments}\nNew arguments: {new_arguments}\n---\n"
            );
            return Ok(ToolHookPlan::Execute {
                arguments: new_arguments,
                prefix_notice: Some(notice),
                additional_context: result.additional_context,
            });
        }

        Ok(ToolHookPlan::Execute {
            arguments: arguments.to_string(),
            prefix_notice: None,
            additional_context: result.additional_context,
        })
    }

    pub async fn post_tool_use(
        &self,
        tool_name: &str,
        tool_use_id: &str,
        arguments: &str,
        result: &Result<String, String>,
        repairs_arguments: bool,
    ) -> anyhow::Result<Option<String>> {
        let (event, result_type, text_result_for_llm) = match result {
            Ok(output) => (HookEvent::PostToolUse, "success", output.as_str()),
            Err(error) => (HookEvent::PostToolUseFailure, "failure", error.as_str()),
        };
        let source = HookSource::Tool(tool_name.to_owned());
        let metadata = ToolHookMetadata::new(tool_use_id, tool_name, repairs_arguments, arguments);
        let payload = self.payload(
            event,
            json!({
                "tool_name": tool_name,
                "tool_use_id": tool_use_id,
                "tool_input": parse_tool_input(repairs_arguments, arguments),
                "tool_input_json": arguments,
                "tool_result": {
                    "result_type": result_type,
                    "text_result_for_llm": text_result_for_llm,
                }
            }),
        );
        let result = self
            .run_and_aggregate(event, &source, payload, Some(metadata))
            .await?;
        Ok(join_context(&result.additional_context))
    }

    pub async fn stop(&self, result: &str) -> anyhow::Result<Option<String>> {
        let payload = self.payload(HookEvent::Stop, json!({ "result": result }));
        let result = self
            .run_and_aggregate(HookEvent::Stop, &HookSource::None, payload, None)
            .await?;
        Ok(join_context(&result.additional_context))
    }

    pub async fn error_occurred(&self, message: &str) -> anyhow::Result<()> {
        let payload = self.payload(
            HookEvent::ErrorOccurred,
            json!({
                "error": {
                    "message": message,
                    "name": "Error",
                }
            }),
        );
        self.run_and_aggregate(HookEvent::ErrorOccurred, &HookSource::None, payload, None)
            .await?;
        Ok(())
    }

    async fn run_and_aggregate(
        &self,
        event: HookEvent,
        source: &HookSource,
        payload: Value,
        tool_metadata: Option<ToolHookMetadata>,
    ) -> anyhow::Result<AggregatedHookResult> {
        let matched = self.loaded.matching_groups(event, source);
        if matched.is_empty() {
            return Ok(AggregatedHookResult::default());
        }

        let mut commands = Vec::new();
        for group in matched {
            for hook in &group.hooks {
                commands.push(hook.clone());
            }
        }

        let futures = commands.into_iter().map(|command| {
            let payload = payload.clone();
            let cwd = self.context.cwd.clone();
            let output_limit = self.output_limit;
            async move { run_command_hook(command, payload, cwd, output_limit).await }
        });
        let outcomes = futures::future::join_all(futures).await;

        let mut aggregated = AggregatedHookResult::default();
        for outcome in outcomes {
            self.record_outcome(event, source, tool_metadata.as_ref(), &outcome);

            if let Some(error) = &outcome.error {
                if outcome.command.fail_closed {
                    if event == HookEvent::PreToolUse {
                        aggregated.deny_reasons.push(format!(
                            "{}: {error}",
                            outcome.command.source_path.display()
                        ));
                        continue;
                    }
                    anyhow::bail!(
                        "Hook failed closed for {event} in {}: {error}",
                        outcome.command.source_path.display()
                    );
                }
                tracing::warn!(
                    target: "cake::hooks",
                    event = event.as_str(),
                    source = source.as_display_str(),
                    command = %outcome.command.command,
                    source_file = %outcome.command.source_path.display(),
                    error = %error,
                    "Hook failed open"
                );
                continue;
            }

            let Some(parsed) = &outcome.parsed else {
                continue;
            };

            match &parsed.decision {
                HookDecision::Continue => {},
                HookDecision::Deny { reason } | HookDecision::Stop { reason } => {
                    let label = parsed.decision.decision_label();
                    if event == HookEvent::PreToolUse {
                        aggregated.deny_reasons.push(format!(
                            "{}: {reason}",
                            outcome.command.source_path.display()
                        ));
                    } else {
                        anyhow::bail!("Hook {label} {event}: {reason}");
                    }
                },
            }

            if let Some(context) = parsed.additional_context.as_ref()
                && !context.is_empty()
            {
                aggregated.additional_context.push(context.clone());
            }

            if event == HookEvent::PreToolUse
                && let Some(updated_input) = parsed.updated_input.clone()
            {
                aggregated
                    .updated_inputs
                    .push((updated_input, outcome.command.source_path.clone()));
            }
        }

        Ok(aggregated)
    }

    fn payload(&self, event: HookEvent, extra: Value) -> Value {
        let common = HookRecord {
            version: 1,
            session_id: self.context.session_id.to_string(),
            task_id: self.context.task_id.to_string(),
            transcript_path: self.context.transcript_path.as_ref(),
            cwd: &self.context.cwd,
            hook_event_name: event.as_str(),
            model: &self.context.model,
            timestamp: chrono::Utc::now().to_rfc3339(),
        };

        let mut value = match serde_json::to_value(common) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(target: "cake::hooks", error = %error, "Failed to serialize common hook payload");
                json!({})
            },
        };
        if let (Value::Object(base), Value::Object(extra)) = (&mut value, extra) {
            base.extend(extra);
        }
        value
    }

    fn record_outcome(
        &self,
        event: HookEvent,
        source: &HookSource,
        tool_metadata: Option<&ToolHookMetadata>,
        outcome: &InvocationOutcome,
    ) {
        let stderr_bytes = outcome.stderr.len();
        let stdout_bytes = outcome.stdout.len();
        let level_error = outcome.command.fail_closed && outcome.error.is_some();
        let source_str = source.as_display_str();
        let decision = outcome_decision_label(outcome.parsed.as_ref(), outcome.error.as_deref());
        let resolved_decision =
            resolved_decision_label(outcome.parsed.as_ref(), outcome.error.as_deref());
        if level_error {
            tracing::error!(
                target: "cake::hooks",
                event = event.as_str(),
                source = source_str,
                command = %outcome.command.command,
                source_file = %outcome.command.source_path.display(),
                exit_code = ?outcome.exit_code,
                duration_ms = duration_ms(outcome.duration),
                stderr_bytes,
                stdout_bytes,
                decision,
                resolved_decision,
                fail_closed = outcome.command.fail_closed,
                "Hook invocation failed closed"
            );
        } else if outcome.error.is_some() {
            tracing::warn!(
                target: "cake::hooks",
                event = event.as_str(),
                source = source_str,
                command = %outcome.command.command,
                source_file = %outcome.command.source_path.display(),
                exit_code = ?outcome.exit_code,
                duration_ms = duration_ms(outcome.duration),
                stderr_bytes,
                stdout_bytes,
                decision,
                resolved_decision,
                fail_closed = outcome.command.fail_closed,
                "Hook invocation completed with non-blocking error"
            );
        } else {
            tracing::info!(
                target: "cake::hooks",
                event = event.as_str(),
                source = source_str,
                command = %outcome.command.command,
                source_file = %outcome.command.source_path.display(),
                exit_code = ?outcome.exit_code,
                duration_ms = duration_ms(outcome.duration),
                stderr_bytes,
                stdout_bytes,
                decision,
                resolved_decision,
                fail_closed = outcome.command.fail_closed,
                "Hook invocation completed"
            );
        }

        let record = HookEventData {
            timestamp: chrono::Utc::now(),
            task_id: self.context.task_id.to_string(),
            event: event.as_str().to_string(),
            source: source.as_display_str().map(ToOwned::to_owned),
            call_id: tool_metadata.map(|metadata| metadata.call_id.clone()),
            tool_name: tool_metadata.map(|metadata| metadata.tool_name.clone()),
            tool_input_summary: tool_metadata.map(|metadata| metadata.input_summary.clone()),
            source_file: outcome.command.source_path.clone(),
            command: outcome.command.command.clone(),
            exit_code: outcome.exit_code,
            duration_ms: outcome.duration.as_millis().try_into().unwrap_or(u64::MAX),
            decision: decision.to_owned(),
            resolved_decision: Some(resolved_decision.to_owned()),
            fail_closed: outcome.command.fail_closed,
            stdout: outcome.stdout.clone(),
            stderr: outcome.stderr.clone(),
        };

        if let Some(sink) = &self.context.hook_event_sink {
            sink(StreamRecord::HookEvent(record));
            return;
        }

        let Some(writer) = &self.context.session_writer else {
            return;
        };
        let record = SessionRecord::HookEvent(record);
        if let Err(error) = writer.append_record(&record) {
            tracing::warn!(
                target: "cake::hooks",
                error = %error,
                "Failed to append hook transcript record"
            );
        }
    }
}

/// RAII guard that kills the entire hook process tree on drop, unless
/// [`defuse`](#method.defuse) has been called.
///
/// On Unix the guard sends [`libc::SIGKILL`] to the hook's process group (if a PID
/// was recorded).  On other platforms it is a no-op — the underlying
/// `kill_on_drop(true)` on the `Command` still terminates the immediate child
/// on drop.
#[cfg(unix)]
struct HookProcessGuard {
    pid: Option<u32>,
}

#[cfg(unix)]
impl HookProcessGuard {
    const fn new(pid: Option<u32>) -> Self {
        Self { pid }
    }

    /// Prevent the guard from killing the process group on drop.
    const fn defuse(&mut self) {
        self.pid = None;
    }
}

#[cfg(unix)]
impl Drop for HookProcessGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            #[expect(
                unsafe_code,
                reason = "libc::kill requires unsafe; see SAFETY comment below"
            )]
            // SAFETY: pid was obtained from `Child::id()` at a point when the
            // child was still alive, and the child's process group ID equals
            // its PID because `shell_command` calls `process_group(0)` on
            // Unix.  A negative PID in `kill(2)` targets every process in the
            // group |pid|, which is exactly what we need to reach any
            // descendent pipelines, background jobs, etc.  The cast from u32
            // to i32 is safe because PIDs fit in i32 on all supported Unix
            // targets.
            unsafe {
                ::libc::kill(-(pid as ::libc::pid_t), ::libc::SIGKILL);
            }
        }
    }
}

/// No-op guard for non-Unix platforms.
#[cfg(not(unix))]
struct HookProcessGuard;

#[cfg(not(unix))]
impl HookProcessGuard {
    fn new(_pid: Option<u32>) -> Self {
        Self
    }

    fn defuse(&mut self) {}
}

#[expect(
    clippy::too_many_lines,
    reason = "hook command execution has many branches for error handling"
)]
async fn run_command_hook(
    command: HookCommand,
    payload: Value,
    cwd: PathBuf,
    output_limit: Option<usize>,
) -> InvocationOutcome {
    let start = Instant::now();
    if let Some(status) = &command.status_message {
        tracing::info!(
            target: "cake::hooks",
            command = %command.command,
            source_file = %command.source_path.display(),
            status_message = %status,
            "Starting hook"
        );
    }

    let mut process = shell_command(&command.command);
    process
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            return InvocationOutcome {
                command,
                exit_code: None,
                duration: start.elapsed(),
                stdout: String::new(),
                stderr: String::new(),
                parsed: None,
                error: Some(format!("failed to spawn hook command: {error}")),
            };
        },
    };

    // Record the child PID and install a drop guard that kills the entire
    // hook process tree on any non-successful exit (timeout, cancellation,
    // early error return).  The guard is defused on the success path so
    // normally completed hooks are not affected.
    let child_id = child.id();
    #[expect(
        clippy::used_underscore_binding,
        reason = "_guard prefix suppresses unused-variable warning on timeout path"
    )]
    let mut _guard = HookProcessGuard::new(child_id);

    if let Some(mut stdin) = child.stdin.take() {
        let input = serde_json::to_vec(&payload).unwrap_or_default();
        // Feed stdin from a detached task so a hook that never reads stdin
        // cannot block the call. Write errors are ignored because exit status
        // is authoritative and the enclosing timeout bounds the writer.
        tokio::spawn(async move {
            _ = stdin.write_all(&input).await;
            _ = stdin.shutdown().await;
        });
    }

    // The timeout bounds the whole read and wait, like the previous
    // `wait_with_output` call. `wait_for_hook_output` reads both streams with
    // bounded buffers, so a hook that emits more than `output_limit` cannot
    // exhaust memory: each stream buffers at most `output_limit` bytes and
    // only counts the overflow.
    let timeout_result = timeout(
        command.timeout,
        wait_for_hook_output(&mut child, output_limit),
    )
    .await;
    let (stdout_captured, stdout_total, stderr_captured, stderr_total, status) =
        match timeout_result {
            Ok(Ok(output)) => {
                // Hook completed normally — defuse the guard so it does not
                // kill background descendants on drop.
                _guard.defuse();
                output
            },
            Ok(Err(error)) => {
                // The child has already exited (wait succeeded), so there is
                // nothing left in the process group; defuse to avoid a harmless
                // ESRCH.
                _guard.defuse();
                return InvocationOutcome {
                    command,
                    exit_code: None,
                    duration: start.elapsed(),
                    stdout: String::new(),
                    stderr: String::new(),
                    parsed: None,
                    error: Some(format!("failed to wait for hook command: {error}")),
                };
            },
            Err(_) => {
                let timeout_secs = command.timeout.as_secs();
                // Guard fires on return, killing the hook process tree:
                // kill_on_drop(true) will already have sent SIGKILL to the
                // immediate child (the shell) when the read future was dropped;
                // the guard sends SIGKILL to the process group to reach any
                // descendants.
                return InvocationOutcome {
                    command,
                    exit_code: None,
                    duration: start.elapsed(),
                    stdout: String::new(),
                    stderr: String::new(),
                    parsed: None,
                    error: Some(format!("hook command timed out after {timeout_secs}s")),
                };
            },
        };

    let stdout = capped_text(&stdout_captured, stdout_total, output_limit);
    let stderr = capped_text(&stderr_captured, stderr_total, output_limit);
    let exit_code = status.code();

    if exit_code == Some(2) {
        let reason = if stderr.trim().is_empty() {
            parse_hook_output(&stdout)
                .ok()
                .flatten()
                .and_then(|parsed| parsed.reason)
                .unwrap_or_else(|| "hook blocked action".to_string())
        } else {
            stderr.trim().to_string()
        };
        return InvocationOutcome {
            command,
            exit_code,
            duration: start.elapsed(),
            stdout,
            stderr,
            parsed: Some(ParsedHookOutput {
                decision: HookDecision::Stop { reason },
                explicit_allow: false,
                updated_input: None,
                additional_context: None,
            }),
            error: None,
        };
    }

    if exit_code != Some(0) {
        return InvocationOutcome {
            command,
            exit_code,
            duration: start.elapsed(),
            stdout,
            stderr: stderr.clone(),
            parsed: None,
            error: Some(format!(
                "hook exited with code {}{}",
                exit_code.map_or_else(|| "unknown".to_string(), |code| code.to_string()),
                if stderr.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", stderr.trim())
                }
            )),
        };
    }

    match parse_hook_output(&stdout) {
        Ok(Some(raw)) => {
            let parsed: ParsedHookOutput = raw.into();
            InvocationOutcome {
                command,
                exit_code,
                duration: start.elapsed(),
                stdout,
                stderr,
                parsed: Some(parsed),
                error: None,
            }
        },
        Ok(None) => InvocationOutcome {
            command,
            exit_code,
            duration: start.elapsed(),
            stdout,
            stderr,
            parsed: None,
            error: None,
        },
        Err(error) => InvocationOutcome {
            command,
            exit_code,
            duration: start.elapsed(),
            stdout,
            stderr,
            parsed: None,
            error: Some(error),
        },
    }
}

fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(command);
        cmd.kill_on_drop(true);
        cmd
    }

    #[cfg(not(windows))]
    {
        use std::os::unix::process::CommandExt;

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd.kill_on_drop(true);
        // Place the entire hook process tree in its own process group so
        // the timeout path can kill all descendants, not just the shell.
        cmd.as_std_mut().process_group(0);
        cmd
    }
}

fn parse_hook_output(stdout: &str) -> Result<Option<RawHookOutput>, String> {
    if stdout.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str(stdout)
        .map(Some)
        .map_err(|error| format!("hook stdout was not valid JSON: {error}"))
}

const fn outcome_decision_label(
    parsed: Option<&ParsedHookOutput>,
    error: Option<&str>,
) -> &'static str {
    match (parsed, error) {
        (Some(parsed), _) => parsed.decision.decision_label(),
        (None, Some(_)) => "error",
        (None, None) => "none",
    }
}

const fn resolved_decision_label(
    parsed: Option<&ParsedHookOutput>,
    error: Option<&str>,
) -> &'static str {
    match (parsed, error) {
        (Some(parsed), _) if parsed.explicit_allow => "allow",
        (Some(parsed), _) => parsed.decision.decision_label(),
        (None, Some(_)) => "error",
        (None, None) => "none",
    }
}

/// Parse tool-call arguments into the structured `tool_input` a hook sees.
///
/// Applies the same conservative JSON repair pass the tool executor applies
/// before parsing when the caller reports that the tool's registry entry
/// declares `repairs_arguments`, so hook policy and execution agree on what
/// will run. For other tools the strict parse is the executor's own view.
/// When parsing still fails, `tool_input` falls back to `{}`; such a payload
/// cannot parse in the executor either, so the call fails rather than
/// executes. See `#185` and `#277`.
fn parse_tool_input(repairs_arguments: bool, arguments: &str) -> Value {
    serde_json::from_str(&tool_input_payload(repairs_arguments, arguments))
        .unwrap_or_else(|_| json!({}))
}

/// The argument text a hook should parse.
///
/// Tools whose registered executors repair get the repaired payload, so hook
/// policy sees what will actually run; tools whose executors parse strictly
/// (Bash, Read) get the raw payload, matching their view. The flag comes from
/// the tool's registry entry, not from its name (#277).
fn tool_input_payload(repairs_arguments: bool, arguments: &str) -> String {
    if repairs_arguments {
        crate::clients::repair_json_args(arguments)
    } else {
        arguments.to_string()
    }
}

fn tool_input_summary(repairs_arguments: bool, arguments: &str) -> String {
    let value = parse_tool_input(repairs_arguments, arguments);
    let summary = value
        .as_object()
        .and_then(|object| {
            ["command", "path", "file_path", "old_text"]
                .into_iter()
                .find_map(|key| object.get(key).and_then(Value::as_str))
        })
        .map_or_else(|| compact_json(arguments), ToOwned::to_owned);
    truncate_chars(&summary, 240)
}

fn compact_json(arguments: &str) -> String {
    serde_json::from_str::<Value>(arguments)
        .ok()
        .and_then(|value| serde_json::to_string(&value).ok())
        .unwrap_or_else(|| arguments.to_owned())
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let keep = max_chars.saturating_sub(1);
    format!("{}...", value.chars().take(keep).collect::<String>())
}

fn join_context(context: &[String]) -> Option<String> {
    if context.is_empty() {
        None
    } else {
        Some(context.join("\n\n"))
    }
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

/// Read both hook streams with bounded buffers while the hook runs, then wait
/// for it to exit. Each stream buffers at most `output_limit` bytes and only
/// counts the overflow, so a noisy hook cannot exhaust memory while the
/// reported `(truncated, N more bytes)` total stays accurate. Returns the
/// captured prefix and total bytes for each stream, plus the exit status.
async fn wait_for_hook_output(
    child: &mut tokio::process::Child,
    output_limit: Option<usize>,
) -> std::io::Result<(Vec<u8>, usize, Vec<u8>, usize, std::process::ExitStatus)> {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (stdout_result, stderr_result) = tokio::join!(
        read_capped(stdout, output_limit),
        read_capped(stderr, output_limit),
    );
    let (stdout_captured, stdout_total) = stdout_result?;
    let (stderr_captured, stderr_total) = stderr_result?;
    let status = child.wait().await?;
    Ok((
        stdout_captured,
        stdout_total,
        stderr_captured,
        stderr_total,
        status,
    ))
}

/// Read `stream` to EOF, buffering at most `output_limit` bytes and counting
/// the overflow, so a noisy hook cannot exhaust memory while the reported
/// `(truncated, N more bytes)` total stays accurate. Returns the captured
/// prefix and the total bytes read.
async fn read_capped<R>(
    stream: Option<R>,
    output_limit: Option<usize>,
) -> std::io::Result<(Vec<u8>, usize)>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let Some(mut stream) = stream else {
        return Ok((Vec::new(), 0));
    };
    let mut captured = Vec::new();
    let mut total = 0usize;
    // Heap-allocated so the future stays small (clippy::large_futures).
    let mut tmp = vec![0u8; 8192];
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        total += n;
        let keep = match output_limit {
            Some(cap) if captured.len() < cap => n.min(cap - captured.len()),
            Some(_) => 0,
            None => n,
        };
        captured.extend_from_slice(&tmp[..keep]);
    }
    Ok((captured, total))
}

fn capped_text(captured: &[u8], total: usize, output_limit: Option<usize>) -> String {
    let Some(output_limit) = output_limit else {
        return String::from_utf8_lossy(captured).to_string();
    };
    if total <= output_limit {
        return String::from_utf8_lossy(captured).to_string();
    }
    let omitted = total - output_limit;
    let keep = output_limit.min(captured.len());
    format!(
        "{}... (truncated, {omitted} more bytes)",
        String::from_utf8_lossy(&captured[..keep])
    )
}

#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;
