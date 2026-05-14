use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use futures::FutureExt;

use crate::clients::agent_observer::AgentObserver;
use crate::clients::agent_runner::AgentRunner;
use crate::clients::agent_state::{ConversationState, accumulate_usage};
use crate::clients::backend::Backend;
use crate::clients::tools::{ToolContext, ToolRegistry, default_tool_registry};
use crate::clients::types::{
    ConversationItem, SessionRecord, StreamRecord, TaskCompleteData, TaskOutcome, TaskStartData,
    Usage,
};
#[cfg(test)]
use crate::config::model::ApiType;
use crate::config::model::ResolvedModelConfig;
use crate::config::skills::Skill;
use crate::hooks::{HookRunner, ToolHookPlan};
use crate::models::{Message, Role};

/// Result of a single API turn (one request/response cycle).
#[derive(Debug)]
pub(super) struct TurnResult {
    pub(super) items: Vec<ConversationItem>,
    pub(super) usage: Option<Usage>,
}

#[derive(Debug, Clone)]
struct SkillActivation {
    name: String,
    path: PathBuf,
}

#[derive(Debug, Clone)]
struct ToolExecutionOutput {
    output: String,
    skill_activation: Option<SkillActivation>,
}

#[derive(Debug, Default)]
struct SkillActivations {
    active: HashSet<String>,
    pending: HashSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkillReservation {
    Reserved,
    AlreadyActive,
    AlreadyPending,
}

impl SkillActivations {
    fn replace_active(&mut self, skills: HashSet<String>) {
        self.active = skills;
        self.pending.clear();
    }

    fn reserve(&mut self, name: &str) -> SkillReservation {
        if self.active.contains(name) {
            return SkillReservation::AlreadyActive;
        }
        if !self.pending.insert(name.to_string()) {
            return SkillReservation::AlreadyPending;
        }
        SkillReservation::Reserved
    }

    fn complete(&mut self, name: &str) {
        self.pending.remove(name);
        self.active.insert(name.to_string());
    }

    fn fail(&mut self, name: &str) {
        self.pending.remove(name);
    }
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
    /// Optional user-configured command hook runner.
    hook_runner: Option<Arc<HookRunner>>,
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
            hook_runner: None,
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
    pub const fn with_session_id(mut self, id: uuid::Uuid) -> Self {
        self.session_id = id;
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
        let record = StreamRecord::TaskComplete(TaskCompleteData {
            outcome,
            duration_ms,
            turn_count: self.turn_count,
            tool_call_count: self.tool_call_count,
            session_id: self.session_id.to_string(),
            task_id: self.task_id.to_string(),
            usage: self.total_usage.clone(),
            permission_denials: None,
        });

        self.stream_record(record)
    }

    /// Sends a message and runs the agent loop until completion.
    ///
    /// The agent will process the message, execute any tool calls, and continue
    /// until the model produces a final response without requesting more tools.
    ///
    /// # Errors
    ///
    /// Returns an error if the API request fails, the response cannot be parsed,
    /// or a tool execution fails critically.
    #[expect(
        clippy::too_many_lines,
        reason = "agent send loop orchestrates tool execution and retry logic"
    )]
    pub async fn send(&mut self, message: Message) -> anyhow::Result<Option<Message>> {
        let user_item = self.conversation.push_user_message(message.content.clone());
        self.stream_item(&user_item)?;

        // Agent loop: continue until model stops making tool calls
        loop {
            let TurnResult { items, usage } = self.complete_turn().await?;

            // Accumulate usage
            self.accumulate_usage(usage.as_ref());
            let has_tool_calls = items
                .iter()
                .any(|item| matches!(item, ConversationItem::FunctionCall { .. }));

            // Stream each item as JSON if callback is set
            for item in &items {
                self.stream_item(item)?;
            }

            // Report progress for items, but skip assistant messages on the final turn
            // (they're already printed to stdout, so we'd duplicate)
            for item in &items {
                // Skip assistant messages on the final turn (no tool calls)
                if !has_tool_calls
                    && matches!(
                        item,
                        ConversationItem::Message {
                            role: Role::Assistant,
                            ..
                        }
                    )
                {
                    continue;
                }
                self.report_progress(item);
            }

            // Move items into history
            let turn_start = self.conversation.history().len();
            self.conversation.extend_turn_items(items);

            let function_calls: Vec<_> = self.conversation.history()[turn_start..]
                .iter()
                .filter_map(|item| {
                    if let ConversationItem::FunctionCall {
                        call_id,
                        name,
                        arguments,
                        ..
                    } = item
                    {
                        Some((call_id.as_str(), name.as_str(), arguments.as_str()))
                    } else {
                        None
                    }
                })
                .collect();

            // If no function calls, resolve and return the message
            if function_calls.is_empty() {
                return Ok(Some(self.conversation.resolve_assistant_message()));
            }

            self.tool_call_count += u32::try_from(function_calls.len()).unwrap_or(u32::MAX);

            // Run pre-tool hooks concurrently, then execute allowed tool calls concurrently.
            let hook_runner = self.hook_runner.clone();
            let pre_futures = function_calls
                .into_iter()
                .map(|(call_id, name, arguments)| {
                    let hook_runner = hook_runner.clone();
                    async move {
                        let plan = if let Some(runner) = hook_runner {
                            runner.pre_tool_use(name, call_id, arguments).await?
                        } else {
                            ToolHookPlan::Execute {
                                arguments: arguments.to_owned(),
                                prefix_notice: None,
                                additional_context: Vec::new(),
                            }
                        };
                        anyhow::Ok((call_id.to_owned(), name.to_owned(), plan))
                    }
                });
            let pre_results = futures::future::join_all(pre_futures).await;
            let mut tool_plans = Vec::with_capacity(pre_results.len());
            for result in pre_results {
                tool_plans.push(result?);
            }

            let skill_locations = Arc::clone(&self.skill_locations);
            let skill_activations = Arc::clone(&self.skill_activations);
            let tools = &self.tools;
            let tool_context = Arc::clone(&self.tool_context);
            let futures = tool_plans.into_iter().map(|(call_id, name, plan)| {
                let skill_locations = Arc::clone(&skill_locations);
                let skill_activations = Arc::clone(&skill_activations);
                let tool_context = Arc::clone(&tool_context);
                let hook_runner = self.hook_runner.clone();
                match plan {
                    ToolHookPlan::Block {
                        reason,
                        additional_context,
                    } => async move {
                        let output = format!("Hook blocked tool execution: {reason}");
                        let output = append_hook_context(output, &additional_context);
                        (call_id, output, None)
                    }
                    .boxed(),
                    ToolHookPlan::Execute {
                        arguments,
                        prefix_notice,
                        additional_context,
                    } => async move {
                        let result = execute_tool_with_skill_dedup(
                            tools,
                            Arc::clone(&tool_context),
                            &name,
                            &arguments,
                            skill_locations.as_ref(),
                            &skill_activations,
                        )
                        .await;
                        let hook_result = result
                            .as_ref()
                            .map(|result| result.output.clone())
                            .map_err(std::clone::Clone::clone);

                        let post_context = if let Some(runner) = hook_runner {
                            runner
                                .post_tool_use(&name, &call_id, &arguments, &hook_result)
                                .await
                                .ok()
                                .flatten()
                        } else {
                            None
                        };

                        let (mut output, skill_activation) = match result {
                            Ok(result) => (result.output, result.skill_activation),
                            Err(error) => (format!("Error: {error}"), None),
                        };
                        if let Some(notice) = prefix_notice {
                            output = format!("{notice}{output}");
                        }
                        if let Some(context) = post_context
                            && !context.is_empty()
                        {
                            output.push_str("\n\nAdditional hook context:\n");
                            output.push_str(&context);
                        }
                        output = append_hook_context(output, &additional_context);

                        (call_id, output, skill_activation)
                    }
                    .boxed(),
                }
            });

            let results = futures::future::join_all(futures).await;

            // Add results to history in order
            for (call_id, output, skill_activation) in results {
                if let Some(skill_activation) = skill_activation {
                    let record = SessionRecord::SkillActivated {
                        session_id: self.session_id.to_string(),
                        task_id: self.task_id.to_string(),
                        timestamp: chrono::Utc::now(),
                        name: skill_activation.name,
                        path: skill_activation.path,
                    };
                    self.persist_record(&record)?;
                }
                let item = self.conversation.push_tool_output(call_id, output);
                self.stream_item(&item)?;
            }

            // Loop continues - send next request with tool results included
        }
    }

    /// Execute a single API turn with retry logic.
    async fn complete_turn(&mut self) -> anyhow::Result<TurnResult> {
        let tool_definitions = self.tools.definitions();
        let config = &self.config;
        let session_id = self.session_id;
        let history = self.conversation.history();
        let observer = &self.observer;
        self.runner
            .complete_turn(config, session_id, history, &tool_definitions, |status| {
                observer.report_retry(status);
            })
            .await
    }
}

