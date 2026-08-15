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
use crate::clients::tools::{
    SandboxPolicy, ToolContext, ToolRegistry, default_tool_registry, toolbox_tool_entry,
};
use crate::config::model::ResolvedModelConfig;
use crate::config::output_schema::OutputSchema;
use crate::config::skills::Skill;
use crate::config::toolbox::ToolboxTool;
use crate::hooks::HookRunner;
use crate::session_telemetry::{
    JudgeAttemptSink, ProviderTermination, SessionTelemetryContext, SessionTelemetryRecord,
    SessionTelemetrySettings, SessionTelemetryWriter, SharedSessionTelemetryWriter,
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
    pub(super) termination: Option<ProviderTermination>,
    pub(super) provider_request_id: Option<String>,
}

struct AgentTelemetry {
    context: SessionTelemetryContext,
    writer: Arc<SharedSessionTelemetryWriter>,
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
    /// Maps SKILL.md paths to skills for path-watching `SkillActivated` records.
    /// When the Read tool targets one of these paths, a `SkillActivated`
    /// record is emitted once per skill per session.
    skill_locations: Arc<HashMap<PathBuf, Skill>>,
    /// Names of skills that have already had a `SkillActivated` record emitted
    /// in this session. Shared between concurrent tool executions.
    activated_skills: Arc<Mutex<HashSet<String>>>,
    /// Accumulated permission/policy denials encountered during this task.
    /// Hook-blocked tool calls are recorded here so the task completion
    /// record can report them as structured `permission_denials`.
    permission_denials: Vec<String>,
    /// Optional user-configured command hook runner.
    hook_runner: Option<Arc<HookRunner>>,
    /// Optional best-effort telemetry sidecar writer.
    telemetry: Option<AgentTelemetry>,
    /// Optional compiled JSON Schema constraining the final response
    /// (`--output-schema`). Intermediate turns run unconstrained.
    output_schema: Option<Arc<OutputSchema>>,
    /// Whether correction turns may attach the provider's native
    /// structured-output constraint. Cleared for the rest of the run when the
    /// provider rejects the constrained request (HTTP 400); never reset.
    native_constraint_enabled: bool,
}

