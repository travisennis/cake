use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::clients::agent::{Agent, TurnResult, record_turn_usage};
use crate::clients::backend::FinalOutputConstraint;
use crate::clients::retry::{RequestOverrides, RetryReason, RetryStatus};
use crate::clients::tools::{
    ScheduledToolCall, Tool, ToolError, ToolRegistry, ToolResult, argument_compensation_events,
    read_extract_path, schedule_tool_calls,
};
use crate::config::output_schema::{OutputSchema, OutputSchemaError};
use crate::hooks::{HookRunner, ToolHookPlan};
use crate::session_telemetry::{
    AgentRunnerTelemetryEvent, CompensationEventTelemetry, CompensationKind,
    RetryScheduledTelemetry, TerminationClassification, ToolCallTelemetry,
};
use crate::types::{ConversationItem, CutOffError, LimitExceededError, SessionRecord};

/// Maximum number of corrective turns after a final message fails
/// output-schema validation.
const MAX_SCHEMA_CORRECTION_TURNS: u32 = 2;

/// The `max_turns` settings key.
const LIMIT_MAX_TURNS: &str = "max_turns";

/// The `max_tool_calls` settings key.
const LIMIT_MAX_TOOL_CALLS: &str = "max_tool_calls";

pub(super) const SEMANTIC_RECOVERY_PROMPT: &str = "Your previous response ended before providing a final \
answer. Continue from the existing conversation and provide the final answer now. Do not repeat \
completed tool calls or redo completed work.";

type FunctionCall = (String, String, String);

struct TurnContext<'a> {
    turn_mode: &'a mut TurnMode,
    corrections_used: &'a mut u32,
    semantic_recovery_used: &'a mut bool,
    provider_turn_start: usize,
    invocation_start: usize,
    termination: Option<&'a crate::session_telemetry::ProviderTermination>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnMode {
    Normal,
    SchemaCorrection,
    SemanticRecovery,
}

impl TurnMode {
    const fn tools_disabled(self) -> bool {
        !matches!(self, Self::Normal)
    }
}

#[derive(Debug, Clone)]
struct ToolRunResult {
    call_id: String,
    output: String,
    skill_activation: Option<SkillActivation>,
    telemetry: ToolCallTelemetry,
    compensation_events: Vec<CompensationEventTelemetry>,
}

/// Result of checking whether a Read tool call targeted a known skill path.
#[derive(Debug, Clone)]
struct SkillActivation {
    name: String,
    path: PathBuf,
}

/// The permission-denial label for a judge compensation event, when the event
/// records a denied command (#123).
///
/// A `block` verdict carries its verdict code (`block:<code>` in `detail`) and
/// a fail-closed denial carries its failure class. `warn` and `allow` verdicts,
/// bypasses, and allowlist-overridden blocks are not denials --- the command
/// ran --- so they map to `None`. This keeps the `task_complete` denials
/// distinct from hook denials (`{name}({call_id}): blocked by hook`) while
/// carrying the same stable code the telemetry `compensation` record uses.
pub(super) fn judge_denial_label(event: &CompensationEventTelemetry) -> Option<String> {
    match event.kind {
        CompensationKind::JudgeVerdict => {
            if event.overridden == Some(true) {
                return None;
            }
            let code = event.detail.as_deref()?.strip_prefix("block:")?;
            Some(format!("judge block: {code}"))
        },
        CompensationKind::JudgeFailClosed => {
            let class = event.detail.as_deref()?;
            Some(format!("judge fail-closed: {class}"))
        },
        _ => None,
    }
}

/// Build a synchronous error `ToolRunResult` (no tool execution, immediate).
fn immediate_tool_error_result(
    name: &str,
    call_id: &str,
    output: String,
    turn_index: u32,
) -> ToolRunResult {
    ToolRunResult {
        telemetry: ToolCallTelemetry {
            turn_index,
            call_id: call_id.to_string(),
            name: name.to_string(),
            duration_ms: 0,
            output_bytes: output.len(),
            was_error: true,
        },
        call_id: call_id.to_string(),
        output,
        skill_activation: None,
        compensation_events: Vec::new(),
    }
}

