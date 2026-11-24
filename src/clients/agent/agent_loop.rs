use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;

use crate::clients::agent::{
    Agent, FixedTokensCache, RequestCapture, TurnResult, record_turn_usage,
};
use crate::clients::agent_state::{RecordProvenance, accumulate_usage};
use crate::clients::backend::FinalOutputConstraint;
use crate::clients::compaction::{FixedRequest, MeasuredInput, fixed_request_tokens};
use crate::clients::image_budget::prune_request_with_plan;
use crate::clients::retry::{RequestOverrides, RetryReason, RetryStatus, context_failure_code};
use crate::clients::termination::{
    self, ContextPressure, IntendedLimits, TerminationVerdict, TurnEvidence,
};
use crate::clients::tools::{
    ScheduledToolCall, Tool, ToolError, ToolRegistry, ToolResult, argument_compensation_events,
    read_extract_path, schedule_tool_calls,
};
use crate::concurrency::MAX_CONCURRENT_AGENT_OPERATIONS;
use crate::config::output_schema::{OutputSchema, OutputSchemaError};
use crate::hooks::{HookRunner, ToolHookPlan};
use crate::session_telemetry::{
    AgentRunnerTelemetryEvent, CompensationEventTelemetry, CompensationKind,
    RetryScheduledTelemetry, ToolCallTelemetry,
};
use crate::types::compaction::SkillRead;
use crate::types::{
    ConversationItem, CutOffError, ImagePart, LimitExceededError, ReplaySafety, Role, SessionRecord,
};

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
    verdict: &'a TerminationVerdict,
}

/// The invocation's single context-pressure recovery allowance.
///
/// Shared by every pressure verdict in one `Agent::send` invocation, including
/// an HTTP input overflow and an in-band Responses context failure. The guard
/// is consumed before recovery preparation, so a second pressure verdict ends
/// the invocation instead of compacting again.
#[derive(Debug, Default)]
struct PressureRecoveryGuard {
    spent: bool,
}

