mod agent_loop;
mod agent_telemetry;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::OutputFormat;
use crate::clients::agent_observer::AgentObserver;
use crate::clients::agent_runner::AgentRunner;
use crate::clients::agent_state::{ConversationState, accumulate_usage};
use crate::clients::backend::Backend;
use crate::clients::skill_dedup::SkillActivations;
use crate::clients::tools::{ToolContext, ToolRegistry, default_tool_registry};
use crate::config::model::ResolvedModelConfig;
use crate::config::skills::Skill;
use crate::hooks::HookRunner;
use crate::session_telemetry::{
    SessionTelemetryContext, SessionTelemetryRecord, SessionTelemetrySettings,
    SessionTelemetryWriter,
};
use crate::types::{
    ConversationItem, Role, SessionRecord, StreamRecord, TaskCompleteData, TaskOutcome,
    TaskStartData, Usage,
};

/// Result of a single API turn (one request/response cycle).
#[derive(Debug)]
pub(super) struct TurnResult {
    pub(super) items: Vec<ConversationItem>,
    pub(super) usage: Option<Usage>,
}

struct AgentTelemetry {
    context: SessionTelemetryContext,
    writer: SessionTelemetryWriter,
}

// =============================================================================
// Agent (shared loop over any backend)
// =============================================================================

/// Orchestrates conversation loops, tool execution, and API communication.
///
/// The `Agent` manages the conversation history, executes tool calls from the
/// AI model, and handles streaming output. It supports multiple API backends
/// through the `ApiType` configuration.
pub struct Agent {
    config: ResolvedModelConfig,
    runner: AgentRunner,
    observer: AgentObserver,
    /// Conversation history using typed items
    conversation: ConversationState,
    tools: ToolRegistry,
    tool_context: Arc<ToolContext>,
    /// Session ID for tracking
    session_id: uuid::Uuid,
    /// Task ID for the current CLI invocation.
    task_id: uuid::Uuid,
    /// Accumulated usage across all API calls
    total_usage: Usage,
    /// Number of API calls made
    turn_count: u32,
    /// Number of tool calls executed
    tool_call_count: u32,
    /// Maps SKILL.md paths to skills for activation deduplication.
    /// When the Read tool targets one of these paths, the agent checks if the
    /// skill has already been activated and returns a lightweight message instead.
    skill_locations: Arc<HashMap<PathBuf, Skill>>,
    /// Names of active and in-progress skill activations.
    /// Shared between tool executions for concurrent access.
    skill_activations: Arc<Mutex<SkillActivations>>,
    /// Accumulated permission/policy denials encountered during this task.
    /// Hook-blocked tool calls are recorded here so the task completion
    /// record can report them as structured `permission_denials`.
    permission_denials: Vec<String>,
    /// Optional user-configured command hook runner.
    hook_runner: Option<Arc<HookRunner>>,
    /// Optional best-effort telemetry sidecar writer.
    telemetry: Option<AgentTelemetry>,
}

impl Agent {
    /// Creates a new agent with the given configuration and initial prompt messages.
    ///
    /// The agent is initialized with four default tools: Bash, Read, Edit, and Write.
    /// A new session ID is generated automatically.
    pub fn new(config: ResolvedModelConfig, initial_messages: &[(Role, String)]) -> Self {
        Self {
            runner: AgentRunner::new(Backend::from_api_type(config.model_config.api_type)),
            config,
            observer: AgentObserver::default(),
            conversation: ConversationState::new(initial_messages),
            tools: default_tool_registry(),
            tool_context: Arc::new(ToolContext::from_current_process()),
            session_id: uuid::Uuid::new_v4(),
            task_id: uuid::Uuid::new_v4(),
            total_usage: Usage::default(),
            turn_count: 0,
            tool_call_count: 0,
            skill_locations: Arc::new(HashMap::new()),
            skill_activations: Arc::new(Mutex::new(SkillActivations::default())),
            permission_denials: Vec::new(),
            hook_runner: None,
            telemetry: None,
        }
    }