impl Agent {
    /// Creates a new agent with the given configuration and initial prompt messages.
    ///
    /// The agent is initialized with four default tools: Bash, Read, Edit, and Write.
    /// Attaching a read-only tool context via [`Self::with_tool_context`] removes
    /// Edit and Write. A new session ID is generated automatically.
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
            activated_skills: Arc::new(Mutex::new(HashSet::new())),
            permission_denials: Vec::new(),
            hook_runner: None,
            telemetry: None,
            output_schema: None,
            native_constraint_enabled: true,
        }
    }

    /// Returns the enabled tool names.
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.names()
    }

    /// Returns the tool context attached to this agent (for tests).
    #[cfg(test)]
    pub(crate) const fn tool_context(&self) -> &Arc<ToolContext> {
        &self.tool_context
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
    ///
    /// Under the read-only sandbox policy, the Edit and Write tools are
    /// removed from the registry: they mutate files in-process, outside the
    /// OS sandbox that only wraps Bash, so they must not be offered at all
    /// for the policy's no-mutation guarantee to hold.
    pub fn with_tool_context(mut self, context: Arc<ToolContext>) -> Self {
        if context.sandbox_policy == SandboxPolicy::ReadOnly {
            self.tools.retain_read_safe_tools();
        }
        self.tool_context = context;
        self
    }

    /// Registers user-defined toolbox tools (`tb__*`) alongside the built-in
    /// tools.
    ///
    /// Call after [`Self::with_session_id`], if any: each tool's executor
    /// captures the current session id to expose `CAKE_THREAD_ID` and
    /// `AGENT_THREAD_ID` to the tool process.
    ///
    /// Toolbox tools run as unsandboxed external processes, so under the
    /// read-only sandbox policy they are never registered — this method
    /// skips them when the current tool context is read-only, and
    /// [`Self::with_tool_context`] strips any already-registered `tb__*`
    /// entries when it applies a read-only context, keeping the policy's
    /// no-mutation guarantee regardless of builder call order.
    pub fn with_toolbox_tools(mut self, toolbox_tools: Vec<ToolboxTool>) -> Self {
        if self.tool_context.sandbox_policy == SandboxPolicy::ReadOnly {
            return self;
        }
        for tool in toolbox_tools {
            self.tools
                .push_entry(toolbox_tool_entry(tool, self.session_id));
        }
        self
    }

    /// Sets the tool registry (for test fixtures).
    #[cfg(test)]
    pub(super) fn with_tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = tools;
        self
    }

    /// Sets the accumulated permission denials (for test fixtures).
    #[cfg(test)]
    pub(super) fn with_permission_denials(mut self, denials: Vec<String>) -> Self {
        self.permission_denials = denials;
        self
    }

    /// Sets the max output tokens config value (for test fixtures).
    #[cfg(test)]
    pub const fn with_max_output_tokens(mut self, val: Option<u32>) -> Self {
        self.config.model_config.max_output_tokens = val;
        self
    }

    /// Sets the context window config value (for test fixtures).
    #[cfg(test)]
    pub const fn with_context_window(mut self, val: Option<u32>) -> Self {
        self.config.model_config.context_window = val;
        self
    }

    /// Sets the reasoning max tokens config value (for test fixtures).
    #[cfg(test)]
    pub const fn with_reasoning_max_tokens(mut self, val: Option<u32>) -> Self {
        self.config.model_config.reasoning_max_tokens = val;
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

    /// Returns the task ID.
    #[cfg(test)]
    pub const fn task_id(&self) -> uuid::Uuid {
        self.task_id
    }

    /// Returns the accumulated permission denials.
    #[cfg(test)]
    pub fn permission_denials(&self) -> &[String] {
        &self.permission_denials
    }

    /// Returns accumulated usage across all API calls.
    pub const fn total_usage(&self) -> &Usage {
        &self.total_usage
    }

    /// Returns the configured model context window in tokens, if set.
    pub const fn context_window(&self) -> Option<u32> {
        self.config.model_config.context_window
    }

    /// Returns the token budget remaining in the configured context window.
    ///
    /// Computed from the accumulated usage across all API calls. `None` when
    /// no window is configured; never negative when configured (saturates at
    /// zero once usage meets or exceeds the window).
    pub fn context_remaining_tokens(&self) -> Option<u64> {
        self.context_window()
            .map(|window| u64::from(window).saturating_sub(self.total_usage.total_tokens))
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

    /// Set the skill locations for path-watching `SkillActivated` records.
    ///
    /// These paths are checked when the Read tool is used. If the model reads
    /// a SKILL.md that matches a known skill path, a `SkillActivated` record is
    /// emitted once per skill per session.
    pub fn with_skill_locations(mut self, locations: HashMap<PathBuf, Skill>) -> Self {
        self.skill_locations = Arc::new(locations);
        self
    }

    /// Enables command hooks for lifecycle and tool-call events.
    pub fn with_hook_runner(mut self, runner: Arc<HookRunner>) -> Self {
        self.hook_runner = Some(runner);
        self
    }

    /// Constrains the final response to validate against a compiled JSON
    /// Schema. Intermediate tool-use turns are unaffected.
    pub fn set_output_schema(&mut self, schema: Arc<OutputSchema>) {
        self.output_schema = Some(schema);
    }

    /// Builder form of [`Self::set_output_schema`] (for test fixtures).
    #[cfg(test)]
    pub(super) fn with_output_schema(mut self, schema: Arc<OutputSchema>) -> Self {
        self.set_output_schema(schema);
        self
    }

    /// Append hook-provided developer context before the next provider request.
    pub fn append_developer_context(&mut self, contexts: Vec<String>) {
        self.conversation.append_developer_context(contexts);
    }

    /// Append developer context stating the output-schema requirement for the
    /// final response. No-op when no schema is attached.
    pub fn append_output_schema_context(&mut self) {
        let Some(schema) = &self.output_schema else {
            return;
        };
        let schema_json =
            serde_json::to_string_pretty(&schema.raw).unwrap_or_else(|_| schema.raw.to_string());
        let message = format!(
            "The final response for this task (the last assistant message, \
             after all tool use is complete) must be a single JSON document \
             that validates against the following JSON Schema (draft 2020-12). \
             Respond with only the JSON document: no Markdown code fences and \
             no surrounding prose. Intermediate tool use and reasoning are \
             unaffected by this requirement.\n\n{schema_json}"
        );
        self.conversation.append_developer_context(vec![message]);
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

    /// Enables human-readable progress updates for recoverable agent-loop events.
    pub fn with_progress_callback(
        mut self,
        callback: impl Fn(&str) + Send + Sync + 'static,
    ) -> Self {
        self.observer.set_progress(callback);
        self
    }

    /// Enables best-effort session telemetry sidecar writing.
    ///
    /// The writer is shared with a [`JudgeAttemptSink`] installed into the
    /// tool context, so the Bash preflight records judge attempts as soon as
    /// judging completes instead of waiting for the tool result.
    pub fn with_session_telemetry(
        mut self,
        writer: SessionTelemetryWriter,
        invocation_id: uuid::Uuid,
    ) -> Self {
        let context = SessionTelemetryContext {
            session_id: self.session_id.to_string(),
            invocation_id: invocation_id.to_string(),
        };
        let writer = Arc::new(SharedSessionTelemetryWriter::new(writer));
        self.telemetry = Some(AgentTelemetry {
            context: context.clone(),
            writer: Arc::clone(&writer),
        });
        self.install_judge_attempt_sink(JudgeAttemptSink::new(writer, context));
        self
    }

    /// Give the Bash preflight a sink that records judge attempts immediately,
    /// rebuilding the tool context around the judge context with the sink.
    fn install_judge_attempt_sink(&mut self, sink: JudgeAttemptSink) {
        let Some(judge) = self.tool_context.judge.clone() else {
            return;
        };
        let mut judge = (*judge).clone();
        judge.record_attempt = Some(sink);
        let context = Arc::new(
            (*self.tool_context)
                .clone()
                .with_judge(Some(Arc::new(judge))),
        );
        self.tool_context = context;
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

    fn report_progress(&self, message: &str) {
        self.observer.report_progress(message);
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
        let Self {
            conversation,
            session_id,
            task_id,
            observer,
            ..
        } = &mut *self;

        let session_id = session_id.to_string();
        let task_id = task_id.to_string();

        for item in conversation.history().iter().take_while(|item| {
            matches!(
                item,
                ConversationItem::Message {
                    role: Role::System | Role::Developer,
                    ..
                }
            )
        }) {
            if let ConversationItem::Message {
                role: Role::Developer,
                content,
                ..
            } = item
            {
                observer.persist_record(&SessionRecord::PromptContext {
                    session_id: session_id.clone(),
                    task_id: task_id.clone(),
                    role: Role::Developer,
                    content: content.clone(),
                    timestamp,
                })?;
            }
        }
        Ok(())
    }

    /// Emit the repair outputs synthesized while restoring history.
    ///
    /// A restored conversation can contain a function call whose output the
    /// previous process never persisted. [`Self::with_history`] closes each
    /// such call in memory; this appends the same items to the session file so
    /// later restores read a well-formed history. No-op for a fresh session or
    /// a restored one whose calls are all matched.
    pub fn emit_history_repair_records(&mut self) -> anyhow::Result<()> {
        for item in self.conversation.take_pending_repairs() {
            self.stream_item(&item)?;
        }
        Ok(())
    }

    /// Accumulate usage from an API turn.
    const fn accumulate_usage(&mut self, turn_usage: Option<&Usage>) {
        accumulate_usage(&mut self.total_usage, turn_usage);
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
