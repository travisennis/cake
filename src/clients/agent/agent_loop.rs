use std::sync::Arc;
use std::time::Instant;

use futures::FutureExt;

use crate::clients::agent::{Agent, TurnResult};
use crate::clients::skill_dedup::{SkillActivation, execute_tool_with_skill_dedup};
use crate::clients::tools::{ScheduledToolPlan, reject_duplicate_mutating_tool_calls};
use crate::hooks::ToolHookPlan;
use crate::session_telemetry::ToolCallTelemetry;
use crate::types::{ConversationItem, SessionRecord};

type FunctionCall = (String, String, String);

#[derive(Debug, Clone)]
struct ToolRunResult {
    call_id: String,
    output: String,
    skill_activation: Option<SkillActivation>,
    telemetry: ToolCallTelemetry,
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
    #[expect(
        clippy::too_many_lines,
        reason = "agent send loop orchestrates tool execution and retry logic"
    )]
    pub async fn send(&mut self, content: String) -> anyhow::Result<Option<String>> {
        let user_item = self.conversation.push_user_message(content);
        self.stream_item(&user_item)?;

        // Agent loop: continue until model stops making tool calls
        loop {
            let TurnResult { items, usage } = self.complete_turn().await?;

            // Accumulate usage
            self.accumulate_usage(usage.as_ref());

            // Extract owned function call data before moving items into history
            let function_calls = Self::function_calls_from_items(&items);

            self.stream_turn_items(&items)?;

            // Move items into history
            self.conversation.extend_turn_items(items);

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
            let tool_plans = reject_duplicate_mutating_tool_calls(
                &self.tools,
                self.tool_context.as_ref(),
                tool_plans,
            );

            // Record hook-blocked tool calls as permission denials before
            // spawning the async execution futures.
            for (call_id, name, plan) in &tool_plans {
                if let ScheduledToolPlan::Hook(ToolHookPlan::Block { reason, .. }) = plan {
                    self.permission_denials
                        .push(format!("{name}({call_id}): {reason}"));
                }
            }

            let skill_locations = Arc::clone(&self.skill_locations);
            let skill_activations = Arc::clone(&self.skill_activations);
            let tools = &self.tools;
            let tool_context = Arc::clone(&self.tool_context);
            let turn_index = self.turn_count;
            let futures = tool_plans.into_iter().map(|(call_id, name, plan)| {
                let skill_locations = Arc::clone(&skill_locations);
                let skill_activations = Arc::clone(&skill_activations);
                let tool_context = Arc::clone(&tool_context);
                let hook_runner = self.hook_runner.clone();
                match plan {
                    ScheduledToolPlan::RejectedDuplicateMutation { output } => async move {
                        immediate_tool_error_result(&name, &call_id, output, turn_index)
                    }
                    .boxed(),
                    ScheduledToolPlan::Hook(ToolHookPlan::Block {
                        reason,
                        additional_context,
                    }) => async move {
                        let output = format!("Hook blocked tool execution: {reason}");
                        let output = append_hook_context(output, &additional_context);
                        immediate_tool_error_result(&name, &call_id, output, turn_index)
                    }
                    .boxed(),
                    ScheduledToolPlan::Hook(ToolHookPlan::Execute {
                        arguments,
                        prefix_notice,
                        additional_context,
                    }) => async move {
                        let start = Instant::now();
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

                        let was_error = result.is_err();
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

                        let duration_ms =
                            start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
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
                    }
                    .boxed(),
                }
            });

            let results = futures::future::join_all(futures).await;

            // Add results to history in order
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

            // Loop continues - send next request with tool results included
        }
    }

    /// Execute a single API turn with retry logic.
    pub(super) async fn complete_turn(&mut self) -> anyhow::Result<TurnResult> {
        let tool_definitions = self.tools.definitions();
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