    /// Returns the enabled tool names.
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.names()
    }

    #[cfg(test)]
    fn history(&self) -> &[ConversationItem] {
        self.conversation.history()
    }

    #[cfg(test)]
    const fn history_mut(&mut self) -> &mut Vec<ConversationItem> {
        self.conversation.history_mut()
    }

    /// Sets the directory context used for tool execution and sandboxing.
    pub fn with_tool_context(mut self, context: Arc<ToolContext>) -> Self {
        self.tool_context = context;
        self
    }

    /// Returns the resolved provider model identifier.
    pub fn model_name(&self) -> &str {
        &self.config.model_config.model
    }

    pub const fn session_telemetry_settings(
        &self,
        output_format: OutputFormat,
    ) -> SessionTelemetrySettings {
        SessionTelemetrySettings {
            api_type: self.config.model_config.api_type,
            output_format,
            max_output_tokens: self.config.model_config.max_output_tokens,
            reasoning_effort: self.config.model_config.reasoning_effort,
            reasoning_max_tokens: self.config.model_config.reasoning_max_tokens,
        }
    }

    /// Returns the session ID.
    pub const fn session_id(&self) -> uuid::Uuid {
        self.session_id
    }

    /// Returns accumulated usage across all API calls.
    pub const fn total_usage(&self) -> &Usage {
        &self.total_usage
    }

    /// Returns the number of API calls made.
    pub const fn turn_count(&self) -> u32 {
        self.turn_count
    }

    /// Sets the session ID for a restored session.
    ///
    /// Use this when continuing a previous session to preserve the session ID.
    pub fn with_session_id(mut self, id: uuid::Uuid) -> Self {
        self.session_id = id;
        if let Some(telemetry) = &mut self.telemetry {
            telemetry.context.session_id = id.to_string();
        }
        self
    }

    /// Sets the task ID for the current invocation.
    pub const fn with_task_id(mut self, id: uuid::Uuid) -> Self {
        self.task_id = id;
        self
    }

    /// Sets accumulated usage (for test fixtures).
    #[cfg(test)]
    pub const fn with_total_usage(mut self, usage: Usage) -> Self {
        self.total_usage = usage;
        self
    }

    /// Sets the turn count (for test fixtures).
    #[cfg(test)]
    pub const fn with_turn_count(mut self, count: u32) -> Self {
        self.turn_count = count;
        self
    }

    /// Sets the conversation history for a restored session.
    ///
    /// Use this when continuing a previous session to restore the conversation context.
    pub fn with_history(mut self, messages: Vec<ConversationItem>) -> anyhow::Result<Self> {
        self.conversation.with_restored_history(messages)?;
        Ok(self)
    }

    /// Set the skill locations for deduplication.
    ///
    /// These paths are checked when the Read tool is used. If the model reads
    /// a SKILL.md file that was already read in this session, a lightweight
    /// "already activated" message is returned instead of the full content.
    pub fn with_skill_locations(mut self, locations: HashMap<PathBuf, Skill>) -> Self {
        self.skill_locations = Arc::new(locations);
        self
    }

    /// Set the initially activated skills (used when resuming a session).
    ///
    /// These skills are pre-seeded into the activated set so they are not
    /// re-read during the resumed session.
    pub fn with_activated_skills(self, skills: HashSet<String>) -> Self {
        {
            let mut guard = self.skill_activations.lock().unwrap_or_else(|e| {
                tracing::error!("skill_activations mutex poisoned, recovering: {e}");
                e.into_inner()
            });
            guard.replace_active(skills);
        }
        self
    }

    /// Enables command hooks for lifecycle and tool-call events.
    pub fn with_hook_runner(mut self, runner: Arc<HookRunner>) -> Self {
        self.hook_runner = Some(runner);
        self
    }

    /// Append hook-provided developer context before the next provider request.
    pub fn append_developer_context(&mut self, contexts: Vec<String>) {
        self.conversation.append_developer_context(contexts);
    }

    /// Returns the names of skills that have been activated in this session.
    #[cfg(test)]
    pub(crate) fn test_active_skills(&self) -> HashSet<String> {
        self.skill_activations
            .lock()
            .unwrap_or_else(|e| {
                tracing::error!("skill_activations mutex poisoned, recovering: {e}");
                e.into_inner()
            })
            .active
            .clone()
    }

    /// Enables streaming JSON output for each message.
    ///
    /// The callback receives a JSON string for each message, tool call, and result.
    /// This is useful for integrating with other tools or TUIs.
    pub fn with_streaming_json(mut self, callback: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.observer.set_streaming_json(callback);
        self
    }

    /// Enables live append-only persistence for each emitted task record.
    pub fn with_persist_callback(
        mut self,
        callback: impl FnMut(&SessionRecord) -> anyhow::Result<()> + Send + Sync + 'static,
    ) -> Self {
        self.observer.set_persist_callback(callback);
        self
    }

    /// Enables progress reporting for tool execution.
    ///
    /// The callback receives conversation items as they occur, useful for
    /// displaying human-readable progress during long-running operations.
    pub fn with_progress_callback(
        mut self,
        callback: impl Fn(&ConversationItem) + Send + Sync + 'static,
    ) -> Self {
        self.observer.set_progress_callback(callback);
        self
    }

    /// Enables retry wait reporting.
    pub fn with_retry_callback(
        mut self,
        callback: impl Fn(&crate::clients::retry::RetryStatus) + Send + Sync + 'static,
    ) -> Self {
        self.observer.set_retry_callback(callback);
        self
    }

    /// Enables best-effort session telemetry sidecar writing.
    pub fn with_session_telemetry(
        mut self,
        writer: SessionTelemetryWriter,
        invocation_id: uuid::Uuid,
    ) -> Self {
        self.telemetry = Some(AgentTelemetry {
            context: SessionTelemetryContext {
                session_id: self.session_id.to_string(),
                invocation_id: invocation_id.to_string(),
            },
            writer,
        });
        self
    }

    /// Report a conversation item via the progress callback, if set.
    fn report_progress(&self, item: &ConversationItem) {
        self.observer.report_progress(item);
    }

    /// Emit a task record to persistence and streaming sinks.
    fn stream_record(&mut self, record: StreamRecord) -> anyhow::Result<()> {
        self.observer.stream_record(record)
    }

    /// Persist a session-only audit record without emitting it to stream-json.
    fn persist_record(&mut self, record: &SessionRecord) -> anyhow::Result<()> {
        self.observer.persist_record(record)
    }

    /// Stream a conversation item as JSON via the streaming callback, if set.
    fn stream_item(&mut self, item: &ConversationItem) -> anyhow::Result<()> {
        self.observer.stream_item(item)
    }

    /// Emit the task start record.
    pub fn emit_task_start_record(&mut self) -> anyhow::Result<()> {
        let record = StreamRecord::TaskStart(TaskStartData {
            session_id: self.session_id.to_string(),
            task_id: self.task_id.to_string(),
            timestamp: chrono::Utc::now(),
        });

        self.stream_record(record)
    }

    /// Emit append-only audit records for mutable prompt context used by this task.
    pub fn emit_prompt_context_records(&mut self) -> anyhow::Result<()> {
        let timestamp = chrono::Utc::now();
        let prompt_context: Vec<_> = self
            .conversation
            .history()
            .iter()
            .take_while(|item| {
                matches!(
                    item,
                    ConversationItem::Message {
                        role: Role::System | Role::Developer,
                        ..
                    }
                )
            })
            .filter_map(|item| {
                let ConversationItem::Message { role, content, .. } = item else {
                    return None;
                };
                (!matches!(role, Role::System)).then(|| SessionRecord::PromptContext {
                    session_id: self.session_id.to_string(),
                    task_id: self.task_id.to_string(),
                    role: *role,
                    content: content.clone(),
                    timestamp,
                })
            })
            .collect();

        for record in &prompt_context {
            self.persist_record(record)?;
        }
        Ok(())
    }

    /// Accumulate usage from an API turn
    const fn accumulate_usage(&mut self, turn_usage: Option<&Usage>) {
        accumulate_usage(&mut self.total_usage, &mut self.turn_count, turn_usage);
    }

    /// Emit the task completion record with success/error and usage stats.
    pub fn emit_task_complete_record(
        &mut self,
        outcome: TaskOutcome,
        duration_ms: u64,
    ) -> anyhow::Result<()> {
        let permission_denials = if self.permission_denials.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.permission_denials))
        };
        let record = StreamRecord::TaskComplete(TaskCompleteData {
            outcome,
            duration_ms,
            turn_count: self.turn_count,
            tool_call_count: self.tool_call_count,
            session_id: self.session_id.to_string(),
            task_id: self.task_id.to_string(),
            usage: self.total_usage,
            permission_denials,
        });

        self.stream_record(record)
    }

    /// Emit the final telemetry summary for this CLI invocation.
    pub fn emit_session_summary_telemetry(
        &mut self,
        success: bool,
        duration_ms: u64,
        error: Option<String>,
    ) {
        let Some(context) = self.telemetry_context() else {
            return;
        };
        let record = SessionTelemetryRecord::SessionSummary {
            session_id: context.session_id,
            invocation_id: context.invocation_id,
            timestamp: chrono::Utc::now(),
            success,
            duration_ms,
            turn_count: self.turn_count,
            usage: self.total_usage,
            error,
        };
        self.append_telemetry_record(&record);
    }
}

#[cfg(test)]
#[path = "agent/agent_tests.rs"]
mod tests;