impl PressureRecoveryGuard {
    /// Consume the allowance, returning whether it was still available.
    const fn consume(&mut self) -> bool {
        let available = !self.spent;
        self.spent = true;
        available
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TurnMode {
    Normal,
    SchemaCorrection,
    SemanticRecovery,
}

impl TurnMode {
    pub(super) const fn tools_disabled(self) -> bool {
        !matches!(self, Self::Normal)
    }
}

#[derive(Debug, Clone)]
struct ToolRunResult {
    call_id: String,
    output: String,
    /// Inline images returned by the tool, empty for text-only results.
    images: Vec<ImagePart>,
    /// Known-skill instructions observed on every successful read, including
    /// rereads after the activation telemetry was already emitted.
    skill_read: Option<SkillRead>,
    skill_activation: Option<SkillActivation>,
    telemetry: ToolCallTelemetry,
    compensation_events: Vec<CompensationEventTelemetry>,
    permission_denials: Vec<String>,
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
            let detail = event.detail.as_deref()?;
            if let Some(code) = detail.strip_prefix("block:") {
                return Some(format!("judge block: {code}"));
            }
            (detail == "block").then(|| "judge block".to_string())
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
    let now = chrono::Utc::now();
    ToolRunResult {
        telemetry: ToolCallTelemetry {
            turn_index,
            call_id: (!call_id.is_empty()).then(|| call_id.to_string()),
            name: name.to_string(),
            duration_ms: 0,
            started_at: Some(now),
            completed_at: Some(now),
            output_bytes: output.len(),
            was_error: true,
        },
        call_id: call_id.to_string(),
        output,
        images: Vec::new(),
        skill_read: None,
        skill_activation: None,
        compensation_events: Vec::new(),
        permission_denials: Vec::new(),
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
        let invocation_start = self.conversation.history().len();
        let task_id = self.task_id.to_string();
        let user_item = self.conversation.push_user_message(content, Some(&task_id));
        self.stream_item(&user_item)?;
        let mut compaction = super::compaction::InvocationCompactionState::new(
            invocation_start,
            invocation_start,
            task_id,
        );

        let mut corrections_used: u32 = 0;
        let mut turn_mode = TurnMode::Normal;
        let mut semantic_recovery_used = false;
        let mut pressure_guard = PressureRecoveryGuard::default();

        loop {
            self.enforce_limits(0, invocation_start)?;

            self.refresh_changed_mcp_tools().await;

            self.maybe_compact_before_request(turn_mode, &mut compaction)
                .await?;

            let provider_turn_start = self.conversation.history().len();
            let (turn, verdict) = self
                .complete_turn_with_pressure_recovery(
                    turn_mode,
                    &mut pressure_guard,
                    &mut compaction,
                )
                .await?;
            let TurnResult {
                items,
                consumed_sources,
                ..
            } = turn;

            self.conversation.mark_images_consumed(&consumed_sources);

            let function_calls = self.process_completed_turn(items, turn_mode)?;
            let turn_context = TurnContext {
                turn_mode: &mut turn_mode,
                corrections_used: &mut corrections_used,
                semantic_recovery_used: &mut semantic_recovery_used,
                provider_turn_start,
                invocation_start,
                verdict: &verdict,
            };
            if let Some(document) = self.process_turn(function_calls, turn_context).await? {
                self.refresh_request_capture();
                return Ok(document);
            }
        }
    }

    /// Send one prebuilt request body for exact request reuse.
    ///
    /// This path never executes tools. A returned tool call is an error, not a
    /// dispatch, whatever `tool_choice` the captured request carries.
    pub async fn send_reused(&mut self, request: Vec<u8>) -> anyhow::Result<String> {
        let turn = self
            .runner
            .fixed_turn(&self.config, self.session_id, request)
            .await?;
        self.turn_count = self.turn_count.saturating_add(1);
        if let Some(reported) = turn.usage {
            accumulate_usage(&mut self.total_usage, Some(&reported.usage));
            self.last_usage = Some(reported.usage);
        }
        if turn
            .items
            .iter()
            .any(|item| matches!(item, ConversationItem::FunctionCall { .. }))
        {
            anyhow::bail!(
                "Reused request returned a tool call; refusing to execute it. The request capture may be incompatible."
            );
        }
        let message = turn
            .items
            .iter()
            .rev()
            .find_map(|item| match item {
                ConversationItem::Message {
                    role: Role::Assistant,
                    content,
                    ..
                } => Some(content.clone()),
                _ => None,
            })
            .ok_or_else(|| anyhow::anyhow!("Reused request returned no final assistant message"))?;
        let Some(schema) = self.output_schema.clone() else {
            return Ok(message);
        };
        let trimmed = message.trim();
        let instance: serde_json::Value =
            serde_json::from_str(trimmed).map_err(|error| OutputSchemaError::Unsatisfied {
                attempts: 1,
                detail: format!("the response is not a single valid JSON document: {error}"),
            })?;
        if let Some(detail) = schema.validation_detail(&instance) {
            return Err(OutputSchemaError::Unsatisfied {
                attempts: 1,
                detail,
            }
            .into());
        }
        Ok(trimmed.to_string())
    }

    /// Record the body the next normal main-agent request would send.
    ///
    /// The capture is rebuilt after the final turn, so it includes the accepted
    /// assistant items and follows any tool results.
    fn refresh_request_capture(&mut self) {
        if !self.capture_requests {
            self.request_capture = None;
            return;
        }
        let history = self.conversation.request_context();
        let history = if self.config.model_config.supports_images {
            match prune_request_with_plan(history, &self.image_policy) {
                Ok(plan) => plan.history,
                Err(error) => {
                    tracing::debug!(
                        target: "cake",
                        error = %error,
                        "Skipping request capture after an image planning failure"
                    );
                    return;
                },
            }
        } else {
            history
        };
        let tools = tool_definitions_for_turn(&self.tools, TurnMode::Normal);
        let overrides = RequestOverrides {
            max_output_tokens: self.config.model_config.max_output_tokens,
            reasoning_max_tokens: self.config.model_config.reasoning_max_tokens,
            context_overflow_retry_used: false,
        };
        match self.runner.backend().build_request_json(
            &self.config,
            &history,
            tools,
            self.tool_choice,
            &overrides,
            None,
        ) {
            Ok(body) => {
                self.request_capture = Some(RequestCapture {
                    body,
                    turn: self.turn_count,
                });
            },
            Err(error) => {
                tracing::debug!(
                    target: "cake",
                    error = %error,
                    "Skipping request capture after a request build failure"
                );
            },
        }
    }

    /// Complete one provider turn, recovering once from classified pressure.
    ///
    /// Returns the first response that is not context pressure together with
    /// its classifier verdict. A discarded response is never persisted,
    /// streamed, or dispatched. When pressure cannot be recovered, the error is
    /// the neutral [`CutOffError`] naming the session.
    async fn complete_turn_with_pressure_recovery(
        &mut self,
        turn_mode: TurnMode,
        guard: &mut PressureRecoveryGuard,
        compaction: &mut super::compaction::InvocationCompactionState,
    ) -> anyhow::Result<(TurnResult, TerminationVerdict)> {
        loop {
            let turn = match self.complete_turn_with_image_recovery(turn_mode).await {
                Ok(turn) => turn,
                Err(error) => {
                    let Some(pressure) = pressure_from_error(&error) else {
                        return Err(error);
                    };
                    self.recover_from_context_pressure(pressure, guard, turn_mode, compaction)
                        .await?;
                    continue;
                },
            };

            let verdict = termination::classify(
                self.intended_limits(),
                TurnEvidence::Response {
                    termination: turn.termination.as_ref(),
                    usage: turn.usage,
                },
            );
            if let Some(pressure) = verdict.pressure {
                tracing::info!(
                    target: "cake",
                    pressure = ?pressure,
                    measured_input = verdict.measured_input,
                    context_window = verdict.context_window,
                    "Discarding a response classified as context pressure"
                );
                self.recover_from_context_pressure(pressure, guard, turn_mode, compaction)
                    .await?;
                continue;
            }
            return Ok((turn, verdict));
        }
    }

    /// Consume the invocation's recovery allowance and compact before retrying.
    ///
    /// A spent guard or a recovery that commits no checkpoint returns the
    /// neutral context-pressure error. On success the caller retries the
    /// intended turn with the same mode, output constraint, correction
    /// counters, and tool policy.
    async fn recover_from_context_pressure(
        &mut self,
        pressure: ContextPressure,
        guard: &mut PressureRecoveryGuard,
        turn_mode: TurnMode,
        compaction: &mut super::compaction::InvocationCompactionState,
    ) -> anyhow::Result<()> {
        if !guard.consume() {
            tracing::info!(
                target: "cake",
                pressure = ?pressure,
                "Context-pressure recovery was already used in this invocation"
            );
            return Err(context_pressure_error(self.session_id, pressure).into());
        }
        tracing::info!(
            target: "cake",
            pressure = ?pressure,
            "Attempting one context-pressure recovery"
        );
        if !self.compact_after_pressure(turn_mode, compaction).await? {
            return Err(context_pressure_error(self.session_id, pressure).into());
        }
        tracing::info!(
            target: "cake",
            pressure = ?pressure,
            "Context checkpoint committed; retrying the intended turn once"
        );
        Ok(())
    }

    /// The limits the current request was intended to honor.
    ///
    /// The output cap is the resolved model maximum before any context
    /// clamping; `complete_turn` may send a smaller allowance after an overflow,
    /// which must never become the reference for a cap comparison.
    const fn intended_limits(&self) -> IntendedLimits {
        IntendedLimits {
            output_cap: self.config.model_config.max_output_tokens,
            context_window: self.context_window(),
        }
    }

    fn process_completed_turn(
        &mut self,
        items: Vec<ConversationItem>,
        turn_mode: TurnMode,
    ) -> anyhow::Result<Vec<FunctionCall>> {
        self.turn_count += 1;
        self.log_context_budget();

        let function_calls = Self::function_calls_from_items(&items);
        let replay_eligible = !turn_mode.tools_disabled();
        let source_task = self.task_id.to_string();
        let provenance: Vec<RecordProvenance> = items
            .iter()
            .map(|item| {
                RecordProvenance::live(
                    item,
                    Some(source_task.clone()),
                    self.item_replay(item, replay_eligible),
                )
            })
            .collect();
        self.stream_turn_items(&items, replay_eligible)?;
        self.conversation
            .extend_turn_items_with_provenance(items, provenance);
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
                context.verdict,
            );
        }

        if context.turn_mode.tools_disabled() {
            self.handle_disabled_tools_turn(
                &function_calls,
                context.corrections_used,
                context.turn_mode,
                context.provider_turn_start,
                context.verdict,
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
        verdict: &TerminationVerdict,
    ) -> anyhow::Result<Option<String>> {
        let message = self
            .conversation
            .resolve_assistant_message_from(provider_turn_start);
        if message.is_none() || verdict.marks_incomplete() {
            if !*semantic_recovery_used && verdict.allows_semantic_continuation() {
                *semantic_recovery_used = true;
                *turn_mode = TurnMode::SemanticRecovery;
                self.record_semantic_recovery_retry();
                let task_id = self.task_id.to_string();
                let continuation = self.conversation.push_synthetic_user_message(
                    SEMANTIC_RECOVERY_PROMPT.to_string(),
                    Some(&task_id),
                );
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
                verdict,
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
        verdict: &TerminationVerdict,
    ) -> anyhow::Result<()> {
        for (call_id, name, _) in function_calls {
            let output =
                format!("not executed: correction turn offers no tools for {name}({call_id})");
            let task_id = self.task_id.to_string();
            let provenance = RecordProvenance::live_tool_output(Some(task_id), None, None, false);
            let item =
                self.conversation
                    .push_tool_output(call_id.clone(), output, Vec::new(), provenance);
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
        Err(cut_off_error(turn_items, verdict, Some(self.session_id)).into())
    }

    async fn execute_tool_turn(
        &mut self,
        function_calls: Vec<FunctionCall>,
        invocation_start: usize,
    ) -> anyhow::Result<()> {
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
        let results = self.run_tool_plans(tool_plans).await;
        self.activate_pending_mcp_tools()?;
        Ok(results)
    }

    async fn plan_tool_calls(
        &self,
        function_calls: Vec<FunctionCall>,
    ) -> anyhow::Result<Vec<(String, String, ToolHookPlan)>> {
        let hook_runner = self.hook_runner.clone();
        let pre_futures =
            function_calls
                .into_iter()
                .enumerate()
                .map(|(index, (call_id, name, arguments))| {
                    let hook_runner = hook_runner.clone();
                    let repairs_arguments = self.tools.repairs_arguments(&name);
                    async move {
                        let result = if let Some(runner) = hook_runner {
                            runner
                                .pre_tool_use(&name, &call_id, &arguments, repairs_arguments)
                                .await
                        } else {
                            Ok(ToolHookPlan::Execute {
                                arguments,
                                prefix_notice: None,
                                additional_context: Vec::new(),
                            })
                        };
                        (index, result.map(|plan| (call_id, name, plan)))
                    }
                });
        let mut pre_results = futures::stream::iter(pre_futures)
            .buffer_unordered(MAX_CONCURRENT_AGENT_OPERATIONS)
            .collect::<Vec<_>>()
            .await;
        pre_results.sort_unstable_by_key(|(index, _)| *index);
        let mut tool_plans = Vec::with_capacity(pre_results.len());
        for (_, result) in pre_results {
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
    /// Calls targeting the same canonical path run sequentially in issue order
    /// when the turn mutates that path, so each call sees the previous call's
    /// effects; all other calls run concurrently. Results are returned in the
    /// model's issue order regardless of grouping.
    async fn run_tool_plans(
        &self,
        tool_plans: Vec<(String, String, ToolHookPlan)>,
    ) -> Vec<ToolRunResult> {
        let expected_results = tool_plans.len();
        let groups = schedule_tool_calls(&self.tools, self.tool_context.as_ref(), tool_plans);
        let mut results = Vec::with_capacity(expected_results);
        results.extend(
            futures::stream::iter(groups.into_iter().map(|group| self.run_group(group)))
                .buffer_unordered(MAX_CONCURRENT_AGENT_OPERATIONS)
                .collect::<Vec<_>>()
                .await
                .into_iter()
                .flatten(),
        );
        results.sort_unstable_by_key(|(index, _)| *index);
        results.into_iter().map(|(_, result)| result).collect()
    }

    /// Execute one scheduling group: members run sequentially in issue order,
    /// each seeing the previous call's effects. Every member after the first
    /// was delayed by same-path serialization, recorded as a telemetry event.
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
                let started_at = chrono::Utc::now();
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
                let (mut compensation_events, permission_denials, mut output, images) = match result
                {
                    Ok(result) => (
                        result.compensation_events,
                        result.permission_denials,
                        result.output,
                        result.images,
                    ),
                    Err(error) => (
                        error.compensation_events,
                        Vec::new(),
                        format!("Error: {}", error.message),
                        Vec::new(),
                    ),
                };
                let registered = self.tools.has(&name);
                compensation_events.extend(argument_compensation_events(
                    &name,
                    &arguments,
                    was_error,
                    registered,
                    self.tools.repairs_arguments(&name),
                ));

                let (skill_read, skill_activation) = observe_skill_read(
                    &name,
                    &arguments,
                    was_error,
                    &self.skill_locations,
                    &self.activated_skills,
                );
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
                let completed_at = chrono::Utc::now();
                ToolRunResult {
                    telemetry: ToolCallTelemetry {
                        turn_index,
                        call_id: (!call_id.is_empty()).then(|| call_id.clone()),
                        name,
                        duration_ms,
                        started_at: Some(started_at),
                        completed_at: Some(completed_at),
                        output_bytes: output.len(),
                        was_error,
                    },
                    call_id,
                    output,
                    images,
                    skill_read,
                    skill_activation,
                    compensation_events,
                    permission_denials,
                }
            },
        }
    }

    fn record_tool_results(&mut self, results: Vec<ToolRunResult>) -> anyhow::Result<()> {
        for result in results {
            self.record_permission_denials(
                &result.telemetry.name,
                &result.call_id,
                &result.permission_denials,
            );
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
            let has_images = !result.images.is_empty();
            let item = self.conversation.push_tool_output(
                result.call_id,
                result.output,
                result.images,
                RecordProvenance::live_tool_output(
                    Some(self.task_id.to_string()),
                    replay,
                    result.skill_read,
                    has_images,
                ),
            );
            self.stream_item_with_replay(&item, replay)?;
        }
        Ok(())
    }

    fn record_compensation_events(&mut self, events: Vec<CompensationEventTelemetry>) {
        for event in events {
            self.append_compensation_telemetry(event);
        }
    }

    /// Record policy-denial labels supplied by a tool after it ran. The tool
    /// supplies only the stable source/operation/path label; this method adds
    /// the same tool/call identity used by hook and judge denials.
    fn record_permission_denials(&mut self, name: &str, call_id: &str, denials: &[String]) {
        self.permission_denials.extend(
            denials
                .iter()
                .map(|denial| format!("{name}({call_id}): {denial}")),
        );
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
        let task_id = self.task_id.to_string();
        let item = self
            .conversation
            .push_synthetic_user_message(corrective, Some(&task_id));
        self.stream_item(&item)?;
        Ok(())
    }

    /// Complete one turn, recovering from a provider that rejects the
    /// conversation's images.
    ///
    /// One image the provider cannot read rejects the whole request, so a turn
    /// that fails that way drops every image, tells the model, and asks again.
    /// The retry is bounded to one per turn: a second rejection after the
    /// images are gone is not about pixels and stays terminal.
    async fn complete_turn_with_image_recovery(
        &mut self,
        turn_mode: TurnMode,
    ) -> anyhow::Result<TurnResult> {
        let mut images_dropped = false;
        loop {
            match self
                .complete_turn_with_output_schema_fallback(turn_mode)
                .await
            {
                Ok(turn) => return Ok(turn),
                Err(error) if !images_dropped && is_image_rejection(&error) => {
                    images_dropped = true;
                    self.recover_from_image_rejection(&error)?;
                },
                Err(error) => return Err(error),
            }
        }
    }

    /// Recover from a provider that rejected the conversation's images.
    ///
    /// Nothing else in the request can be repaired by removing pixels, so the
    /// recovery drops every image and tells the model why the visuals are
    /// gone. The note streams and persists like any other conversation item, so
    /// a resumed session carries the same explanation the model saw.
    fn recover_from_image_rejection(&mut self, error: &anyhow::Error) -> anyhow::Result<()> {
        let dropped = self.conversation.drop_images();
        let detail = image_rejection_detail(error);
        tracing::warn!(
            target: "cake",
            dropped,
            detail,
            "Provider rejected an image; retrying the turn without images"
        );
        let note = format!(
            "The provider rejected this conversation's images: {detail}\n\n\
             cake removed {dropped} image(s) from the conversation and is retrying without \
             them. Do not read the same image again as-is: crop or downscale it first, or \
             continue from the text you already have."
        );
        let task_id = self.task_id.to_string();
        let item = self
            .conversation
            .push_synthetic_user_message(note, Some(&task_id));
        self.stream_item(&item)
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
        let fixed_output_schema = native_constraint_attached
            .then(|| self.output_schema.as_deref().map(|schema| &schema.raw))
            .flatten();
        let registry_revision = self.tools.revision();
        let tools_disabled = turn_mode.tools_disabled();
        let request_fixed_tokens = match self.fixed_tokens {
            Some(cache)
                if cache.registry_revision == registry_revision
                    && cache.tools_disabled == tools_disabled
                    && cache.schema_attached == native_constraint_attached =>
            {
                cache.tokens
            },
            _ => {
                let tokens = fixed_request_tokens(&FixedRequest {
                    tool_schemas: tool_definitions,
                    output_schema: fixed_output_schema,
                });
                self.fixed_tokens = Some(FixedTokensCache {
                    registry_revision,
                    tools_disabled,
                    schema_attached: native_constraint_attached,
                    tokens,
                });
                tokens
            },
        };
        let config = &self.config;
        let session_id = self.session_id;
        let task_id = self.task_id;
        let history = self.conversation.request_context();
        let mut consumed_sources = Vec::new();
        let history = if self.config.model_config.supports_images {
            let request = prune_request_with_plan(history, &self.image_policy)?;
            let source_refs = self.conversation.request_source_refs();
            consumed_sources = request
                .fully_emitted_items()
                .into_iter()
                .filter_map(|item| source_refs.get(item).copied())
                .collect();
            request.history
        } else {
            history
        };
        let turn_index = self.turn_count.saturating_add(1);
        self.last_measurement = None;
        let health = self.health_callback();
        let runner = &self.runner;
        let runner_telemetry = self.runner_telemetry_sink();
        let observer = &mut self.observer;
        let total_usage = &mut self.total_usage;
        let last_usage = &mut self.last_usage;
        let mut turn = runner
            .complete_turn(
                config,
                session_id,
                task_id,
                turn_index,
                &history,
                tool_definitions,
                self.tool_choice,
                constraint,
                None,
                health.as_ref(),
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
                        config.model_config.context_window,
                        settlement,
                    );
                },
            )
            .await?;
        self.record_request_measurement(turn.usage.as_ref(), history.len(), request_fixed_tokens);
        turn.consumed_sources = consumed_sources;
        Ok(turn)
    }

    fn record_request_measurement(
        &mut self,
        usage: Option<&crate::types::ReportedUsage>,
        covered_item_count: usize,
        fixed_tokens: u64,
    ) {
        let Some(usage) = usage else {
            return;
        };
        if usage.presence != crate::types::UsagePresence::Complete || usage.usage.input_tokens == 0
        {
            return;
        }
        self.last_measurement = Some(MeasuredInput {
            input_tokens: usage.usage.input_tokens,
            covered_item_count,
            model: self.config.model_config.model.clone(),
            fixed_tokens,
            presence: usage.presence,
        });
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
        self.append_runner_telemetry(AgentRunnerTelemetryEvent::retry_scheduled(
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

    fn stream_turn_items(
        &mut self,
        items: &[ConversationItem],
        replay_eligible: bool,
    ) -> anyhow::Result<()> {
        if !self.observer.is_active() {
            return Ok(());
        }
        for item in items {
            let replay = self.item_replay(item, replay_eligible);
            self.stream_item_with_replay(item, replay)?;
        }
        Ok(())
    }

    /// The replay declaration the live loop records and persists for one item.
    fn item_replay(&self, item: &ConversationItem, replay_eligible: bool) -> Option<ReplaySafety> {
        replay_eligible
            .then(|| match item {
                ConversationItem::FunctionCall { name, .. } => Some(self.tools.replay_safety(name)),
                _ => None,
            })
            .flatten()
    }
}

fn is_native_constraint_rejection(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<crate::exit_code::ApiError>()
        .is_some_and(|api_error| {
            api_error.status == 400
                && api_error.input_overflow.is_none()
                && api_error.body_read_failure.is_none()
        })
}

fn tool_definitions_for_turn(tools: &ToolRegistry, turn_mode: TurnMode) -> &[Tool] {
    if turn_mode.tools_disabled() {
        &[]
    } else {
        tools.definitions()
    }
}

/// Whether a provider rejected the request because it could not read an image.
///
/// Providers report an unreadable image through a generic invalid-request
/// status, so the status alone cannot single it out: the body must also name an
/// image and a rejection. Requiring both keeps an unrelated 400 from silently
/// costing the run the pixels it can still use. A typed input overflow is never
/// an image rejection: dropping pixels is not what makes the input fit, and the
/// compaction owner must see the verdict unchanged.
fn is_image_rejection(error: &anyhow::Error) -> bool {
    let Some(api_error) = error.downcast_ref::<crate::exit_code::ApiError>() else {
        return false;
    };
    if api_error.input_overflow.is_some() || api_error.body_read_failure.is_some() {
        return false;
    }
    if !matches!(api_error.status, 400 | 413 | 415 | 422) {
        return false;
    }
    let body = api_error.body.to_ascii_lowercase();
    body.contains("image")
        && [
            "unsupported",
            "invalid",
            "not supported",
            "decode",
            "format",
            "too large",
            "exceeds",
        ]
        .iter()
        .any(|marker| body.contains(marker))
}

/// The provider's own rejection text, bounded so the recovery note stays small.
fn image_rejection_detail(error: &anyhow::Error) -> String {
    const MAX_CHARS: usize = 400;
    let detail = error
        .downcast_ref::<crate::exit_code::ApiError>()
        .map_or_else(|| error.to_string(), |api_error| api_error.body.clone());
    let detail = detail.trim();
    if detail.chars().count() <= MAX_CHARS {
        return detail.to_string();
    }
    let end = detail
        .char_indices()
        .nth(MAX_CHARS)
        .map_or(detail.len(), |(index, _)| index);
    let truncated = detail.get(..end).unwrap_or(detail);
    format!("{truncated}…")
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

/// Record the read evidence and one-time activation for a tool result.
fn observe_skill_read(
    name: &str,
    arguments: &str,
    was_error: bool,
    skill_locations: &std::collections::HashMap<PathBuf, crate::config::skills::Skill>,
    activated_skills: &std::sync::Mutex<std::collections::HashSet<String>>,
) -> (Option<SkillRead>, Option<SkillActivation>) {
    if was_error {
        return (None, None);
    }
    let skill_read = detect_skill_read_if_configured(name, arguments, skill_locations);
    let activation = skill_read
        .as_ref()
        .and_then(|read| activate_skill_if_first(read, activated_skills));
    (skill_read, activation)
}

/// Skip the skill read probe entirely when no skills were discovered.
fn detect_skill_read_if_configured(
    name: &str,
    arguments: &str,
    skill_locations: &std::collections::HashMap<PathBuf, crate::config::skills::Skill>,
) -> Option<SkillRead> {
    if skill_locations.is_empty() {
        return None;
    }
    detect_skill_read(name, arguments, skill_locations)
}

/// Observe a successful `Read` of a known SKILL.md path, including rereads.
///
/// Skill evidence is deliberately `Read`-only: the `Read` tool carries a
/// single path argument, while matching a known `SKILL.md` inside arbitrary
/// shell text is a heuristic that could misattribute activation. A `Bash` read
/// of a `SKILL.md` does not produce evidence. An ordinary Read is the common
/// case, so the payload must contain `SKILL.md` before the argument parse and
/// canonicalize run; an alias that omits the spelling is not probed, and the
/// canonical path stays the lookup key.
fn detect_skill_read(
    name: &str,
    arguments: &str,
    skill_locations: &std::collections::HashMap<PathBuf, crate::config::skills::Skill>,
) -> Option<SkillRead> {
    if name != "Read" {
        return None;
    }
    if !arguments.contains("SKILL.md") {
        return None;
    }
    let path_str = read_extract_path(arguments)?;
    let path = PathBuf::from(&path_str).canonicalize().ok()?;
    let skill = skill_locations.get(&path)?;
    Some(SkillRead {
        name: skill.name.clone(),
        path,
    })
}

/// Emit `SkillActivated` telemetry once per skill per session.
fn activate_skill_if_first(
    read: &SkillRead,
    activated_skills: &std::sync::Mutex<std::collections::HashSet<String>>,
) -> Option<SkillActivation> {
    let mut activated = activated_skills
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    activated.insert(read.name.clone()).then(|| {
        tracing::info!("Skill '{}' activated", read.name);
        SkillActivation {
            name: read.name.clone(),
            path: read.path.clone(),
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
    verdict: &TerminationVerdict,
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
    if let Some(label) = verdict.diagnostic() {
        detail.push_str(" Provider termination: ");
        detail.push_str(label);
        detail.push('.');
    }
    if let Some(session_id) = resume_session_id {
        detail.push_str(&resume_hint(session_id));
    }
    CutOffError::new(detail)
}

/// Build the neutral error for a discarded pressure response.
///
/// It states the evidence the classifier established and the remedy, never a
/// truncation cause. Resuming is not the remedy: the resumed request re-sends
/// the same over-window input, so it would be discarded again. The session ID
/// stays for inspection.
fn context_pressure_error(session_id: uuid::Uuid, pressure: ContextPressure) -> CutOffError {
    let detail = match pressure {
        ContextPressure::MeasuredInput {
            input_tokens,
            context_window,
        } => format!(
            "The model's response was discarded because the reported input \
             ({input_tokens} tokens) exceeded the configured context window \
             ({context_window} tokens). Raise or remove the model's `context_window`, \
             or start a new session; resuming this session re-sends the same input."
        ),
        ContextPressure::ProviderInputOverflow {
            input_tokens,
            context_limit,
        } => format!(
            "The model's response was discarded because the provider rejected the \
             request: its input ({input_tokens} tokens) did not fit the context limit \
             ({context_limit} tokens). Start a new session; resuming this session \
             re-sends the same input."
        ),
        ContextPressure::ProviderContextFailure { code } => format!(
            "The model's response was discarded because the provider reported a \
             context-length failure ({code}). Start a new session; resuming this \
             session re-sends the same input."
        ),
    };
    CutOffError::new(format!("{detail} Session: {session_id}."))
}

/// Extract a typed context-pressure verdict from a terminal provider error.
///
/// HTTP failures carry the shared parser's verdict in `ApiError`; in-band
/// Responses failures carry a bounded code that must be recognized before the
/// error is treated as recoverable pressure.
fn pressure_from_error(error: &anyhow::Error) -> Option<ContextPressure> {
    if let Some(api_error) = error.downcast_ref::<crate::exit_code::ApiError>()
        && let Some(overflow) = api_error.input_overflow
    {
        return Some(ContextPressure::ProviderInputOverflow {
            input_tokens: overflow.input_tokens,
            context_limit: overflow.context_limit,
        });
    }

    let failed = error.downcast_ref::<crate::clients::responses::ResponsesStreamFailed>()?;
    let metadata = failed.metadata();
    let code = context_failure_code(
        metadata.error_code.as_deref(),
        metadata.error_type.as_deref(),
    )?;
    Some(ContextPressure::ProviderContextFailure { code })
}

/// The sentence offering an explicit continuation of the session.
fn resume_hint(session_id: uuid::Uuid) -> String {
    format!(" To continue this session explicitly, run: cake --resume {session_id} \"try again\"")
}