impl Agent {
    /// Log the remaining context-window budget after a completed turn, when a
    /// window is configured. Factored into its own method so context accounting
    /// stays separate from the main turn-control flow.
    pub(super) fn log_context_budget(&self) {
        if let Some(remaining) = self.context_remaining_tokens() {
            tracing::info!(
                target: "cake",
                turn = self.turn_count,
                window = self.context_window(),
                context_tokens = self.last_usage().map(|usage| usage.input_tokens),
                remaining_context_tokens = remaining,
                "Context window budget remaining after turn"
            );
        }
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
    pub async fn send(&mut self, content: String) -> anyhow::Result<String> {
        // Items at or after this index belong to the current invocation; a
        // resumed session's prior tasks must never contribute to this task's
        // partial result.
        let invocation_start = self.conversation.history().len();
        let user_item = self.conversation.push_user_message(content);
        self.stream_item(&user_item)?;

        // Output-schema correction state: when the final message fails
        // validation, the loop re-enters with tools disabled for at most
        // MAX_SCHEMA_CORRECTION_TURNS corrective turns.
        let mut corrections_used: u32 = 0;
        let mut turn_mode = TurnMode::Normal;
        let mut semantic_recovery_used = false;

        // Agent loop: continue until model stops making tool calls
        loop {
            // Stop before starting another turn when a user-configured
            // limit has already been consumed (for example after a
            // schema-correction continue).
            self.enforce_limits(0, invocation_start)?;

            let provider_turn_start = self.conversation.history().len();
            let TurnResult {
                items, termination, ..
            } = self
                .complete_turn_with_output_schema_fallback(turn_mode)
                .await?;

            let function_calls = self.process_completed_turn(items)?;
            let turn_context = TurnContext {
                turn_mode: &mut turn_mode,
                corrections_used: &mut corrections_used,
                semantic_recovery_used: &mut semantic_recovery_used,
                provider_turn_start,
                invocation_start,
                termination: termination.as_ref(),
            };
            if let Some(document) = self.process_turn(function_calls, turn_context).await? {
                return Ok(document);
            }
        }
    }

    fn process_completed_turn(
        &mut self,
        items: Vec<ConversationItem>,
    ) -> anyhow::Result<Vec<FunctionCall>> {
        // Count every completed API turn unconditionally. Usage is settled by
        // AgentRunner before it classifies or retries the provider attempt.
        self.turn_count += 1;
        self.log_context_budget();

        // Extract owned function call data before moving items into history.
        let function_calls = Self::function_calls_from_items(&items);
        self.stream_turn_items(&items)?;
        self.conversation.extend_turn_items(items);
        Ok(function_calls)
    }

    async fn process_turn(
        &mut self,
        function_calls: Vec<FunctionCall>,
        context: TurnContext<'_>,
    ) -> anyhow::Result<Option<String>> {
        if function_calls.is_empty() {
            return self.handle_final_message(
                context.corrections_used,
                context.turn_mode,
                context.semantic_recovery_used,
                context.provider_turn_start,
                context.termination,
            );
        }

        if context.turn_mode.tools_disabled() {
            self.handle_disabled_tools_turn(
                &function_calls,
                context.corrections_used,
                context.turn_mode,
                context.provider_turn_start,
                context.termination,
            )?;
            return Ok(None);
        }

        self.execute_tool_turn(function_calls, context.invocation_start)
            .await?;
        Ok(None)
    }

    fn handle_final_message(
        &mut self,
        corrections_used: &mut u32,
        turn_mode: &mut TurnMode,
        semantic_recovery_used: &mut bool,
        provider_turn_start: usize,
        termination: Option<&crate::session_telemetry::ProviderTermination>,
    ) -> anyhow::Result<Option<String>> {
        let message = self
            .conversation
            .resolve_assistant_message_from(provider_turn_start);
        if message.is_none() || termination_marks_incomplete(termination) {
            if !*semantic_recovery_used && semantic_incomplete_is_retryable(termination) {
                *semantic_recovery_used = true;
                *turn_mode = TurnMode::SemanticRecovery;
                self.record_semantic_recovery_retry();
                let continuation = self
                    .conversation
                    .push_user_message(SEMANTIC_RECOVERY_PROMPT.to_string());
                self.stream_item(&continuation)?;
                return Ok(None);
            }

            let turn_items = self
                .conversation
                .history()
                .get(provider_turn_start..)
                .unwrap_or_default();
            return Err(cut_off_error(
                turn_items,
                termination,
                semantic_recovery_used.then_some(self.session_id),
            )
            .into());
        }

        if let Some(document) =
            self.resolve_final_message_or_correct(corrections_used, turn_mode, provider_turn_start)?
        {
            return Ok(Some(document));
        }
        Ok(None)
    }

    fn handle_disabled_tools_turn(
        &mut self,
        function_calls: &[FunctionCall],
        corrections_used: &mut u32,
        turn_mode: &mut TurnMode,
        provider_turn_start: usize,
        termination: Option<&crate::session_telemetry::ProviderTermination>,
    ) -> anyhow::Result<()> {
        // Correction turns offer no tools, so any function calls here come
        // from a misbehaving provider. Do not execute them; treat the turn
        // as a failed validation attempt.
        // Append synthetic FunctionCallOutput items for every unexecuted call
        // so the history stays well-formed for both the Responses API and
        // Chat Completions backends.
        for (call_id, name, _) in function_calls {
            let output =
                format!("not executed: correction turn offers no tools for {name}({call_id})");
            let item = self.conversation.push_tool_output(call_id.clone(), output);
            self.stream_item(&item)?;
        }
        if *turn_mode == TurnMode::SchemaCorrection {
            self.record_correction_tool_call_failure(corrections_used, turn_mode)?;
            return Ok(());
        }

        let turn_items = self
            .conversation
            .history()
            .get(provider_turn_start..)
            .unwrap_or_default();
        Err(cut_off_error(turn_items, termination, Some(self.session_id)).into())
    }

    async fn execute_tool_turn(
        &mut self,
        function_calls: Vec<FunctionCall>,
        invocation_start: usize,
    ) -> anyhow::Result<()> {
        // Stop before executing a turn's tool calls when the turn
        // already consumed the turn budget or the batch would exceed the
        // tool-call budget: executing them could never lead to a final
        // answer within the configured limits.
        self.enforce_limits(function_calls.len(), invocation_start)?;
        let results = self.execute_function_calls(function_calls).await?;
        self.record_tool_results(results)?;
        Ok(())
    }

    /// Stop the loop with a [`LimitExceededError`] when a user-configured
    /// limit has been reached.
    fn enforce_limits(
        &self,
        pending_tool_calls: usize,
        invocation_start: usize,
    ) -> anyhow::Result<()> {
        if let Some(error) = self.limit_exceeded_if_reached(pending_tool_calls, invocation_start) {
            return Err(error.into());
        }
        Ok(())
    }

    /// Whether a user-configured limit has been reached, and the error that
    /// stops the loop when it has.
    ///
    /// `pending_tool_calls` is the size of the tool batch the current turn
    /// is about to execute; `0` at the top of the loop, where no batch is
    /// pending. `invocation_start` bounds the partial-result lookup to the
    /// current invocation. The turn limit fires once `turn_count` reaches
    /// it; the tool-call limit fires when executing the pending batch would
    /// push `tool_call_count` past it, so a batch that cannot complete
    /// within the budget is never started.
    fn limit_exceeded_if_reached(
        &self,
        pending_tool_calls: usize,
        invocation_start: usize,
    ) -> Option<LimitExceededError> {
        if let Some(limit) = self.max_turns
            && self.turn_count >= limit
        {
            return Some(self.limit_exceeded_error(
                LIMIT_MAX_TURNS,
                limit,
                self.turn_count,
                invocation_start,
            ));
        }
        if let Some(limit) = self.max_tool_calls {
            let projected = self
                .tool_call_count
                .saturating_add(u32::try_from(pending_tool_calls).unwrap_or(u32::MAX));
            if projected > limit {
                return Some(self.limit_exceeded_error(
                    LIMIT_MAX_TOOL_CALLS,
                    limit,
                    self.tool_call_count,
                    invocation_start,
                ));
            }
        }
        None
    }

    /// Build a [`LimitExceededError`] naming the limit that fired, carrying
    /// the last assistant message of the current invocation (if any) as the
    /// partial result.
    fn limit_exceeded_error(
        &self,
        limit: &str,
        limit_value: u32,
        count: u32,
        invocation_start: usize,
    ) -> LimitExceededError {
        let detail = match limit {
            LIMIT_MAX_TURNS => {
                format!("max_turns limit exceeded after {count} turns (max_turns = {limit_value})")
            },
            LIMIT_MAX_TOOL_CALLS => format!(
                "max_tool_calls limit exceeded after {count} tool calls \
                 (max_tool_calls = {limit_value})"
            ),
            _ => unreachable!("unknown limit kind: {limit}"),
        };
        LimitExceededError::new(
            limit.to_string(),
            detail,
            self.conversation
                .resolve_assistant_message_from(invocation_start),
        )
    }

    async fn execute_function_calls(
        &mut self,
        function_calls: Vec<FunctionCall>,
    ) -> anyhow::Result<Vec<ToolRunResult>> {
        self.tool_call_count += u32::try_from(function_calls.len()).unwrap_or(u32::MAX);
        let tool_plans = self.plan_tool_calls(function_calls).await?;
        self.record_hook_blocked_denials(&tool_plans);
        Ok(self.run_tool_plans(tool_plans).await)
    }

    async fn plan_tool_calls(
        &self,
        function_calls: Vec<FunctionCall>,
    ) -> anyhow::Result<Vec<(String, String, ToolHookPlan)>> {
        let hook_runner = self.hook_runner.clone();
        let pre_futures = function_calls
            .into_iter()
            .map(|(call_id, name, arguments)| {
                let hook_runner = hook_runner.clone();
                // The registry entry declares whether the executor repairs
                // arguments, so the hook sees exactly what will run (#277).
                let repairs_arguments = self.tools.repairs_arguments(&name);
                async move {
                    let plan = if let Some(runner) = hook_runner {
                        runner
                            .pre_tool_use(&name, &call_id, &arguments, repairs_arguments)
                            .await?
                    } else {
                        ToolHookPlan::Execute {
                            arguments,
                            prefix_notice: None,
                            additional_context: Vec::new(),
                        }
                    };
                    anyhow::Ok((call_id, name, plan))
                }
            });
        let pre_results = futures::future::join_all(pre_futures).await;
        let mut tool_plans = Vec::with_capacity(pre_results.len());
        for result in pre_results {
            tool_plans.push(result?);
        }
        Ok(tool_plans)
    }

    fn record_hook_blocked_denials(&mut self, tool_plans: &[(String, String, ToolHookPlan)]) {
        for (call_id, name, plan) in tool_plans {
            if let ToolHookPlan::Block { reason, .. } = plan {
                self.permission_denials
                    .push(format!("{name}({call_id}): {reason}"));
            }
        }
    }

    /// Execute a turn's tool calls.
    ///
    /// Calls mutating the same canonical path run sequentially in issue order
    /// so each sees the previous call's effects; all other calls run
    /// concurrently. Results are returned in the model's issue order
    /// regardless of grouping.
    async fn run_tool_plans(
        &self,
        tool_plans: Vec<(String, String, ToolHookPlan)>,
    ) -> Vec<ToolRunResult> {
        let expected_results = tool_plans.len();
        let groups = schedule_tool_calls(&self.tools, self.tool_context.as_ref(), tool_plans);
        let group_futures = groups.into_iter().map(|group| self.run_group(group));
        let mut results = Vec::with_capacity(expected_results);
        results.extend(
            futures::future::join_all(group_futures)
                .await
                .into_iter()
                .flatten(),
        );
        results.sort_unstable_by_key(|(index, _)| *index);
        results.into_iter().map(|(_, result)| result).collect()
    }

    /// Execute one scheduling group: members run sequentially in issue order,
    /// each seeing the previous call's effects. Every member after the first
    /// is a same-path serialization reordering the model needed compensated,
    /// recorded as a telemetry event.
    async fn run_group(&self, group: Vec<ScheduledToolCall>) -> Vec<(usize, ToolRunResult)> {
        let mut results = Vec::with_capacity(group.len());
        for (position, call) in group.into_iter().enumerate() {
            let mut result = self.run_tool_call(call.call_id, call.name, call.plan).await;
            if position > 0
                && let Some(path) = call.target.as_ref()
            {
                result
                    .compensation_events
                    .push(CompensationEventTelemetry::new(
                        CompensationKind::SamePathSerialization,
                        Some(path.display().to_string()),
                    ));
            }
            results.push((call.index, result));
        }
        results
    }

    async fn run_tool_call(
        &self,
        call_id: String,
        name: String,
        plan: ToolHookPlan,
    ) -> ToolRunResult {
        let turn_index = self.turn_count;
        match plan {
            ToolHookPlan::Block {
                reason,
                additional_context,
            } => {
                let output = format!("Hook blocked tool execution: {reason}");
                let output = append_hook_context(output, &additional_context);
                immediate_tool_error_result(&name, &call_id, output, turn_index)
            },
            ToolHookPlan::Execute {
                arguments,
                prefix_notice,
                additional_context,
            } => {
                let start = Instant::now();
                let result = self
                    .tools
                    .execute(Arc::clone(&self.tool_context), &name, &call_id, &arguments)
                    .await;
                let post_context = post_tool_context_for_result(
                    self.hook_runner.as_ref(),
                    &self.tools,
                    &name,
                    &call_id,
                    &arguments,
                    &result,
                )
                .await;

                let was_error = result.is_err();
                let mut compensation_events;
                let mut output = match result {
                    Ok(result) => {
                        compensation_events = result.compensation_events;
                        result.output
                    },
                    Err(error) => {
                        // Tool errors carry the events observed while the tool
                        // failed (e.g. a judge block verdict or fail-closed
                        // denial), so they still reach session telemetry.
                        compensation_events = error.compensation_events;
                        format!("Error: {}", error.message)
                    },
                };
                // Classify argument-driven compensations centrally: the
                // repair pass and Edit parse are the source of truth, and the
                // events survive calls that fail after a repair. Unregistered
                // tools never reached an argument parser, so they cannot
                // carry argument compensations.
                let registered = self.tools.has(&name);
                compensation_events.extend(argument_compensation_events(
                    &name,
                    &arguments,
                    was_error,
                    registered,
                    self.tools.repairs_arguments(&name),
                ));

                let skill_activation = if was_error {
                    None
                } else {
                    detect_skill_activation_if_configured(
                        &name,
                        &arguments,
                        &self.skill_locations,
                        &self.activated_skills,
                    )
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

                let duration_ms = start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                ToolRunResult {
                    telemetry: ToolCallTelemetry {
                        turn_index,
                        call_id: call_id.clone(),
                        name,
                        duration_ms,
                        output_bytes: output.len(),
                        was_error,
                    },
                    call_id,
                    output,
                    skill_activation,
                    compensation_events,
                }
            },
        }
    }

    fn record_tool_results(&mut self, results: Vec<ToolRunResult>) -> anyhow::Result<()> {
        for result in results {
            self.record_judge_denials(
                &result.telemetry.name,
                &result.call_id,
                &result.compensation_events,
            );
            let replay = Some(self.tools.replay_safety(&result.telemetry.name));
            self.append_tool_call_telemetry(result.telemetry);
            self.record_compensation_events(result.compensation_events);
            if let Some(skill_activation) = result.skill_activation {
                let record = SessionRecord::SkillActivated {
                    session_id: self.session_id.to_string(),
                    task_id: self.task_id.to_string(),
                    timestamp: chrono::Utc::now(),
                    name: skill_activation.name,
                    path: skill_activation.path,
                };
                self.persist_record(&record)?;
            }
            let item = self
                .conversation
                .push_tool_output(result.call_id, result.output);
            self.stream_item_with_replay(&item, replay)?;
        }
        Ok(())
    }

    fn record_compensation_events(&mut self, events: Vec<CompensationEventTelemetry>) {
        for event in events {
            self.append_compensation_telemetry(event);
        }
    }

    /// Record judge blocks and fail-closed denials into `permission_denials`
    /// through the same path hook denials use, so `task_complete` reports
    /// command-safety denials with a distinct label carrying the verdict code
    /// or failure class (#123). The label comes from the compensation events
    /// the Bash preflight recorded on the tool error path, so a denial is
    /// never inferred from a generic tool failure.
    fn record_judge_denials(
        &mut self,
        name: &str,
        call_id: &str,
        events: &[CompensationEventTelemetry],
    ) {
        for event in events {
            let Some(label) = judge_denial_label(event) else {
                continue;
            };
            self.permission_denials
                .push(format!("{name}({call_id}): {label}"));
        }
    }

    async fn complete_turn_with_output_schema_fallback(
        &mut self,
        turn_mode: TurnMode,
    ) -> anyhow::Result<TurnResult> {
        let mut next_attempt = 1;
        loop {
            let constraint_attached = self.native_constraint_attached(turn_mode);
            match self
                .complete_turn_in_mode(turn_mode, &mut next_attempt)
                .await
            {
                Ok(turn) => return Ok(turn),
                Err(error) if constraint_attached && is_native_constraint_rejection(&error) => {
                    self.native_constraint_enabled = false;
                    tracing::warn!(
                        target: "cake",
                        "Provider rejected the native output-schema constraint (HTTP 400); \
                         retrying the correction turn without it"
                    );
                },
                Err(error) => return Err(error),
            }
        }
    }

    const fn native_constraint_attached(&self, turn_mode: TurnMode) -> bool {
        turn_mode.tools_disabled() && self.native_constraint_enabled && self.output_schema.is_some()
    }

    fn resolve_final_message_or_correct(
        &mut self,
        corrections_used: &mut u32,
        turn_mode: &mut TurnMode,
        provider_turn_start: usize,
    ) -> anyhow::Result<Option<String>> {
        let Some(message) = self
            .conversation
            .resolve_assistant_message_from(provider_turn_start)
        else {
            return Ok(None);
        };
        let Some(schema) = self.output_schema.clone() else {
            return Ok(Some(message));
        };
        match validate_final_message(&schema, &message) {
            Ok(document) => Ok(Some(document)),
            Err(detail) => {
                self.push_schema_correction(corrections_used, turn_mode, detail)?;
                Ok(None)
            },
        }
    }

    fn record_correction_tool_call_failure(
        &mut self,
        corrections_used: &mut u32,
        turn_mode: &mut TurnMode,
    ) -> anyhow::Result<()> {
        self.push_schema_correction(
            corrections_used,
            turn_mode,
            "the response contained tool calls instead of a single JSON document".to_string(),
        )
    }

    /// Record a failed output-schema validation attempt.
    ///
    /// Returns a typed [`OutputSchemaError::Unsatisfied`] error once the
    /// correction budget is exhausted; otherwise enters correction mode and
    /// pushes a corrective user message that streams and persists like any
    /// other conversation item.
    fn push_schema_correction(
        &mut self,
        corrections_used: &mut u32,
        turn_mode: &mut TurnMode,
        detail: String,
    ) -> anyhow::Result<()> {
        if *corrections_used >= MAX_SCHEMA_CORRECTION_TURNS {
            return Err(OutputSchemaError::Unsatisfied {
                attempts: corrections_used.saturating_add(1),
                detail,
            }
            .into());
        }
        *corrections_used += 1;
        *turn_mode = TurnMode::SchemaCorrection;
        let corrective = format!(
            "Your previous response failed output schema validation:\n{detail}\n\n\
             Respond with only a single JSON document that validates against \
             the required output schema. Do not include Markdown code fences, \
             commentary, or any other text."
        );
        let item = self.conversation.push_user_message(corrective);
        self.stream_item(&item)?;
        Ok(())
    }

    /// Execute a single API turn with retry logic.
    ///
    /// Correction turns offer no tools — the model must answer with the final
    /// JSON document, not more tool calls — and attach the provider's native
    /// structured-output constraint while it remains enabled for this run.
    async fn complete_turn_in_mode(
        &mut self,
        turn_mode: TurnMode,
        next_attempt: &mut u32,
    ) -> anyhow::Result<TurnResult> {
        let tool_definitions = tool_definitions_for_turn(&self.tools, turn_mode);
        let native_constraint_attached = self.native_constraint_attached(turn_mode);
        let constraint = final_output_constraint_for_turn(
            self.output_schema.as_deref(),
            native_constraint_attached,
        );
        let config = &self.config;
        let session_id = self.session_id;
        let task_id = self.task_id;
        let history = self.conversation.history();
        let turn_index = self.turn_count.saturating_add(1);
        let runner = &self.runner;
        let runner_telemetry = self.runner_telemetry_sink();
        let observer = &mut self.observer;
        let total_usage = &mut self.total_usage;
        let last_usage = &mut self.last_usage;
        runner
            .complete_turn(
                config,
                session_id,
                task_id,
                turn_index,
                history,
                tool_definitions,
                constraint,
                next_attempt,
                |event| {
                    if let Some(sink) = &runner_telemetry {
                        sink.record(event);
                    }
                },
                |settlement| {
                    record_turn_usage(
                        observer,
                        total_usage,
                        last_usage,
                        session_id,
                        task_id,
                        settlement,
                    );
                },
            )
            .await
    }

    #[cfg(test)]
    pub(super) async fn complete_turn(
        &mut self,
        in_correction_mode: bool,
    ) -> anyhow::Result<TurnResult> {
        let turn_mode = if in_correction_mode {
            TurnMode::SchemaCorrection
        } else {
            TurnMode::Normal
        };
        let mut next_attempt = 1;
        self.complete_turn_in_mode(turn_mode, &mut next_attempt)
            .await
    }

    fn record_semantic_recovery_retry(&mut self) {
        let status = RetryStatus {
            attempt: 1,
            max_retries: 1,
            delay: Duration::ZERO,
            reason: RetryReason::SemanticIncomplete,
            detail: "successful provider turn contained no final assistant message".to_string(),
        };
        let request_overrides = RequestOverrides {
            max_output_tokens: self.config.model_config.max_output_tokens,
            reasoning_max_tokens: self.config.model_config.reasoning_max_tokens,
            context_overflow_retry_used: false,
        };
        self.append_runner_telemetry(AgentRunnerTelemetryEvent::RetryScheduled(
            RetryScheduledTelemetry::from_status(
                &status,
                self.turn_count,
                false,
                &request_overrides,
            ),
        ));
        self.report_progress("Retrying incomplete model turn (semantic_incomplete, attempt 1/1)");
        tracing::info!(
            target: "cake",
            reason = ?status.reason,
            detail = %status.detail,
            attempt = status.attempt,
            max_attempts = status.max_retries,
            "Retrying incomplete model turn"
        );
    }

    fn function_calls_from_items(items: &[ConversationItem]) -> Vec<FunctionCall> {
        let mut calls = Vec::with_capacity(items.len());
        calls.extend(items.iter().filter_map(|item| {
            if let ConversationItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            } = item
            {
                Some((call_id.clone(), name.clone(), arguments.clone()))
            } else {
                None
            }
        }));
        calls
    }

    fn stream_turn_items(&mut self, items: &[ConversationItem]) -> anyhow::Result<()> {
        for item in items {
            let replay = match item {
                ConversationItem::FunctionCall { name, .. } => Some(self.tools.replay_safety(name)),
                _ => None,
            };
            self.stream_item_with_replay(item, replay)?;
        }
        Ok(())
    }
}

fn is_native_constraint_rejection(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<crate::exit_code::ApiError>()
        .is_some_and(|api_error| api_error.status == 400)
}

fn tool_definitions_for_turn(tools: &ToolRegistry, turn_mode: TurnMode) -> &[Tool] {
    if turn_mode.tools_disabled() {
        &[]
    } else {
        tools.definitions()
    }
}

fn final_output_constraint_for_turn(
    output_schema: Option<&OutputSchema>,
    native_constraint_attached: bool,
) -> Option<FinalOutputConstraint<'_>> {
    native_constraint_attached
        .then(|| {
            output_schema.map(|schema| FinalOutputConstraint {
                name: &schema.name,
                schema: &schema.raw,
            })
        })
        .flatten()
}

