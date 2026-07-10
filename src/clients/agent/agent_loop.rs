use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crate::clients::agent::{Agent, TurnResult};
use crate::clients::backend::FinalOutputConstraint;
use crate::clients::tools::{Tool, ToolRegistry, read_extract_path, schedule_tool_calls};
use crate::config::output_schema::{OutputSchema, OutputSchemaError};
use crate::hooks::{HookRunner, ToolHookPlan};
use crate::session_telemetry::ToolCallTelemetry;
use crate::types::{ConversationItem, CutOffError, SessionRecord};

/// Maximum number of corrective turns after a final message fails
/// output-schema validation.
const MAX_SCHEMA_CORRECTION_TURNS: u32 = 2;

type FunctionCall = (String, String, String);

#[derive(Debug, Clone)]
struct ToolRunResult {
    call_id: String,
    output: String,
    skill_activation: Option<SkillActivation>,
    telemetry: ToolCallTelemetry,
}

/// Result of checking whether a Read tool call targeted a known skill path.
#[derive(Debug, Clone)]
struct SkillActivation {
    name: String,
    path: PathBuf,
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
    }
}

impl Agent {
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
        let user_item = self.conversation.push_user_message(content);
        self.stream_item(&user_item)?;

        // History index where this turn's items begin. Resumed and multi-turn
        // histories carry earlier assistant messages; cut-off detection must
        // only consider items produced by the current turn.
        let turn_start = self.conversation.history().len();

        // Output-schema correction state: when the final message fails
        // validation, the loop re-enters with tools disabled for at most
        // MAX_SCHEMA_CORRECTION_TURNS corrective turns.
        let mut corrections_used: u32 = 0;
        let mut in_correction_mode = false;

        // Agent loop: continue until model stops making tool calls
        loop {
            let TurnResult { items, usage } = self
                .complete_turn_with_output_schema_fallback(in_correction_mode)
                .await?;

            // Count every completed API turn unconditionally; accumulate usage separately.
            self.turn_count += 1;
            self.accumulate_usage(usage.as_ref());

            // Extract owned function call data before moving items into history
            let function_calls = Self::function_calls_from_items(&items);

            self.stream_turn_items(&items)?;

            // Move items into history
            self.conversation.extend_turn_items(items);

            // If no function calls, resolve the message; with a schema
            // attached, the message must validate before it is returned.
            if function_calls.is_empty() {
                if let Some(document) = self.resolve_final_message_or_correct(
                    &mut corrections_used,
                    &mut in_correction_mode,
                    turn_start,
                )? {
                    return Ok(document);
                }
                continue;
            }

            // Correction turns offer no tools, so any function calls here come
            // from a misbehaving provider. Do not execute them; treat the turn
            // as a failed validation attempt.
            if in_correction_mode {
                self.record_correction_tool_call_failure(
                    &mut corrections_used,
                    &mut in_correction_mode,
                )?;
                continue;
            }

            let results = self.execute_function_calls(function_calls).await?;
            self.record_tool_results(results)?;

            // Loop continues - send next request with tool results included
        }
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
                async move {
                    let plan = if let Some(runner) = hook_runner {
                        runner.pre_tool_use(&name, &call_id, &arguments).await?
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
        let groups = schedule_tool_calls(&self.tools, self.tool_context.as_ref(), tool_plans);
        let group_futures = groups.into_iter().map(|group| async move {
            let mut results = Vec::with_capacity(group.len());
            for call in group {
                let result = self.run_tool_call(call.call_id, call.name, call.plan).await;
                results.push((call.index, result));
            }
            results
        });
        let mut results: Vec<(usize, ToolRunResult)> = futures::future::join_all(group_futures)
            .await
            .into_iter()
            .flatten()
            .collect();
        results.sort_unstable_by_key(|(index, _)| *index);
        results.into_iter().map(|(_, result)| result).collect()
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
                    .execute(Arc::clone(&self.tool_context), &name, &arguments)
                    .await;
                let hook_result = result
                    .as_ref()
                    .map(|result| result.output.clone())
                    .map_err(std::clone::Clone::clone);

                let post_context = post_tool_context(
                    self.hook_runner.as_ref(),
                    &name,
                    &call_id,
                    &arguments,
                    &hook_result,
                )
                .await;

                let was_error = result.is_err();
                let mut output = match result {
                    Ok(result) => result.output,
                    Err(error) => format!("Error: {error}"),
                };

                let skill_activation = if was_error {
                    None
                } else {
                    detect_skill_activation(
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
                }
            },
        }
    }

