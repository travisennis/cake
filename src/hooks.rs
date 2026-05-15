use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::clients::SessionRecord;
use crate::config::SessionWriter;
use crate::config::hooks::{HookCommand, HookEvent, HookSource, LoadedHooks};

const HOOK_OUTPUT_LIMIT: usize = 64 * 1024;

#[derive(Clone)]
pub struct HookRunner {
    loaded: LoadedHooks,
    context: HookContext,
}

#[derive(Clone)]
pub struct HookContext {
    pub session_id: uuid::Uuid,
    pub task_id: uuid::Uuid,
    pub transcript_path: Option<PathBuf>,
    pub session_writer: Option<SessionWriter>,
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
        Self {
            decision,
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

impl HookRunner {
    pub const fn new(loaded: LoadedHooks, context: HookContext) -> Self {
        Self { loaded, context }
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
            .run_and_aggregate(HookEvent::SessionStart, source, payload)
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
            .run_and_aggregate(HookEvent::UserPromptSubmit, &HookSource::None, payload)
            .await?;
        Ok(result.additional_context)
    }

    pub async fn pre_tool_use(
        &self,
        tool_name: &str,
        tool_use_id: &str,
        arguments: &str,
    ) -> anyhow::Result<ToolHookPlan> {
        let source = HookSource::Tool(tool_name.to_owned());
        let tool_input = parse_tool_input(arguments);
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
            .run_and_aggregate(HookEvent::PreToolUse, &source, payload)
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
    ) -> anyhow::Result<Option<String>> {
        let (event, result_type, text_result_for_llm) = match result {
            Ok(output) => (HookEvent::PostToolUse, "success", output.as_str()),
            Err(error) => (HookEvent::PostToolUseFailure, "failure", error.as_str()),
        };
        let source = HookSource::Tool(tool_name.to_owned());
        let payload = self.payload(
            event,
            json!({
                "tool_name": tool_name,
                "tool_use_id": tool_use_id,
                "tool_input": parse_tool_input(arguments),
                "tool_input_json": arguments,
                "tool_result": {
                    "result_type": result_type,
                    "text_result_for_llm": text_result_for_llm,
                }
            }),
        );
        let result = self.run_and_aggregate(event, &source, payload).await?;
        Ok(join_context(&result.additional_context))
    }

    pub async fn stop(&self, result: Option<&str>) -> anyhow::Result<Option<String>> {
        let payload = self.payload(HookEvent::Stop, json!({ "result": result }));
        let result = self
            .run_and_aggregate(HookEvent::Stop, &HookSource::None, payload)
            .await?;
        Ok(join_context(&result.additional_context))
    }

    pub async fn error_occurred(&self, error: &anyhow::Error) -> anyhow::Result<()> {
        let payload = self.payload(
            HookEvent::ErrorOccurred,
            json!({
                "error": {
                    "message": error.to_string(),
                    "name": "Error",
                }
            }),
        );
        self.run_and_aggregate(HookEvent::ErrorOccurred, &HookSource::None, payload)
            .await?;
        Ok(())
    }

    async fn run_and_aggregate(
        &self,
        event: HookEvent,
        source: &HookSource,
        payload: Value,
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
            async move { run_command_hook(command, payload, cwd).await }
        });
        let outcomes = futures::future::join_all(futures).await;

        let mut aggregated = AggregatedHookResult::default();
        for outcome in outcomes {
            self.record_outcome(event, source, &outcome);

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

    fn record_outcome(&self, event: HookEvent, source: &HookSource, outcome: &InvocationOutcome) {
        let stderr_bytes = outcome.stderr.len();
        let stdout_bytes = outcome.stdout.len();
        let level_error = outcome.command.fail_closed && outcome.error.is_some();
        let source_str = source.as_display_str();
        let decision = outcome
            .parsed
            .as_ref()
            .map_or("error", |p| p.decision.decision_label());
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
                fail_closed = outcome.command.fail_closed,
                "Hook invocation completed"
            );
        }

        let Some(writer) = &self.context.session_writer else {
            return;
        };
        let record = SessionRecord::HookEvent {
            timestamp: chrono::Utc::now(),
            task_id: self.context.task_id.to_string(),
            event: event.as_str().to_string(),
            source: source.as_display_str().map(ToOwned::to_owned),
            source_file: outcome.command.source_path.clone(),
            command: outcome.command.command.clone(),
            exit_code: outcome.exit_code,
            duration_ms: outcome.duration.as_millis().try_into().unwrap_or(u64::MAX),
            decision: decision.to_owned(),
            fail_closed: outcome.command.fail_closed,
            stdout: outcome.stdout.clone(),
            stderr: outcome.stderr.clone(),
        };

        if let Err(error) = writer.append_record(&record) {
            tracing::warn!(
                target: "cake::hooks",
                error = %error,
                "Failed to append hook transcript record"
            );
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "hook command execution has many branches for error handling"
)]
async fn run_command_hook(command: HookCommand, payload: Value, cwd: PathBuf) -> InvocationOutcome {
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

    if let Some(mut stdin) = child.stdin.take() {
        let input = serde_json::to_vec(&payload).unwrap_or_default();
        if let Err(error) = stdin.write_all(&input).await
            && error.kind() != std::io::ErrorKind::BrokenPipe
        {
            return InvocationOutcome {
                command,
                exit_code: None,
                duration: start.elapsed(),
                stdout: String::new(),
                stderr: String::new(),
                parsed: None,
                error: Some(format!("failed to write hook stdin: {error}")),
            };
        }
    }

    let timeout_result = timeout(command.timeout, child.wait_with_output()).await;
    let output = match timeout_result {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
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

    let stdout = capped_text(&output.stdout);
    let stderr = capped_text(&output.stderr);
    let exit_code = output.status.code();

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
        cmd
    }

    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
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

fn parse_tool_input(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| json!({}))
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

fn capped_text(bytes: &[u8]) -> String {
    if bytes.len() <= HOOK_OUTPUT_LIMIT {
        return String::from_utf8_lossy(bytes).to_string();
    }
    let omitted = bytes.len() - HOOK_OUTPUT_LIMIT;
    format!(
        "{}... (truncated, {omitted} more bytes)",
        String::from_utf8_lossy(&bytes[..HOOK_OUTPUT_LIMIT])
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::hooks::{HookGroup, HookMatcher};

    fn runner(command: &str, fail_closed: bool) -> HookRunner {
        let cwd = std::env::temp_dir();
        let source_path = cwd.join("hooks.json");
        let command = HookCommand {
            command: command.to_string(),
            timeout: Duration::from_secs(2),
            fail_closed,
            status_message: None,
            source_path: source_path.clone(),
        };
        let loaded = LoadedHooks {
            groups: vec![HookGroup {
                source_path,
                event: HookEvent::PreToolUse,
                matcher: HookMatcher::All,
                hooks: vec![command],
            }],
        };
        HookRunner::new(
            loaded,
            HookContext {
                session_id: uuid::Uuid::new_v4(),
                task_id: uuid::Uuid::new_v4(),
                transcript_path: None,
                session_writer: None,
                cwd,
                model: "test-model".to_string(),
            },
        )
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn command_hook_receives_stdin_json() {
        let runner = runner(
            "payload=$(cat); case \"$payload\" in *'\"tool_name\":\"Bash\"'*) printf '{\"permission\":\"allow\"}' ;; *) exit 1 ;; esac",
            false,
        );

        let plan = runner
            .pre_tool_use("Bash", "call-1", r#"{"command":"printf ok"}"#)
            .await
            .unwrap();

        assert!(matches!(plan, ToolHookPlan::Execute { .. }));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn exit_two_blocks_pre_tool_use() {
        let runner = runner("echo blocked >&2; exit 2", false);

        let plan = runner
            .pre_tool_use("Bash", "call-1", r#"{"command":"printf ok"}"#)
            .await
            .unwrap();

        match plan {
            ToolHookPlan::Block { reason, .. } => assert!(reason.contains("blocked")),
            ToolHookPlan::Execute { .. } => panic!("expected block"),
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn invalid_json_fails_open_by_default() {
        let runner = runner("printf not-json", false);

        let plan = runner
            .pre_tool_use("Bash", "call-1", r#"{"command":"printf ok"}"#)
            .await
            .unwrap();

        assert!(matches!(plan, ToolHookPlan::Execute { .. }));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn post_tool_use_persists_hook_record_while_session_locked() {
        use crate::clients::GitState;
        use crate::clients::types::SessionRecord;
        use crate::config::session::CURRENT_FORMAT_VERSION;
        use crate::config::{Session, SessionWriter};

        let dir = tempfile::TempDir::new().unwrap();
        let session_path = dir.path().join("session.jsonl");

        let session_id = uuid::Uuid::new_v4();
        let meta = SessionRecord::SessionMeta {
            format_version: CURRENT_FORMAT_VERSION,
            session_id: session_id.to_string(),
            timestamp: chrono::Utc::now(),
            working_directory: dir.path().to_path_buf(),
            model: Some("test-model".to_string()),
            tools: Vec::new(),
            cake_version: Some("test".to_string()),
            system_prompt: None,
            git: GitState {
                repository_url: None,
                branch: None,
                commit_hash: None,
            },
        };
        let file = Session::create_on_disk(&session_path, &meta).unwrap();
        let writer = SessionWriter::new(file);

        let source_path = dir.path().join("hooks.json");
        let command = HookCommand {
            command: r#"printf '{"permission":"allow"}'"#.to_string(),
            timeout: Duration::from_secs(2),
            fail_closed: false,
            status_message: None,
            source_path: source_path.clone(),
        };
        let loaded = LoadedHooks {
            groups: vec![HookGroup {
                source_path,
                event: HookEvent::PostToolUse,
                matcher: HookMatcher::All,
                hooks: vec![command],
            }],
        };
        let runner = HookRunner::new(
            loaded,
            HookContext {
                session_id,
                task_id: uuid::Uuid::new_v4(),
                transcript_path: Some(session_path.clone()),
                session_writer: Some(writer.clone()),
                cwd: dir.path().to_path_buf(),
                model: "test-model".to_string(),
            },
        );

        runner
            .post_tool_use(
                "Bash",
                "call-1",
                r#"{"command":"printf ok"}"#,
                &Ok("ok".to_string()),
            )
            .await
            .unwrap();

        // SessionWriter still holds the lock; reads via fs are unaffected by
        // advisory locks on macOS/Linux.
        let content = std::fs::read_to_string(&session_path).unwrap();
        assert!(
            content.contains(r#""type":"hook_event""#),
            "expected hook_event record in {content}"
        );
        assert!(content.contains(r#""event":"PostToolUse""#));
        drop(writer);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn fail_closed_invalid_json_blocks() {
        let runner = runner("printf not-json", true);

        let plan = runner
            .pre_tool_use("Bash", "call-1", r#"{"command":"printf ok"}"#)
            .await
            .unwrap();

        assert!(matches!(plan, ToolHookPlan::Block { .. }));
    }

    // ── HookDecision::from_raw ──────────────────────────────────────

    #[test]
    fn decision_from_raw_continue_when_no_fields() {
        let d = HookDecision::from_raw(None, None, None, None, None);
        assert_eq!(d, HookDecision::Continue);
    }

    #[test]
    fn decision_from_raw_continue_from_allow_permission() {
        let d = HookDecision::from_raw(None, None, Some("allow"), None, None);
        assert_eq!(d, HookDecision::Continue);
    }

    #[test]
    fn decision_from_raw_continue_from_allow_decision_field() {
        let d = HookDecision::from_raw(None, None, None, Some("allow"), None);
        assert_eq!(d, HookDecision::Continue);
    }

    #[test]
    fn decision_from_raw_continue_from_unknown_permission() {
        // Unknown values are treated as allow (continue).
        let d = HookDecision::from_raw(None, None, Some("xyz"), None, None);
        assert_eq!(d, HookDecision::Continue);
    }

    #[test]
    fn decision_from_raw_continue_true_is_same_as_none() {
        let d = HookDecision::from_raw(Some(true), None, None, None, None);
        assert_eq!(d, HookDecision::Continue);
    }

    #[test]
    fn decision_from_raw_stop_from_continue_false() {
        let d = HookDecision::from_raw(Some(false), None, None, None, None);
        assert_eq!(
            d,
            HookDecision::Stop {
                reason: "hook requested stop".to_string()
            }
        );
    }

    #[test]
    fn decision_from_raw_stop_from_continue_false_with_stop_reason() {
        let d = HookDecision::from_raw(Some(false), Some("done"), None, None, None);
        assert_eq!(
            d,
            HookDecision::Stop {
                reason: "done".to_string()
            }
        );
    }

    #[test]
    fn decision_from_raw_stop_from_continue_false_with_reason() {
        let d = HookDecision::from_raw(Some(false), None, None, None, Some("enough"));
        assert_eq!(
            d,
            HookDecision::Stop {
                reason: "enough".to_string()
            }
        );
    }

    #[test]
    fn decision_from_raw_stop_from_continue_false_stop_reason_wins_over_reason() {
        let d = HookDecision::from_raw(Some(false), Some("first"), None, None, Some("second"));
        assert_eq!(
            d,
            HookDecision::Stop {
                reason: "first".to_string()
            }
        );
    }

    #[test]
    fn decision_from_raw_stop_continue_false_takes_priority_over_permission() {
        // continue: false wins even if permission says allow.
        let d = HookDecision::from_raw(Some(false), None, Some("allow"), None, None);
        assert!(matches!(d, HookDecision::Stop { .. }));
    }

    #[test]
    fn decision_from_raw_deny_from_permission_deny() {
        let d = HookDecision::from_raw(None, None, Some("deny"), None, None);
        assert_eq!(
            d,
            HookDecision::Deny {
                reason: "hook denied action".to_string()
            }
        );
    }

    #[test]
    fn decision_from_raw_deny_from_decision_field_block() {
        let d = HookDecision::from_raw(None, None, None, Some("block"), None);
        assert_eq!(
            d,
            HookDecision::Deny {
                reason: "hook denied action".to_string()
            }
        );
    }

    #[test]
    fn decision_from_raw_deny_from_ask_with_custom_default() {
        let d = HookDecision::from_raw(None, None, Some("ask"), None, None);
        assert_eq!(
            d,
            HookDecision::Deny {
                reason: "interactive ask is not supported yet".to_string()
            }
        );
    }

    #[test]
    fn decision_from_raw_deny_from_permission_deny_with_reason() {
        let d = HookDecision::from_raw(None, None, Some("deny"), None, Some("not allowed"));
        assert_eq!(
            d,
            HookDecision::Deny {
                reason: "not allowed".to_string()
            }
        );
    }

    #[test]
    fn decision_from_raw_permission_takes_priority_over_decision_field() {
        // permission is checked first, so "deny" in permission wins over
        // "allow" in decision.
        let d = HookDecision::from_raw(None, None, Some("deny"), Some("allow"), None);
        assert!(matches!(d, HookDecision::Deny { .. }));
    }

    // ── RawHookOutput deserialization ──────────────────────────────

    #[test]
    fn raw_deser_empty_object_is_continue() {
        let raw: RawHookOutput = serde_json::from_str("{}").unwrap();
        let parsed: ParsedHookOutput = raw.into();
        assert_eq!(parsed.decision, HookDecision::Continue);
        assert!(parsed.updated_input.is_none());
        assert!(parsed.additional_context.is_none());
    }

    #[test]
    fn raw_deser_permission_allow() {
        let raw: RawHookOutput = serde_json::from_str(r#"{"permission": "allow"}"#).unwrap();
        let parsed: ParsedHookOutput = raw.into();
        assert_eq!(parsed.decision, HookDecision::Continue);
    }

    #[test]
    fn raw_deser_continue_false_with_stop_reason() {
        let raw: RawHookOutput =
            serde_json::from_str(r#"{"continue": false, "stop_reason": "done"}"#).unwrap();
        let parsed: ParsedHookOutput = raw.into();
        assert_eq!(
            parsed.decision,
            HookDecision::Stop {
                reason: "done".to_string()
            }
        );
    }

    #[test]
    fn raw_deser_decision_deny_with_reason() {
        let raw: RawHookOutput =
            serde_json::from_str(r#"{"decision": "deny", "reason": "nope"}"#).unwrap();
        let parsed: ParsedHookOutput = raw.into();
        assert_eq!(
            parsed.decision,
            HookDecision::Deny {
                reason: "nope".to_string()
            }
        );
    }

    #[test]
    fn raw_deser_permission_block() {
        let raw: RawHookOutput = serde_json::from_str(r#"{"permission": "block"}"#).unwrap();
        let parsed: ParsedHookOutput = raw.into();
        assert!(matches!(parsed.decision, HookDecision::Deny { .. }));
    }

    #[test]
    fn raw_deser_updated_input_and_additional_context_with_allow() {
        let raw: RawHookOutput = serde_json::from_str(
            r#"{"permission": "allow", "updated_input": {"cmd": "safe"}, "additional_context": "be careful"}"#,
        )
        .unwrap();
        let parsed: ParsedHookOutput = raw.into();
        assert_eq!(parsed.decision, HookDecision::Continue);
        assert_eq!(
            parsed.updated_input,
            Some(serde_json::json!({"cmd": "safe"}))
        );
        assert_eq!(parsed.additional_context, Some("be careful".to_string()));
    }

    // ── HookDecision::decision_label ───────────────────────────────

    #[test]
    fn decision_label_continue_is_none() {
        assert_eq!(HookDecision::Continue.decision_label(), "none");
    }

    #[test]
    fn decision_label_deny_is_deny() {
        let d = HookDecision::Deny {
            reason: "x".to_string(),
        };
        assert_eq!(d.decision_label(), "deny");
    }

    #[test]
    fn decision_label_stop_is_stop() {
        let d = HookDecision::Stop {
            reason: "x".to_string(),
        };
        assert_eq!(d.decision_label(), "stop");
    }
}