/// Validate a final assistant message against the output schema.
///
/// Returns the trimmed JSON document on success, or a human-readable failure
/// detail. Deliberately strict: a Markdown-fenced document is a parse failure
/// handled by the correction loop, keeping the success contract exact.
fn validate_final_message(schema: &OutputSchema, message: &str) -> Result<String, String> {
    let trimmed = message.trim();
    let instance: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(value) => value,
        Err(error) => {
            return Err(format!(
                "the response is not a single valid JSON document: {error}"
            ));
        },
    };
    schema
        .validation_detail(&instance)
        .map_or_else(|| Ok(trimmed.to_string()), Err)
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

/// Prepare the post-tool hook payload only when a post-tool hook exists.
///
/// Successful tool output is otherwise already owned by the caller, so copying
/// it just to pass an unused hook result would add one allocation per call.
async fn post_tool_context_for_result(
    hook_runner: Option<&Arc<HookRunner>>,
    tools: &ToolRegistry,
    name: &str,
    call_id: &str,
    arguments: &str,
    result: &Result<ToolResult, ToolError>,
) -> Option<String> {
    let runner = hook_runner.filter(|runner| runner.has_matching_post_tool_hook(name))?;
    let hook_result = result
        .as_ref()
        .map(|result| result.output.clone())
        .map_err(|error| error.message.clone());
    post_tool_context(
        Some(runner),
        tools.repairs_arguments(name),
        name,
        call_id,
        arguments,
        &hook_result,
    )
    .await
}