fn append_hook_context(mut output: String, contexts: &[String]) -> String {
    let contexts = contexts
        .iter()
        .filter(|context| !context.is_empty())
        .map(String::as_str)
        .collect::<Vec<_>>();
    if contexts.is_empty() {
        return output;
    }

    output.push_str("\n\nAdditional hook context:\n");
    output.push_str(&contexts.join("\n\n"));
    output
}

async fn execute_tool_output(
    tools: &ToolRegistry,
    context: Arc<ToolContext>,
    name: &str,
    arguments: &str,
) -> Result<String, String> {
    tools
        .execute(context, name, arguments)
        .await
        .map(|result| result.output)
}

async fn execute_tool_with_skill_dedup(
    tools: &ToolRegistry,
    context: Arc<ToolContext>,
    name: &str,
    arguments: &str,
    skill_locations: &HashMap<PathBuf, Skill>,
    skill_activations: &Arc<Mutex<SkillActivations>>,
) -> Result<ToolExecutionOutput, String> {
    if name != "Read" {
        return execute_tool_output(tools, context, name, arguments)
            .await
            .map(|output| ToolExecutionOutput {
                output,
                skill_activation: None,
            });
    }

    let Some(path_str) = crate::clients::tools::read::extract_path(arguments) else {
        return execute_tool_output(tools, context, name, arguments)
            .await
            .map(|output| ToolExecutionOutput {
                output,
                skill_activation: None,
            });
    };

    let Ok(path) = PathBuf::from(&path_str).canonicalize() else {
        return execute_tool_output(tools, context, name, arguments)
            .await
            .map(|output| ToolExecutionOutput {
                output,
                skill_activation: None,
            });
    };

    let Some(skill) = skill_locations.get(&path) else {
        return execute_tool_output(tools, context, name, arguments)
            .await
            .map(|output| ToolExecutionOutput {
                output,
                skill_activation: None,
            });
    };
    let skill_name = &skill.name;

    let reservation = skill_activations
        .lock()
        .unwrap_or_else(|e| {
            tracing::error!("skill_activations mutex poisoned, recovering: {e}");
            e.into_inner()
        })
        .reserve(skill_name);
    match reservation {
        SkillReservation::Reserved => {},
        SkillReservation::AlreadyActive => {
            tracing::info!("Skill '{skill_name}' already activated, skipping re-read");
            return Ok(ToolExecutionOutput {
                output: format!(
                    "Skill '{skill_name}' is already active in this session. \
                     Its instructions are already in the conversation context."
                ),
                skill_activation: None,
            });
        },
        SkillReservation::AlreadyPending => {
            tracing::info!("Skill '{skill_name}' activation already in progress, skipping re-read");
            return Ok(ToolExecutionOutput {
                output: format!(
                    "Skill '{skill_name}' activation is already in progress in this tool batch. \
                     Its instructions will be included when that read completes."
                ),
                skill_activation: None,
            });
        },
    }

    let output = match skill.load_body() {
        Ok(output) => output,
        Err(error) => {
            skill_activations
                .lock()
                .unwrap_or_else(|e| {
                    tracing::error!("skill_activations mutex poisoned, recovering: {e}");
                    e.into_inner()
                })
                .fail(skill_name);
            return Err(format!(
                "Failed to load skill '{}': {error}",
                skill.location.display()
            ));
        },
    };
    skill_activations
        .lock()
        .unwrap_or_else(|e| {
            tracing::error!("skill_activations mutex poisoned, recovering: {e}");
            e.into_inner()
        })
        .complete(skill_name);
    tracing::info!("Skill '{}' activated", skill_name);
    Ok(ToolExecutionOutput {
        output,
        skill_activation: Some(SkillActivation {
            name: skill_name.clone(),
            path,
        }),
    })
}