    fn record_tool_results(&mut self, results: Vec<ToolRunResult>) -> anyhow::Result<()> {
        for result in results {
            self.append_tool_call_telemetry(result.telemetry);
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
            self.stream_item(&item)?;
        }
        Ok(())
    }

    async fn complete_turn_with_output_schema_fallback(
        &mut self,
        in_correction_mode: bool,
    ) -> anyhow::Result<TurnResult> {
        loop {
            let constraint_attached = self.native_constraint_attached(in_correction_mode);
            match self.complete_turn(in_correction_mode).await {
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

    const fn native_constraint_attached(&self, in_correction_mode: bool) -> bool {
        in_correction_mode && self.native_constraint_enabled && self.output_schema.is_some()
    }

    fn resolve_final_message_or_correct(
        &mut self,
        corrections_used: &mut u32,
        in_correction_mode: &mut bool,
        turn_start: usize,
    ) -> anyhow::Result<Option<String>> {
        let Some(message) = self.conversation.resolve_assistant_message_from(turn_start) else {
            let turn_items = self
                .conversation
                .history()
                .get(turn_start..)
                .unwrap_or_default();
            return Err(cut_off_error(turn_items).into());
        };
        let Some(schema) = self.output_schema.clone() else {
            return Ok(Some(message));
        };
        match validate_final_message(&schema, &message) {
            Ok(document) => Ok(Some(document)),
            Err(detail) => {
                self.push_schema_correction(corrections_used, in_correction_mode, detail)?;
                Ok(None)
            },
        }
    }

    fn record_correction_tool_call_failure(
        &mut self,
        corrections_used: &mut u32,
        in_correction_mode: &mut bool,
    ) -> anyhow::Result<()> {
        self.push_schema_correction(
            corrections_used,
            in_correction_mode,
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
        in_correction_mode: &mut bool,
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
        *in_correction_mode = true;
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
    pub(super) async fn complete_turn(
        &mut self,
        in_correction_mode: bool,
    ) -> anyhow::Result<TurnResult> {
        let tool_definitions = tool_definitions_for_turn(&self.tools, in_correction_mode);
        let constraint = final_output_constraint_for_turn(
            self.output_schema.as_deref(),
            self.native_constraint_attached(in_correction_mode),
        );
        let config = &self.config;
        let session_id = self.session_id;
        let history = self.conversation.history();
        let turn_index = self.turn_count.saturating_add(1);
        let mut telemetry_events = Vec::new();
        let result = self
            .runner
            .complete_turn(
                config,
                session_id,
                turn_index,
                history,
                tool_definitions,
                constraint,
                |event| {
                    telemetry_events.push(event);
                },
            )
            .await;
        for event in telemetry_events {
            self.append_runner_telemetry(event);
        }
        result
    }

    fn function_calls_from_items(items: &[ConversationItem]) -> Vec<FunctionCall> {
        items
            .iter()
            .filter_map(|item| {
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
            })
            .collect()
    }

    fn stream_turn_items(&mut self, items: &[ConversationItem]) -> anyhow::Result<()> {
        for item in items {
            self.stream_item(item)?;
        }
        Ok(())
    }
}

fn is_native_constraint_rejection(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<crate::exit_code::ApiError>()
        .is_some_and(|api_error| api_error.status == 400)
}

fn tool_definitions_for_turn(tools: &ToolRegistry, in_correction_mode: bool) -> &[Tool] {
    if in_correction_mode {
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

async fn post_tool_context(
    hook_runner: Option<&Arc<HookRunner>>,
    name: &str,
    call_id: &str,
    arguments: &str,
    hook_result: &Result<String, String>,
) -> Option<String> {
    let runner = hook_runner?;
    match runner
        .post_tool_use(name, call_id, arguments, hook_result)
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
fn cut_off_error(turn_items: &[ConversationItem]) -> CutOffError {
    let detail = if turn_items.is_empty() {
        "No response was received from the model.".to_string()
    } else if turn_items
        .iter()
        .any(|item| matches!(item, ConversationItem::Reasoning { .. }))
    {
        "The model's response was cut off during reasoning.".to_string()
    } else {
        "The model's response was incomplete. No final message was received.".to_string()
    };
    CutOffError::new(detail)
}