async fn post_tool_context(
    hook_runner: Option<&Arc<HookRunner>>,
    repairs_arguments: bool,
    name: &str,
    call_id: &str,
    arguments: &str,
    hook_result: &Result<String, String>,
) -> Option<String> {
    let runner = hook_runner?;
    match runner
        .post_tool_use(name, call_id, arguments, hook_result, repairs_arguments)
        .await
    {
        Ok(Some(ctx)) => Some(ctx),
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                target: "cake::hooks",
                error = %error,
                tool_name = %name,
                "PostToolUse hook failed (best-effort)"
            );
            None
        },
    }
}

/// Skip the skill activation probe entirely when no skills were discovered.
fn detect_skill_activation_if_configured(
    name: &str,
    arguments: &str,
    skill_locations: &std::collections::HashMap<PathBuf, crate::config::skills::Skill>,
    activated_skills: &std::sync::Mutex<std::collections::HashSet<String>>,
) -> Option<SkillActivation> {
    if skill_locations.is_empty() {
        return None;
    }
    detect_skill_activation(name, arguments, skill_locations, activated_skills)
}

/// Check whether a just-executed tool call targeted a known SKILL.md path and,
/// if so, emit a `SkillActivated` record once per skill per session.
fn detect_skill_activation(
    name: &str,
    arguments: &str,
    skill_locations: &std::collections::HashMap<PathBuf, crate::config::skills::Skill>,
    activated_skills: &std::sync::Mutex<std::collections::HashSet<String>>,
) -> Option<SkillActivation> {
    if name != "Read" {
        return None;
    }
    let path_str = read_extract_path(arguments)?;
    let path = PathBuf::from(&path_str).canonicalize().ok()?;
    let skill = skill_locations.get(&path)?;
    let mut activated = activated_skills
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    activated.insert(skill.name.clone()).then(|| {
        tracing::info!("Skill '{}' activated", skill.name);
        SkillActivation {
            name: skill.name.clone(),
            path,
        }
    })
}