#[cfg(test)]
fn test_resolved_model_config(api_type: ApiType, base_url: &str) -> ResolvedModelConfig {
    ResolvedModelConfig {
        model_config: crate::config::model::ModelConfig {
            model: "test-model".to_string(),
            api_type,
            base_url: base_url.to_string(),
            api_key_env: "TEST_API_KEY".to_string(),
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            reasoning_effort: None,
            reasoning_summary: None,
            reasoning_max_tokens: None,
            providers: vec![],
        },
        api_key: "test-key".to_string(),
    }
}

#[cfg(test)]
fn test_agent_for(api_type: ApiType, base_url: &str) -> Agent {
    let mut agent = Agent::new(
        test_resolved_model_config(api_type, base_url),
        &[(Role::System, "test system prompt".to_string())],
    );
    agent.session_id = uuid::uuid!("550e8400-e29b-41d4-a716-446655440000");
    agent.task_id = uuid::uuid!("550e8400-e29b-41d4-a716-446655440001");
    agent.tools = crate::clients::tools::ToolRegistry::empty();
    agent
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::types::{InputTokensDetails, OutputTokensDetails};
    use crate::config::model::ApiType;
    use crate::config::skills::SkillScope;
    use tempfile::TempDir;

    fn test_agent() -> Agent {
        test_agent_for(ApiType::ChatCompletions, "https://api.example.com")
    }

    fn test_skill(name: &str, location: PathBuf) -> Skill {
        Skill {
            name: name.to_string(),
            description: "Test skill".to_string(),
            base_directory: location.parent().unwrap().to_path_buf(),
            location,
            scope: SkillScope::Project,
        }
    }

    #[test]
    fn accumulate_usage_adds_tokens() {
        let mut agent = test_agent();
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
            input_tokens_details: InputTokensDetails { cached_tokens: 10 },
            output_tokens_details: OutputTokensDetails {
                reasoning_tokens: 5,
            },
        };
        agent.accumulate_usage(Some(&usage));
        assert_eq!(agent.total_usage.input_tokens, 100);
        assert_eq!(agent.total_usage.output_tokens, 50);
        assert_eq!(agent.total_usage.total_tokens, 150);
        assert_eq!(agent.total_usage.input_tokens_details.cached_tokens, 10);
        assert_eq!(agent.total_usage.output_tokens_details.reasoning_tokens, 5);
        assert_eq!(agent.turn_count, 1);
    }

    #[test]
    fn accumulate_usage_none_is_noop() {
        let mut agent = test_agent();
        agent.accumulate_usage(None);
        assert_eq!(agent.total_usage.input_tokens, 0);
        assert_eq!(agent.turn_count, 0);
    }

    #[test]
    fn accumulate_usage_accumulates_across_calls() {
        let mut agent = test_agent();
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
            input_tokens_details: InputTokensDetails { cached_tokens: 0 },
            output_tokens_details: OutputTokensDetails {
                reasoning_tokens: 0,
            },
        };
        agent.accumulate_usage(Some(&usage));
        agent.accumulate_usage(Some(&usage));
        assert_eq!(agent.total_usage.input_tokens, 200);
        assert_eq!(agent.total_usage.output_tokens, 100);
        assert_eq!(agent.total_usage.total_tokens, 300);
        assert_eq!(agent.turn_count, 2);
    }

    #[test]
    fn emit_task_complete_record_success() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let captured_clone = captured.clone();
        let mut agent = test_agent().with_streaming_json(move |json| {
            *captured_clone.lock().unwrap() = json.to_string();
        });
        agent
            .emit_task_complete_record(TaskOutcome::Success { result: None }, 1000)
            .unwrap();
        drop(agent);
        let json: serde_json::Value = serde_json::from_str(&captured.lock().unwrap()).unwrap();
        assert_eq!(json["type"], "task_complete");
        assert_eq!(json["subtype"], "success");
        assert_eq!(json["is_error"], false);
        assert_eq!(json["duration_ms"], 1000);
        assert_eq!(json["task_id"], "550e8400-e29b-41d4-a716-446655440001");
        assert!(json.get("success").is_none());
    }

    #[test]
    fn emit_task_complete_record_error() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let captured_clone = captured.clone();
        let mut agent = test_agent().with_streaming_json(move |json| {
            *captured_clone.lock().unwrap() = json.to_string();
        });
        agent
            .emit_task_complete_record(
                TaskOutcome::ErrorDuringExecution {
                    error: "boom".to_string(),
                },
                500,
            )
            .unwrap();
        drop(agent);
        let json: serde_json::Value = serde_json::from_str(&captured.lock().unwrap()).unwrap();
        assert_eq!(json["subtype"], "error_during_execution");
        assert_eq!(json["error"], "boom");
        assert_eq!(json["is_error"], true);
    }

    #[test]
    fn emit_task_complete_record_no_callback() {
        let mut agent = test_agent();
        agent
            .emit_task_complete_record(TaskOutcome::Success { result: None }, 1000)
            .unwrap();
    }

    #[test]
    fn emit_task_start_record() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let captured_clone = captured.clone();
        let mut agent = test_agent().with_streaming_json(move |json| {
            *captured_clone.lock().unwrap() = json.to_string();
        });
        agent.emit_task_start_record().unwrap();
        drop(agent);
        let json: serde_json::Value = serde_json::from_str(&captured.lock().unwrap()).unwrap();
        assert_eq!(json["type"], "task_start");
        assert_eq!(json["session_id"], "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(json["task_id"], "550e8400-e29b-41d4-a716-446655440001");
    }

    #[test]
    fn task_records_fan_out_to_persist_and_stream() {
        let persisted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let persisted_clone = persisted.clone();
        let streamed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let streamed_clone = streamed.clone();

        let mut agent = test_agent()
            .with_persist_callback(move |record| {
                persisted_clone.lock().unwrap().push(record.clone());
                Ok(())
            })
            .with_streaming_json(move |json| {
                streamed_clone.lock().unwrap().push(json.to_string());
            });

        agent.emit_task_start_record().unwrap();
        agent
            .emit_task_complete_record(
                TaskOutcome::Success {
                    result: Some("ok".to_string()),
                },
                42,
            )
            .unwrap();

        let persisted = persisted.lock().unwrap();
        assert!(matches!(
            persisted.first(),
            Some(SessionRecord::TaskStart { .. })
        ));
        assert!(matches!(
            persisted.last(),
            Some(SessionRecord::TaskComplete { .. })
        ));
        drop(persisted);

        let streamed = streamed.lock().unwrap();
        let first: serde_json::Value = serde_json::from_str(&streamed[0]).unwrap();
        let last: serde_json::Value = serde_json::from_str(&streamed[1]).unwrap();
        drop(streamed);
        assert_eq!(first["type"], "task_start");
        assert_eq!(last["type"], "task_complete");
    }

    #[test]
    fn prompt_context_records_persist_without_streaming() {
        let persisted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let persisted_clone = persisted.clone();
        let streamed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let streamed_clone = streamed.clone();

        let mut agent = Agent::new(
            test_resolved_model_config(ApiType::ChatCompletions, "https://api.example.com"),
            &[
                (Role::System, "test system prompt".to_string()),
                (Role::Developer, "AGENTS context".to_string()),
                (Role::Developer, "Environment context".to_string()),
            ],
        )
        .with_session_id(uuid::uuid!("550e8400-e29b-41d4-a716-446655440000"))
        .with_task_id(uuid::uuid!("550e8400-e29b-41d4-a716-446655440001"))
        .with_persist_callback(move |record| {
            persisted_clone.lock().unwrap().push(record.clone());
            Ok(())
        })
        .with_streaming_json(move |json| {
            streamed_clone.lock().unwrap().push(json.to_string());
        });

        agent.emit_prompt_context_records().unwrap();

        let persisted = persisted.lock().unwrap();
        assert_eq!(persisted.len(), 2);
        assert!(matches!(
            &persisted[0],
            SessionRecord::PromptContext {
                role: Role::Developer,
                content,
                ..
            } if content == "AGENTS context"
        ));
        assert!(matches!(
            &persisted[1],
            SessionRecord::PromptContext {
                role: Role::Developer,
                content,
                ..
            } if content == "Environment context"
        ));
        drop(persisted);

        assert!(streamed.lock().unwrap().is_empty());
    }

    #[test]
    fn skill_activation_records_persist_without_streaming() {
        let persisted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let persisted_clone = persisted.clone();
        let streamed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let streamed_clone = streamed.clone();

        let mut agent = test_agent()
            .with_persist_callback(move |record| {
                persisted_clone.lock().unwrap().push(record.clone());
                Ok(())
            })
            .with_streaming_json(move |json| {
                streamed_clone.lock().unwrap().push(json.to_string());
            });
        let record = SessionRecord::SkillActivated {
            session_id: agent.session_id.to_string(),
            task_id: agent.task_id.to_string(),
            timestamp: chrono::Utc::now(),
            name: "debugging-cake".to_string(),
            path: PathBuf::from("/work/.agents/skills/debugging-cake/SKILL.md"),
        };

        agent.persist_record(&record).unwrap();

        assert!(matches!(
            persisted.lock().unwrap().first(),
            Some(SessionRecord::SkillActivated { name, .. }) if name == "debugging-cake"
        ));
        assert!(streamed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn skill_read_success_marks_active_and_reports_activation() {
        let dir = TempDir::new().unwrap();
        let skill_path = dir.path().join("SKILL.md");
        std::fs::write(
            &skill_path,
            "---\nname: test-skill\ndescription: Test skill\n---\n\nskill instructions",
        )
        .unwrap();
        let skill_path = skill_path.canonicalize().unwrap();
        let skill_locations = HashMap::from([(
            skill_path.clone(),
            test_skill("test-skill", skill_path.clone()),
        )]);
        let skill_activations = Arc::new(Mutex::new(SkillActivations::default()));
        let arguments = serde_json::json!({ "path": skill_path }).to_string();

        let result = execute_tool_with_skill_dedup(
            &default_tool_registry(),
            Arc::new(ToolContext::from_current_process()),
            "Read",
            &arguments,
            &skill_locations,
            &skill_activations,
        )
        .await
        .unwrap();

        assert!(result.output.contains("skill instructions"));
        assert!(!result.output.contains("name: test-skill"));
        assert!(matches!(
            result.skill_activation,
            Some(SkillActivation { name, path }) if name == "test-skill" && path == skill_path
        ));
        assert!(
            skill_activations
                .lock()
                .unwrap()
                .active
                .contains("test-skill")
        );
    }

    #[tokio::test]
    async fn concurrent_duplicate_skill_reads_only_activate_once() {
        let dir = TempDir::new().unwrap();
        let skill_path = dir.path().join("SKILL.md");
        std::fs::write(
            &skill_path,
            "---\nname: test-skill\ndescription: Test skill\n---\n\nskill instructions",
        )
        .unwrap();
        let skill_path = skill_path.canonicalize().unwrap();
        let skill_locations = HashMap::from([(
            skill_path.clone(),
            test_skill("test-skill", skill_path.clone()),
        )]);
        let skill_activations = Arc::new(Mutex::new(SkillActivations::default()));
        let arguments = serde_json::json!({ "path": skill_path }).to_string();
        let registry = default_tool_registry();
        let context = Arc::new(ToolContext::from_current_process());

        let (first, second) = tokio::join!(
            execute_tool_with_skill_dedup(
                &registry,
                Arc::clone(&context),
                "Read",
                &arguments,
                &skill_locations,
                &skill_activations,
            ),
            execute_tool_with_skill_dedup(
                &registry,
                Arc::clone(&context),
                "Read",
                &arguments,
                &skill_locations,
                &skill_activations,
            )
        );

        let results = [first.unwrap(), second.unwrap()];
        assert_eq!(
            results
                .iter()
                .filter(|result| result.output.contains("skill instructions"))
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| {
                    result.output.contains("activation is already in progress")
                        || result.output.contains("already active in this session")
                })
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| result.skill_activation.is_some())
                .count(),
            1
        );
        assert!(
            skill_activations
                .lock()
                .unwrap()
                .active
                .contains("test-skill")
        );
    }

    #[tokio::test]
    async fn active_skill_read_does_not_reload_body() {
        let dir = TempDir::new().unwrap();
        let skill_path = dir.path().join("SKILL.md");
        std::fs::write(
            &skill_path,
            "---\nname: test-skill\ndescription: Test skill\n---\n\nskill instructions",
        )
        .unwrap();
        let skill_path = skill_path.canonicalize().unwrap();
        let skill_locations = HashMap::from([(
            skill_path.clone(),
            test_skill("test-skill", skill_path.clone()),
        )]);
        let skill_activations = Arc::new(Mutex::new(SkillActivations::default()));
        let arguments = serde_json::json!({ "path": skill_path }).to_string();
        let registry = default_tool_registry();
        let context = Arc::new(ToolContext::from_current_process());

        let first = execute_tool_with_skill_dedup(
            &registry,
            Arc::clone(&context),
            "Read",
            &arguments,
            &skill_locations,
            &skill_activations,
        )
        .await
        .unwrap();
        std::fs::write(&skill_path, b"\xFF").unwrap();

        let second = execute_tool_with_skill_dedup(
            &registry,
            context,
            "Read",
            &arguments,
            &skill_locations,
            &skill_activations,
        )
        .await
        .unwrap();

        assert!(first.output.contains("skill instructions"));
        assert!(second.output.contains("already active in this session"));
        assert!(second.skill_activation.is_none());
    }

    #[tokio::test]
    async fn failed_skill_read_does_not_mark_active() {
        let dir = TempDir::new().unwrap();
        let skill_path = dir.path().join("SKILL.md");
        std::fs::write(
            &skill_path,
            b"---\nname: binary-skill\ndescription: Binary skill\n---\n\n\xFF",
        )
        .unwrap();
        let skill_path = skill_path.canonicalize().unwrap();
        let skill_locations = HashMap::from([(
            skill_path.clone(),
            test_skill("binary-skill", skill_path.clone()),
        )]);
        let skill_activations = Arc::new(Mutex::new(SkillActivations::default()));
        let arguments = serde_json::json!({ "path": skill_path }).to_string();

        let error = execute_tool_with_skill_dedup(
            &default_tool_registry(),
            Arc::new(ToolContext::from_current_process()),
            "Read",
            &arguments,
            &skill_locations,
            &skill_activations,
        )
        .await
        .unwrap_err();

        assert!(error.contains("Failed to load skill"));
        assert!(
            !skill_activations
                .lock()
                .unwrap()
                .active
                .contains("binary-skill")
        );
        assert!(
            !skill_activations
                .lock()
                .unwrap()
                .pending
                .contains("binary-skill")
        );
    }

    #[test]
    fn builder_with_session_id() {
        let id = uuid::uuid!("6ba7b810-9dad-11d1-80b4-00c04fd430c8");
        let agent = test_agent().with_session_id(id);
        assert_eq!(agent.session_id, id);
    }

    #[test]
    fn builder_with_history() {
        let history = vec![ConversationItem::Message {
            role: Role::User,
            content: "hi".to_string(),
            id: None,
            status: None,
            timestamp: None,
        }];
        let agent = test_agent().with_history(history).unwrap();
        // 1 system message (from test_agent) + 1 user message from with_history
        assert_eq!(agent.history().len(), 2);
        assert!(matches!(
            &agent.history()[0],
            ConversationItem::Message {
                role: Role::System,
                ..
            }
        ));
        assert!(matches!(
            &agent.history()[1],
            ConversationItem::Message {
                role: Role::User,
                ..
            }
        ));
    }

    #[test]
    fn stream_item_emits_function_call_output() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured_clone = captured.clone();

        let mut agent = test_agent().with_streaming_json(move |json| {
            captured_clone.lock().unwrap().push(json.to_string());
        });

        let item = ConversationItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: "hello world".to_string(),
            timestamp: None,
        };

        agent.stream_item(&item).unwrap();

        drop(agent);
        let messages: Vec<serde_json::Value> = captured
            .lock()
            .unwrap()
            .iter()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["type"], "function_call_output");
        assert_eq!(messages[0]["call_id"], "call-1");
        assert_eq!(messages[0]["output"], "hello world");
    }
}