/// Build a [`CutOffError`] describing why no assistant message was produced.
///
/// `turn_items` must contain only the current turn's items (history after the
/// turn-start index), so reasoning from earlier turns cannot mislabel a fresh
/// empty response as cut off during reasoning.
fn cut_off_error(
    turn_items: &[ConversationItem],
    termination: Option<&crate::session_telemetry::ProviderTermination>,
    resume_session_id: Option<uuid::Uuid>,
) -> CutOffError {
    let mut detail = if turn_items.is_empty() {
        "No response was received from the model.".to_string()
    } else if turn_items
        .iter()
        .any(|item| matches!(item, ConversationItem::Reasoning { .. }))
    {
        "The model's response was cut off during reasoning.".to_string()
    } else {
        "The model's response was incomplete. No final message was received.".to_string()
    };
    detail.push_str(&termination_diagnostic(termination));
    if let Some(session_id) = resume_session_id {
        detail.push_str(" To continue this session explicitly, run: cake --resume ");
        detail.push_str(&session_id.to_string());
        detail.push_str(" \"try again\"");
    }
    CutOffError::new(detail)
}

fn semantic_incomplete_is_retryable(
    termination: Option<&crate::session_telemetry::ProviderTermination>,
) -> bool {
    termination.is_none_or(|termination| {
        !matches!(
            termination.classification,
            TerminationClassification::ContentFilter | TerminationClassification::Failed
        ) && !provider_declares_refusal(termination.provider_status.as_deref())
            && !provider_declares_refusal(termination.provider_reason.as_deref())
    })
}

fn termination_marks_incomplete(
    termination: Option<&crate::session_telemetry::ProviderTermination>,
) -> bool {
    termination.is_some_and(|termination| {
        matches!(
            termination.classification,
            TerminationClassification::ToolCalls
                | TerminationClassification::TokenLimit
                | TerminationClassification::ContentFilter
                | TerminationClassification::Incomplete
                | TerminationClassification::Failed
        ) || provider_declares_refusal(termination.provider_status.as_deref())
            || provider_declares_refusal(termination.provider_reason.as_deref())
    })
}

fn provider_declares_refusal(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        value.eq_ignore_ascii_case("refusal") || value.eq_ignore_ascii_case("refused")
    })
}

fn termination_diagnostic(
    termination: Option<&crate::session_telemetry::ProviderTermination>,
) -> String {
    termination.map_or_else(String::new, |termination| {
        format!(
            " Provider termination: {}.",
            termination.classification.as_str()
        )
    })
}

#[cfg(test)]
mod skill_activation_tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use crate::config::skills::Skill;

    /// Writes one SKILL.md per name under a temp directory and returns the
    /// path-to-skill map keyed by canonicalized location, as production does.
    fn skill_fixture(names: &[&str]) -> (tempfile::TempDir, HashMap<PathBuf, Skill>) {
        let dir = tempfile::tempdir().expect("temp dir");
        let locations = names
            .iter()
            .map(|name| {
                let skill_dir = dir.path().join(name);
                std::fs::create_dir_all(&skill_dir).expect("skill dir");
                std::fs::write(skill_dir.join("SKILL.md"), format!("# {name}\n"))
                    .expect("skill md");
                let location = skill_dir
                    .join("SKILL.md")
                    .canonicalize()
                    .expect("canonicalize");
                (
                    location.clone(),
                    crate::config::skills::Skill {
                        name: (*name).to_string(),
                        description: "test".to_string(),
                        location,
                        base_directory: skill_dir,
                        scope: crate::config::skills::SkillScope::Project,
                    },
                )
            })
            .collect();
        (dir, locations)
    }

    fn read_args(path: &Path) -> String {
        serde_json::json!({ "path": path.display().to_string() }).to_string()
    }

    fn location_of<'a>(locations: &'a HashMap<PathBuf, Skill>, name: &str) -> &'a PathBuf {
        locations
            .values()
            .find(|skill| skill.name == name)
            .map(|skill| &skill.location)
            .expect("fixture skill")
    }

    #[test]
    fn seeded_set_suppresses_reemission_on_resume() {
        let (_dir, locations) = skill_fixture(&["debugging-cake"]);
        let path = location_of(&locations, "debugging-cake");
        // As on resume: the set was hydrated from the persisted session.
        let activated = Mutex::from(HashSet::from(["debugging-cake".to_string()]));

        assert!(
            detect_skill_activation("Read", &read_args(path), &locations, &activated).is_none()
        );
    }

    #[test]
    fn fresh_run_emits_first_observation_exactly_once() {
        let (_dir, locations) = skill_fixture(&["debugging-cake"]);
        let path = location_of(&locations, "debugging-cake");
        let activated = Mutex::new(HashSet::new());

        // Non-Read tools never activate.
        assert!(
            detect_skill_activation("Bash", &read_args(path), &locations, &activated).is_none()
        );

        let first = detect_skill_activation("Read", &read_args(path), &locations, &activated)
            .expect("first read emits once");
        assert_eq!(first.name, "debugging-cake");
        assert_eq!(&first.path, path);

        // A repeat sequential read stays suppressed; the mutex keeps the
        // insert-once check atomic if calls ever run concurrently.
        assert!(
            detect_skill_activation("Read", &read_args(path), &locations, &activated).is_none()
        );
    }

    #[test]
    fn resumed_run_still_emits_newly_activated_skill_once() {
        let (_dir, locations) = skill_fixture(&["known-skill", "new-skill"]);
        let activated = Mutex::from(HashSet::from(["known-skill".to_string()]));

        let new_path = location_of(&locations, "new-skill");
        let activation =
            detect_skill_activation("Read", &read_args(new_path), &locations, &activated)
                .expect("a skill not in the resumed set still activates");
        assert_eq!(activation.name, "new-skill");

        assert!(
            detect_skill_activation("Read", &read_args(new_path), &locations, &activated).is_none()
        );
        let known_path = location_of(&locations, "known-skill");
        assert!(
            detect_skill_activation("Read", &read_args(known_path), &locations, &activated)
                .is_none()
        );
    }
}