/// Error handling tests using wiremock for HTTP mocking
#[cfg(test)]
mod error_tests {
    use super::*;
    use crate::config::hooks::{HookCommand, HookEvent, HookGroup, HookMatcher, LoadedHooks};
    use crate::config::model::ApiType;
    use crate::hooks::{HookContext, HookRunner};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

    /// Create a test agent configured to use the Responses API with a mock server URL
    fn test_agent_with_url(base_url: &str) -> Agent {
        test_agent_for(ApiType::Responses, base_url)
    }

    /// Create a test agent configured to use the Chat Completions API with a mock server URL
    fn test_agent_chat_completions(base_url: &str) -> Agent {
        test_agent_for(ApiType::ChatCompletions, base_url)
    }

    /// Create a successful Responses API response
    fn success_response() -> serde_json::Value {
        serde_json::json!({
            "id": "resp-123",
            "output": [
                {
                    "type": "message",
                    "id": "msg-1",
                    "status": "completed",
                    "content": [
                        {
                            "type": "output_text",
                            "text": "Hello!"
                        }
                    ]
                }
            ],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "total_tokens": 15
            }
        })
    }

    /// Create a successful Chat Completions API response
    fn success_chat_response() -> serde_json::Value {
        serde_json::json!({
            "id": "chatcmpl-123",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Hello!"
                    },
                    "finish_reason": "stop"
                }
            ],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        })
    }

    fn tool_call_response() -> serde_json::Value {
        serde_json::json!({
            "id": "resp-tool",
            "output": [
                {
                    "type": "function_call",
                    "id": "fc-1",
                    "call_id": "call-1",
                    "name": "Bash",
                    "arguments": "{\"command\":\"printf unsafe\"}"
                }
            ],
            "usage": {
                "input_tokens": 1,
                "output_tokens": 1,
                "total_tokens": 2
            }
        })
    }

    fn loop_tool_call_response(read_arguments: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "resp-tool",
            "output": [
                {
                    "type": "function_call",
                    "id": "fc-1",
                    "call_id": "call-1",
                    "name": "Read",
                    "arguments": read_arguments
                }
            ],
            "usage": {
                "input_tokens": 3,
                "output_tokens": 2,
                "total_tokens": 5
            }
        })
    }

    fn loop_final_response() -> serde_json::Value {
        serde_json::json!({
            "id": "resp-final",
            "output": [
                {
                    "type": "message",
                    "id": "msg-final",
                    "status": "completed",
                    "content": [
                        {
                            "type": "output_text",
                            "text": "done"
                        }
                    ]
                }
            ],
            "usage": {
                "input_tokens": 4,
                "output_tokens": 1,
                "total_tokens": 5
            }
        })
    }

    #[derive(Debug)]
    struct FunctionCallOutputMatcher {
        call_id: String,
        output: String,
    }

    impl Match for FunctionCallOutputMatcher {
        fn matches(&self, request: &Request) -> bool {
            let Ok(body) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
                return false;
            };

            body["input"].as_array().is_some_and(|items| {
                items.iter().any(|item| {
                    item["type"] == "function_call_output"
                        && item["call_id"] == self.call_id
                        && item["output"] == self.output
                })
            })
        }
    }

    struct LoopFixture {
        _dir: tempfile::TempDir,
        read_arguments: String,
        expected_tool_output: String,
    }

    fn loop_fixture() -> LoopFixture {
        let fixture_dir = tempfile::TempDir::new_in(std::env::current_dir().unwrap()).unwrap();
        let fixture_path = fixture_dir.path().join("loop-input.txt");
        std::fs::write(&fixture_path, "alpha\nbeta\ngamma\n").unwrap();
        let read_arguments = serde_json::json!({
            "path": fixture_path,
            "start_line": 1,
            "end_line": 2
        })
        .to_string();
        let expected_tool_output = format!(
            "File: {}\nLines 1-2/3\n     1: alpha\n     2: beta\n[... 1 more lines ...]",
            fixture_path.display()
        );

        LoopFixture {
            _dir: fixture_dir,
            read_arguments,
            expected_tool_output,
        }
    }

    async fn mount_agent_loop_mocks(
        mock_server: &MockServer,
        read_arguments: &str,
        expected_tool_output: &str,
    ) {
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(body_partial_json(serde_json::json!({
                "input": [
                    {
                        "type": "message",
                        "role": "user",
                        "content": [
                            {
                                "type": "input_text",
                                "text": "run a command"
                            }
                        ]
                    }
                ]
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(loop_tool_call_response(read_arguments)),
            )
            .expect(1)
            .up_to_n_times(1)
            .mount(mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(FunctionCallOutputMatcher {
                call_id: "call-1".to_string(),
                output: expected_tool_output.to_string(),
            })
            .respond_with(ResponseTemplate::new(200).set_body_json(loop_final_response()))
            .expect(1)
            .mount(mock_server)
            .await;
    }

    fn assert_agent_loop_history(agent: &Agent, read_arguments: &str, expected_tool_output: &str) {
        assert_eq!(
            agent
                .history()
                .iter()
                .map(|item| match item {
                    ConversationItem::Message { role, .. } => role.as_str(),
                    ConversationItem::FunctionCall { .. } => "function_call",
                    ConversationItem::FunctionCallOutput { .. } => "function_call_output",
                    ConversationItem::Reasoning { .. } => "reasoning",
                })
                .collect::<Vec<_>>(),
            vec![
                "system",
                "user",
                "function_call",
                "function_call_output",
                "assistant",
            ]
        );
        assert!(matches!(
            &agent.history()[2],
            ConversationItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            } if call_id == "call-1" && name == "Read" && arguments == read_arguments
        ));
        assert!(matches!(
            &agent.history()[3],
            ConversationItem::FunctionCallOutput {
                call_id,
                output,
                ..
            } if call_id == "call-1" && output == expected_tool_output
        ));
    }

    fn stream_records(streamed: &Arc<Mutex<Vec<String>>>) -> Vec<serde_json::Value> {
        let streamed = streamed.lock().unwrap();
        streamed
            .iter()
            .map(|json| serde_json::from_str::<serde_json::Value>(json).unwrap())
            .collect()
    }

    fn assert_agent_loop_stream_records(stream_records: &[serde_json::Value]) {
        assert!(
            stream_records
                .iter()
                .any(|record| record["type"] == "function_call"
                    && record["call_id"] == "call-1"
                    && record["name"] == "Read")
        );
        assert!(stream_records.iter().any(|record| {
            record["type"] == "function_call_output"
                && record["call_id"] == "call-1"
                && record["output"]
                    .as_str()
                    .is_some_and(|output| output.contains("alpha"))
        }));
        assert!(
            stream_records
                .iter()
                .any(|record| record["type"] == "message"
                    && record["role"] == "assistant"
                    && record["content"] == "done")
        );
    }

    #[tokio::test]
    async fn agent_loop_executes_tool_and_continues_to_final_response() {
        let mock_server = MockServer::start().await;
        let fixture = loop_fixture();
        mount_agent_loop_mocks(
            &mock_server,
            &fixture.read_arguments,
            &fixture.expected_tool_output,
        )
        .await;

        let streamed = Arc::new(Mutex::new(Vec::new()));
        let streamed_clone = Arc::clone(&streamed);
        let mut agent = test_agent_with_url(&mock_server.uri()).with_streaming_json(move |json| {
            streamed_clone.lock().unwrap().push(json.to_string());
        });
        agent.tools = crate::clients::tools::read_tool_registry();

        let result = agent
            .send(Message {
                role: Role::User,
                content: "run a command".to_string(),
            })
            .await
            .unwrap();

        assert!(matches!(result, Some(Message { content, .. }) if content == "done"));
        assert_eq!(agent.turn_count, 2);
        assert_eq!(agent.total_usage.input_tokens, 7);
        assert_eq!(agent.total_usage.output_tokens, 3);
        assert_eq!(agent.total_usage.total_tokens, 10);
        assert_agent_loop_history(
            &agent,
            &fixture.read_arguments,
            &fixture.expected_tool_output,
        );
        assert_agent_loop_stream_records(&stream_records(&streamed));
    }

    #[tokio::test]
    async fn pre_tool_hook_denies_tool_execution() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(tool_call_response()))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
            .mount(&mock_server)
            .await;

        let tmp = tempfile::TempDir::new().unwrap();
        let source_path = tmp.path().join("hooks.json");
        let loaded = LoadedHooks {
            groups: vec![HookGroup {
                source_path: source_path.clone(),
                event: HookEvent::PreToolUse,
                matcher: HookMatcher::All,
                hooks: vec![HookCommand {
                    command: "echo blocked >&2; exit 2".to_string(),
                    timeout: Duration::from_secs(2),
                    fail_closed: false,
                    status_message: None,
                    source_path,
                }],
            }],
        };

        let runner = Arc::new(HookRunner::new(
            loaded,
            HookContext {
                session_id: uuid::Uuid::new_v4(),
                task_id: uuid::Uuid::new_v4(),
                transcript_path: None,
                cwd: tmp.path().to_path_buf(),
                model: "test-model".to_string(),
            },
        ));
        let mut agent = test_agent_with_url(&mock_server.uri()).with_hook_runner(runner);

        let result = agent
            .send(Message {
                role: Role::User,
                content: "run a command".to_string(),
            })
            .await
            .unwrap();

        assert!(matches!(result, Some(Message { content, .. }) if content == "Hello!"));
        assert!(agent.history().iter().any(|item| matches!(
            item,
            ConversationItem::FunctionCallOutput { output, .. }
                if output.starts_with("Hook blocked tool execution:")
                    && output.contains("blocked")
        )));
    }

    // =========================================================================
    // HTTP Error Response Tests (Non-retryable 4xx errors)
    // =========================================================================

    #[tokio::test]
    async fn test_400_bad_request_returns_error() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": {
                    "message": "Invalid request: missing required field",
                    "type": "invalid_request_error"
                }
            })))
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_with_url(&mock_server.uri());
        agent.history_mut().push(ConversationItem::Message {
            role: Role::User,
            content: "test".to_string(),
            id: None,
            status: None,
            timestamp: None,
        });

        let result = agent.complete_turn().await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("test-model"));
    }

    #[tokio::test]
    async fn test_401_unauthorized_returns_error() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": {
                    "message": "Invalid API key",
                    "type": "authentication_error"
                }
            })))
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_with_url(&mock_server.uri());
        agent.history_mut().push(ConversationItem::Message {
            role: Role::User,
            content: "test".to_string(),
            id: None,
            status: None,
            timestamp: None,
        });

        let result = agent.complete_turn().await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("test-model"));
    }

    #[tokio::test]
    async fn test_403_forbidden_returns_error() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": {
                    "message": "Access denied",
                    "type": "permission_error"
                }
            })))
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_with_url(&mock_server.uri());
        agent.history_mut().push(ConversationItem::Message {
            role: Role::User,
            content: "test".to_string(),
            id: None,
            status: None,
            timestamp: None,
        });

        let result = agent.complete_turn().await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("test-model"));
    }

    #[tokio::test]
    async fn test_404_not_found_returns_error() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": {
                    "message": "Model not found",
                    "type": "not_found_error"
                }
            })))
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_with_url(&mock_server.uri());
        agent.history_mut().push(ConversationItem::Message {
            role: Role::User,
            content: "test".to_string(),
            id: None,
            status: None,
            timestamp: None,
        });

        let result = agent.complete_turn().await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("test-model"));
    }

    // =========================================================================
    // Retry Logic Tests (5xx and 429 errors should retry)
    // =========================================================================

    #[tokio::test]
    async fn test_429_too_many_requests_retries_and_succeeds() {
        let mock_server = MockServer::start().await;

        // First request returns 429
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
                "error": {
                    "message": "Rate limit exceeded",
                    "type": "rate_limit_error"
                }
            })))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        // Second request succeeds
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_with_url(&mock_server.uri());
        agent.history_mut().push(ConversationItem::Message {
            role: Role::User,
            content: "test".to_string(),
            id: None,
            status: None,
            timestamp: None,
        });

        let result = agent.complete_turn().await;
        assert!(result.is_ok());
        let turn_result = result.unwrap();
        assert_eq!(turn_result.items.len(), 1);
    }

    #[tokio::test]
    async fn test_429_retry_after_header_is_honored() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("Retry-After", "1")
                    .set_body_json(serde_json::json!({
                        "error": {
                            "message": "Rate limit exceeded",
                            "type": "rate_limit_error"
                        }
                    })),
            )
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
            .mount(&mock_server)
            .await;

        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = Arc::clone(&captured);
        let mut agent =
            test_agent_with_url(&mock_server.uri()).with_retry_callback(move |status| {
                captured_clone.lock().unwrap().push(status.clone());
            });
        agent.history_mut().push(ConversationItem::Message {
            role: Role::User,
            content: "test".to_string(),
            id: None,
            status: None,
            timestamp: None,
        });

        let start = Instant::now();
        let result = agent.complete_turn().await;
        let elapsed = start.elapsed();

        assert!(result.is_ok());
        assert!(elapsed >= Duration::from_millis(900));
        let status = {
            let statuses = captured.lock().unwrap();
            assert_eq!(statuses.len(), 1);
            statuses[0].clone()
        };
        assert_eq!(status.delay, Duration::from_secs(1));
        assert_eq!(status.detail, "429 rate limit");
        assert_eq!(status.attempt, 2);
    }

    #[tokio::test]
    async fn test_500_internal_server_error_retries_and_succeeds() {
        let mock_server = MockServer::start().await;

        // First request returns 500
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
                "error": {
                    "message": "Internal server error",
                    "type": "server_error"
                }
            })))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        // Second request succeeds
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_with_url(&mock_server.uri());
        agent.history_mut().push(ConversationItem::Message {
            role: Role::User,
            content: "test".to_string(),
            id: None,
            status: None,
            timestamp: None,
        });

        let result = agent.complete_turn().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_502_bad_gateway_retries_and_succeeds() {
        let mock_server = MockServer::start().await;

        // First request returns 502
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(502).set_body_json(serde_json::json!({
                "error": {
                    "message": "Bad gateway",
                    "type": "bad_gateway"
                }
            })))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        // Second request succeeds
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_with_url(&mock_server.uri());
        agent.history_mut().push(ConversationItem::Message {
            role: Role::User,
            content: "test".to_string(),
            id: None,
            status: None,
            timestamp: None,
        });

        let result = agent.complete_turn().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_503_service_unavailable_retries_and_succeeds() {
        let mock_server = MockServer::start().await;

        // First request returns 503
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(503).set_body_json(serde_json::json!({
                "error": {
                    "message": "Service temporarily unavailable",
                    "type": "service_unavailable"
                }
            })))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        // Second request succeeds
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_with_url(&mock_server.uri());
        agent.history_mut().push(ConversationItem::Message {
            role: Role::User,
            content: "test".to_string(),
            id: None,
            status: None,
            timestamp: None,
        });

        let result = agent.complete_turn().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_503_x_should_retry_false_returns_error() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(
                ResponseTemplate::new(503)
                    .insert_header("x-should-retry", "false")
                    .set_body_json(serde_json::json!({
                        "error": {
                            "message": "Service temporarily unavailable",
                            "type": "server_error"
                        }
                    })),
            )
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_with_url(&mock_server.uri());
        agent.history_mut().push(ConversationItem::Message {
            role: Role::User,
            content: "test".to_string(),
            id: None,
            status: None,
            timestamp: None,
        });

        let result = agent.complete_turn().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_529_overloaded_retries_and_succeeds() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(529).set_body_json(serde_json::json!({
                "error": {
                    "message": "Provider overloaded",
                    "type": "server_error"
                }
            })))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_with_url(&mock_server.uri());
        agent.history_mut().push(ConversationItem::Message {
            role: Role::User,
            content: "test".to_string(),
            id: None,
            status: None,
            timestamp: None,
        });

        let result = agent.complete_turn().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_overloaded_error_body_retries_and_succeeds() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
                "error": {
                    "message": "provider overloaded",
                    "type": "overloaded_error"
                }
            })))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_with_url(&mock_server.uri());
        agent.history_mut().push(ConversationItem::Message {
            role: Role::User,
            content: "test".to_string(),
            id: None,
            status: None,
            timestamp: None,
        });

        let result = agent.complete_turn().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_504_gateway_timeout_retries_and_succeeds() {
        let mock_server = MockServer::start().await;

        // First request returns 504
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(504).set_body_json(serde_json::json!({
                "error": {
                    "message": "Gateway timeout",
                    "type": "gateway_timeout"
                }
            })))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        // Second request succeeds
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_with_url(&mock_server.uri());
        agent.history_mut().push(ConversationItem::Message {
            role: Role::User,
            content: "test".to_string(),
            id: None,
            status: None,
            timestamp: None,
        });

        let result = agent.complete_turn().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_max_retries_exceeded_returns_error() {
        let mock_server = MockServer::start().await;

        // All requests return 429 (exceeds MAX_RETRIES)
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
                "error": {
                    "message": "Rate limit exceeded",
                    "type": "rate_limit_error"
                }
            })))
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_with_url(&mock_server.uri());
        agent.history_mut().push(ConversationItem::Message {
            role: Role::User,
            content: "test".to_string(),
            id: None,
            status: None,
            timestamp: None,
        });

        let result = agent.complete_turn().await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("test-model"));
    }

    #[tokio::test]
    async fn test_context_overflow_reduces_max_output_tokens_once() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(body_partial_json(serde_json::json!({
                "max_output_tokens": 5000,
                "reasoning": {
                    "max_tokens": 4000
                }
            })))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": {
                    "message": "input length and max_tokens exceed context limit: 12000 + 5000 > 16384",
                    "type": "invalid_request_error"
                }
            })))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(body_partial_json(serde_json::json!({
                "max_output_tokens": 3360,
                "reasoning": {
                    "max_tokens": 3359
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_with_url(&mock_server.uri());
        agent.config.model_config.max_output_tokens = Some(5000);
        agent.config.model_config.reasoning_max_tokens = Some(4000);
        agent.history_mut().push(ConversationItem::Message {
            role: Role::User,
            content: "test".to_string(),
            id: None,
            status: None,
            timestamp: None,
        });

        let result = agent.complete_turn().await;
        assert!(result.is_ok());
    }

    // =========================================================================
    // Chat Completions API Error Tests
    // =========================================================================

    #[tokio::test]
    async fn test_chat_completions_400_bad_request_returns_error() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": {
                    "message": "Invalid request",
                    "type": "invalid_request_error"
                }
            })))
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_chat_completions(&mock_server.uri());
        agent.history_mut().push(ConversationItem::Message {
            role: Role::User,
            content: "test".to_string(),
            id: None,
            status: None,
            timestamp: None,
        });

        let result = agent.complete_turn().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_chat_completions_429_retries_and_succeeds() {
        let mock_server = MockServer::start().await;

        // First request returns 429
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
                "error": {
                    "message": "Rate limit exceeded",
                    "type": "rate_limit_error"
                }
            })))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        // Second request succeeds
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_chat_response()))
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_chat_completions(&mock_server.uri());
        agent.history_mut().push(ConversationItem::Message {
            role: Role::User,
            content: "test".to_string(),
            id: None,
            status: None,
            timestamp: None,
        });

        let result = agent.complete_turn().await;
        assert!(result.is_ok());
    }

    // =========================================================================
    // Successful Response Tests
    // =========================================================================

    #[tokio::test]
    async fn test_successful_responses_api_call() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_with_url(&mock_server.uri());
        agent.history_mut().push(ConversationItem::Message {
            role: Role::User,
            content: "Hello".to_string(),
            id: None,
            status: None,
            timestamp: None,
        });

        let result = agent.complete_turn().await;
        assert!(result.is_ok());
        let turn_result = result.unwrap();
        assert_eq!(turn_result.items.len(), 1);
        assert!(matches!(&turn_result.items[0], ConversationItem::Message {
            role: Role::Assistant,
            content,
            ..
        } if content == "Hello!"));
        assert!(turn_result.usage.is_some());
        let usage = turn_result.usage.unwrap();
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 5);
    }

    #[tokio::test]
    async fn test_successful_chat_completions_api_call() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_chat_response()))
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_chat_completions(&mock_server.uri());
        agent.history_mut().push(ConversationItem::Message {
            role: Role::User,
            content: "Hello".to_string(),
            id: None,
            status: None,
            timestamp: None,
        });

        let result = agent.complete_turn().await;
        assert!(result.is_ok());
        let turn_result = result.unwrap();
        assert_eq!(turn_result.items.len(), 1);
        assert!(matches!(&turn_result.items[0], ConversationItem::Message {
            role: Role::Assistant,
            content,
            ..
        } if content == "Hello!"));
    }
}