#[cfg(test)]
mod helper_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use crate::config::hooks::{HookCommand, HookEvent, HookGroup, HookMatcher, LoadedHooks};
    use crate::config::model::{ApiType, ModelConfig, ResolvedModelConfig};
    use crate::hooks::HookContext;
    use crate::types::Role;

    fn test_agent() -> Agent {
        Agent::new(
            ResolvedModelConfig {
                model_config: ModelConfig {
                    model: "test-model".to_string(),
                    api_type: ApiType::ChatCompletions,
                    base_url: "https://api.example.com".to_string(),
                    api_key_env: "TEST_API_KEY".to_string(),
                    provider: None,
                    provider_headers: None,
                    temperature: None,
                    top_p: None,
                    max_output_tokens: None,
                    context_window: None,
                    reasoning_effort: None,
                    reasoning_summary: None,
                    reasoning_max_tokens: None,
                    providers: vec![],
                },
                api_key: "test-key".to_string(),
            },
            &[(Role::System, "test system prompt".to_string())],
        )
    }

    fn hook_context() -> HookContext {
        HookContext {
            session_id: uuid::Uuid::new_v4(),
            task_id: uuid::Uuid::new_v4(),
            transcript_path: None,
            hook_event_sink: None,
            cwd: std::env::current_dir().expect("current directory"),
            model: "test-model".to_string(),
        }
    }

    fn hook_runner(event: HookEvent, command: &str, fail_closed: bool) -> Arc<HookRunner> {
        Arc::new(HookRunner::new(
            LoadedHooks {
                groups: vec![HookGroup {
                    event,
                    matcher: HookMatcher::All,
                    hooks: vec![HookCommand {
                        command: command.to_string(),
                        timeout: Duration::from_secs(2),
                        fail_closed,
                        status_message: None,
                        source_path: PathBuf::from("test-hook.json"),
                    }],
                }],
            },
            hook_context(),
        ))
    }

    fn empty_hook_runner() -> Arc<HookRunner> {
        Arc::new(HookRunner::new(LoadedHooks::default(), hook_context()))
    }

    fn read_arguments(path: &std::path::Path) -> String {
        serde_json::json!({ "path": path.display().to_string() }).to_string()
    }

    #[tokio::test]
    async fn run_tool_call_covers_execute_error_and_block_paths() {
        let dir = tempfile::tempdir_in(std::env::current_dir().expect("current directory"))
            .expect("temporary directory");
        let path = dir.path().join("input.txt");
        std::fs::write(&path, "tool output\n").expect("input file");

        let mut agent = test_agent().with_hook_runner(hook_runner(
            HookEvent::PostToolUse,
            "printf '%s' '{\"additional_context\":\"post context\"}'",
            false,
        ));
        agent.turn_count = 4;
        let success = agent
            .run_tool_call(
                "call-success".to_string(),
                "Read".to_string(),
                ToolHookPlan::Execute {
                    arguments: read_arguments(&path),
                    prefix_notice: Some("prefix notice\n".to_string()),
                    additional_context: vec!["plan context".to_string()],
                },
            )
            .await;
        assert!(!success.telemetry.was_error);
        assert_eq!(success.telemetry.turn_index, 4);
        assert_eq!(success.telemetry.call_id, "call-success");
        assert!(success.output.starts_with("prefix notice\n"));
        assert!(success.output.contains("tool output"));
        assert!(success.output.contains("post context"));
        assert!(success.output.contains("plan context"));
        assert_eq!(success.telemetry.output_bytes, success.output.len());

        let missing = dir.path().join("missing.txt");
        let error = agent
            .run_tool_call(
                "call-error".to_string(),
                "Read".to_string(),
                ToolHookPlan::Execute {
                    arguments: read_arguments(&missing),
                    prefix_notice: None,
                    additional_context: Vec::new(),
                },
            )
            .await;
        assert!(error.telemetry.was_error);
        assert!(error.output.starts_with("Error:"), "{}", error.output);

        let blocked = agent
            .run_tool_call(
                "call-blocked".to_string(),
                "Read".to_string(),
                ToolHookPlan::Block {
                    reason: "not allowed".to_string(),
                    additional_context: vec!["block context".to_string()],
                },
            )
            .await;
        assert!(blocked.telemetry.was_error);
        assert!(
            blocked
                .output
                .contains("Hook blocked tool execution: not allowed")
        );
        assert!(blocked.output.contains("block context"));
    }

    #[tokio::test]
    async fn post_tool_context_skips_nonmatching_hook_configuration() {
        let runner = hook_runner(HookEvent::PreToolUse, "exit 1", true);
        let result = ToolResult {
            output: "tool output".to_string(),
            compensation_events: Vec::new(),
        };
        let agent = test_agent().with_hook_runner(Arc::clone(&runner));

        assert_eq!(
            post_tool_context_for_result(
                Some(&runner),
                &agent.tools,
                "Read",
                "call-1",
                "{}",
                &Ok(result),
            )
            .await,
            None
        );
    }

    #[tokio::test]
    async fn post_tool_context_covers_all_hook_outcomes() {
        let result = Ok("tool output".to_string());
        assert_eq!(
            post_tool_context(None, false, "Read", "call-1", "{}", &result).await,
            None
        );

        let empty = empty_hook_runner();
        assert_eq!(
            post_tool_context(Some(&empty), false, "Read", "call-1", "{}", &result).await,
            None
        );

        let context = hook_runner(
            HookEvent::PostToolUse,
            "printf '%s' '{\"additional_context\":\"post context\"}'",
            false,
        );
        assert_eq!(
            post_tool_context(Some(&context), false, "Read", "call-1", "{}", &result,)
                .await
                .as_deref(),
            Some("post context")
        );

        let failed = hook_runner(HookEvent::PostToolUse, "exit 1", true);
        assert_eq!(
            post_tool_context(Some(&failed), false, "Read", "call-1", "{}", &result).await,
            None
        );
    }

    fn tool_result(
        skill_activation: Option<SkillActivation>,
        compensation_events: Vec<CompensationEventTelemetry>,
    ) -> ToolRunResult {
        ToolRunResult {
            call_id: "call-1".to_string(),
            output: "tool output".to_string(),
            skill_activation,
            telemetry: ToolCallTelemetry {
                turn_index: 1,
                call_id: "call-1".to_string(),
                name: "Read".to_string(),
                duration_ms: 1,
                output_bytes: 11,
                was_error: false,
            },
            compensation_events,
        }
    }

    #[test]
    fn record_tool_results_persists_skill_and_denial_records() {
        let persisted = Arc::new(Mutex::new(Vec::<SessionRecord>::new()));
        let persisted_clone = Arc::clone(&persisted);
        let streamed = Arc::new(Mutex::new(Vec::<String>::new()));
        let streamed_clone = Arc::clone(&streamed);
        let mut agent = test_agent()
            .with_persist_callback(move |record| {
                persisted_clone
                    .lock()
                    .expect("persist lock")
                    .push(record.clone());
                Ok(())
            })
            .with_streaming_json(move |json| {
                streamed_clone
                    .lock()
                    .expect("stream lock")
                    .push(json.to_string());
            });

        agent
            .record_tool_results(vec![tool_result(
                Some(SkillActivation {
                    name: "debugging-cake".to_string(),
                    path: PathBuf::from("/skills/debugging-cake/SKILL.md"),
                }),
                vec![CompensationEventTelemetry::judge_verdict(
                    "block",
                    Some("unsafe-command"),
                    1,
                    false,
                )],
            )])
            .expect("record tool result");

        assert_eq!(
            agent.permission_denials,
            vec!["Read(call-1): judge block: unsafe-command"]
        );
        assert!(matches!(
            persisted.lock().expect("persist lock").first(),
            Some(SessionRecord::SkillActivated { name, .. }) if name == "debugging-cake"
        ));
        assert!(matches!(
            persisted.lock().expect("persist lock").get(1),
            Some(SessionRecord::FunctionCallOutput(data))
                if data.replay == Some(crate::types::ReplaySafety::Safe)
        ));
        assert!(matches!(
            agent.history().last(),
            Some(ConversationItem::FunctionCallOutput { call_id, output, .. })
                if call_id == "call-1" && output == "tool output"
        ));
        let streamed = streamed.lock().expect("stream lock");
        assert_eq!(streamed.len(), 1);
        let streamed_record: serde_json::Value = serde_json::from_str(&streamed[0]).unwrap();
        assert_eq!(streamed_record["replay"], "safe");
        drop(streamed);
    }

    #[test]
    fn stream_turn_items_persists_replay_snapshot_for_read() {
        let persisted = Arc::new(Mutex::new(Vec::<SessionRecord>::new()));
        let persisted_clone = Arc::clone(&persisted);
        let streamed = Arc::new(Mutex::new(Vec::<String>::new()));
        let streamed_clone = Arc::clone(&streamed);
        let mut agent = test_agent()
            .with_persist_callback(move |record| {
                persisted_clone
                    .lock()
                    .expect("persist lock")
                    .push(record.clone());
                Ok(())
            })
            .with_streaming_json(move |json| {
                streamed_clone
                    .lock()
                    .expect("stream lock")
                    .push(json.to_string());
            });

        let items = vec![ConversationItem::FunctionCall {
            id: "fc-1".to_string(),
            call_id: "call-1".to_string(),
            name: "Read".to_string(),
            arguments: "{}".to_string(),
            timestamp: None,
        }];
        agent
            .stream_turn_items(&items)
            .expect("stream tool call record");

        assert!(matches!(
            persisted.lock().expect("persist lock").first(),
            Some(SessionRecord::FunctionCall(data))
                if data.replay == Some(crate::types::ReplaySafety::Safe)
        ));
        let streamed = streamed.lock().expect("stream lock");
        let streamed_record: serde_json::Value = serde_json::from_str(&streamed[0]).unwrap();
        assert_eq!(streamed_record["replay"], "safe");
        drop(streamed);
    }

    #[test]
    fn disabled_tool_turn_records_correction_and_cutoff_paths() {
        let calls = vec![("call-1".to_string(), "Read".to_string(), "{}".to_string())];
        let mut correction_agent = test_agent();
        let mut corrections_used = 0;
        let mut mode = TurnMode::SchemaCorrection;
        let correction_start = correction_agent.history().len();
        correction_agent
            .handle_disabled_tools_turn(
                &calls,
                &mut corrections_used,
                &mut mode,
                correction_start,
                None,
            )
            .expect("correction tool calls are recorded");
        assert_eq!(corrections_used, 1);
        assert!(correction_agent.history().iter().any(|item| matches!(
            item,
            ConversationItem::FunctionCallOutput { output, .. }
                if output.contains("not executed: correction turn offers no tools")
        )));

        let mut recovery_agent = test_agent();
        let mut recovery_mode = TurnMode::SemanticRecovery;
        let recovery_start = recovery_agent.history().len();
        let mut unused_corrections = 0;
        let error = recovery_agent
            .handle_disabled_tools_turn(
                &calls,
                &mut unused_corrections,
                &mut recovery_mode,
                recovery_start,
                None,
            )
            .expect_err("semantic recovery tool calls must stop");
        assert!(error.to_string().contains("response was incomplete"));
    }
}
