use super::*;
use crate::config::model::{ApiType, ResolvedModelConfig};
use crate::types::{InputTokensDetails, OutputTokensDetails};

fn test_resolved_model_config(api_type: ApiType, base_url: &str) -> ResolvedModelConfig {
    ResolvedModelConfig {
        model_config: crate::config::model::ModelConfig {
            model: "test-model".to_string(),
            api_type,
            base_url: base_url.to_string(),
            api_key_env: "TEST_API_KEY".to_string(),
            provider: None,
            provider_headers: None,
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

fn test_agent_for(api_type: ApiType, base_url: &str) -> Agent {
    Agent::new(
        test_resolved_model_config(api_type, base_url),
        &[(Role::System, "test system prompt".to_string())],
    )
    .with_session_id(uuid::uuid!("550e8400-e29b-41d4-a716-446655440000"))
    .with_task_id(uuid::uuid!("550e8400-e29b-41d4-a716-446655440001"))
    .with_tools(crate::clients::tools::ToolRegistry::empty())
}

fn test_agent() -> Agent {
    test_agent_for(ApiType::ChatCompletions, "https://api.example.com")
}

#[test]
fn read_only_tool_context_removes_edit_and_write() {
    let mut context = ToolContext::from_current_process();
    context.sandbox_policy = SandboxPolicy::ReadOnly;
    let agent = Agent::new(
        test_resolved_model_config(ApiType::ChatCompletions, "https://api.example.com"),
        &[(Role::System, "test system prompt".to_string())],
    )
    .with_tool_context(Arc::new(context));

    assert_eq!(agent.tool_names(), vec!["Bash", "Read"]);
}

#[test]
fn workspace_write_tool_context_keeps_all_tools() {
    let agent = Agent::new(
        test_resolved_model_config(ApiType::ChatCompletions, "https://api.example.com"),
        &[(Role::System, "test system prompt".to_string())],
    )
    .with_tool_context(Arc::new(ToolContext::from_current_process()));

    assert_eq!(agent.tool_names(), vec!["Bash", "Edit", "Read", "Write"]);
}

fn test_toolbox_tool() -> crate::config::toolbox::ToolboxTool {
    crate::config::toolbox::ToolboxTool {
        registered_name: "tb__run_tests".to_string(),
        original_name: "run_tests".to_string(),
        path: std::path::PathBuf::from("/tools/run_tests"),
        description: "Run the test suite.".to_string(),
        parameters: serde_json::json!({ "type": "object", "properties": {} }),
        format: crate::config::toolbox::ToolboxFormat::Json,
        timeout_secs: 60,
    }
}

#[test]
fn toolbox_tools_register_after_builtins() {
    let agent = Agent::new(
        test_resolved_model_config(ApiType::ChatCompletions, "https://api.example.com"),
        &[(Role::System, "test system prompt".to_string())],
    )
    .with_tool_context(Arc::new(ToolContext::from_current_process()))
    .with_toolbox_tools(vec![test_toolbox_tool()]);

    assert_eq!(
        agent.tool_names(),
        vec!["Bash", "Edit", "Read", "Write", "tb__run_tests"]
    );
}

#[test]
fn read_only_tool_context_skips_toolbox_tools_regardless_of_order() {
    let mut context = ToolContext::from_current_process();
    context.sandbox_policy = SandboxPolicy::ReadOnly;
    let context = Arc::new(context);

    // Read-only context applied first: registration is skipped.
    let agent = Agent::new(
        test_resolved_model_config(ApiType::ChatCompletions, "https://api.example.com"),
        &[(Role::System, "test system prompt".to_string())],
    )
    .with_tool_context(Arc::clone(&context))
    .with_toolbox_tools(vec![test_toolbox_tool()]);
    assert_eq!(agent.tool_names(), vec!["Bash", "Read"]);

    // Read-only context applied second: registered entries are stripped.
    let agent = Agent::new(
        test_resolved_model_config(ApiType::ChatCompletions, "https://api.example.com"),
        &[(Role::System, "test system prompt".to_string())],
    )
    .with_toolbox_tools(vec![test_toolbox_tool()])
    .with_tool_context(context);
    assert_eq!(agent.tool_names(), vec!["Bash", "Read"]);
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
    // accumulate_usage no longer increments turn_count; the agent loop does.
    assert_eq!(agent.turn_count, 0);
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
    // accumulate_usage no longer increments turn_count; the agent loop does.
    assert_eq!(agent.turn_count, 0);
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
fn emit_task_complete_record_with_permission_denials() {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let captured_clone = captured.clone();
    let mut agent = test_agent()
        .with_streaming_json(move |json| {
            *captured_clone.lock().unwrap() = json.to_string();
        })
        .with_permission_denials(vec!["Bash(call-1): blocked by hook".to_string()]);
    agent
        .emit_task_complete_record(TaskOutcome::Success { result: None }, 1000)
        .unwrap();
    drop(agent);
    let json: serde_json::Value = serde_json::from_str(&captured.lock().unwrap()).unwrap();
    assert_eq!(json["type"], "task_complete");
    assert_eq!(
        json["permission_denials"],
        serde_json::json!(["Bash(call-1): blocked by hook"])
    );
}

#[test]
fn judge_denial_label_maps_only_real_denials() {
    use crate::clients::agent::agent_loop::judge_denial_label;
    use crate::session_telemetry::{CompensationEventTelemetry, CompensationKind};

    // A block verdict is a denial carrying its verdict code.
    let block =
        CompensationEventTelemetry::judge_verdict("block", Some("git-force-push"), 42, false);
    assert_eq!(
        judge_denial_label(&block).as_deref(),
        Some("judge block: git-force-push")
    );

    // A fail-closed denial carries its failure class.
    let fail_closed = CompensationEventTelemetry::judge_fail_closed("transport");
    assert_eq!(
        judge_denial_label(&fail_closed).as_deref(),
        Some("judge fail-closed: transport")
    );

    // Everything else is not a denial: the command ran.
    let warn =
        CompensationEventTelemetry::judge_verdict("warn", Some("rg-replace-footgun"), 3, false);
    assert_eq!(judge_denial_label(&warn), None);
    let allow = CompensationEventTelemetry::judge_verdict("allow", None, 2, false);
    assert_eq!(judge_denial_label(&allow), None);
    let bypass = CompensationEventTelemetry::judge_bypass();
    assert_eq!(judge_denial_label(&bypass), None);

    // An allowlist-overridden block ran, so it is not a denial.
    let overridden =
        CompensationEventTelemetry::judge_verdict("block", Some("destructive-rm"), 5, true);
    assert_eq!(judge_denial_label(&overridden), None);

    // Non-judge compensations are not denials.
    let unrelated =
        CompensationEventTelemetry::new(CompensationKind::OutputTruncation, Some("Read".into()));
    assert_eq!(judge_denial_label(&unrelated), None);
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
        session_id: agent.session_id().to_string(),
        task_id: agent.task_id().to_string(),
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
fn history_repair_records_persist_and_stream() {
    let persisted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let persisted_clone = persisted.clone();
    let streamed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let streamed_clone = streamed.clone();

    let history = vec![
        ConversationItem::Message {
            role: Role::User,
            content: "list files".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
        ConversationItem::FunctionCall {
            id: "fc-1".to_string(),
            call_id: "call-1".to_string(),
            name: "Bash".to_string(),
            arguments: r#"{"command":"ls"}"#.to_string(),
            timestamp: None,
        },
    ];

    let mut agent = test_agent()
        .with_history(history)
        .unwrap()
        .with_persist_callback(move |record| {
            persisted_clone.lock().unwrap().push(record.clone());
            Ok(())
        })
        .with_streaming_json(move |json| {
            streamed_clone.lock().unwrap().push(json.to_string());
        });

    agent.emit_history_repair_records().unwrap();

    let persisted = persisted.lock().unwrap();
    assert_eq!(persisted.len(), 1);
    assert!(matches!(
        &persisted[0],
        SessionRecord::FunctionCallOutput(data)
            if data.call_id == "call-1"
                && data.output == "not executed: the previous cake process ended before \
                    Bash(call-1) recorded a result. Assume the tool did not run, and call \
                    it again if its result is still needed."
    ));
    drop(persisted);

    let streamed = streamed.lock().unwrap();
    assert_eq!(streamed.len(), 1);
    let record: serde_json::Value = serde_json::from_str(&streamed[0]).unwrap();
    drop(streamed);
    assert_eq!(record["type"], "function_call_output");
    assert_eq!(record["call_id"], "call-1");
}

#[test]
fn history_repair_records_are_emitted_once() {
    let persisted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let persisted_clone = persisted.clone();

    let history = vec![ConversationItem::FunctionCall {
        id: "fc-1".to_string(),
        call_id: "call-1".to_string(),
        name: "Bash".to_string(),
        arguments: "{}".to_string(),
        timestamp: None,
    }];

    let mut agent = test_agent()
        .with_history(history)
        .unwrap()
        .with_persist_callback(move |record| {
            persisted_clone.lock().unwrap().push(record.clone());
            Ok(())
        });

    agent.emit_history_repair_records().unwrap();
    agent.emit_history_repair_records().unwrap();

    assert_eq!(persisted.lock().unwrap().len(), 1);
}

#[test]
fn history_repair_records_are_absent_for_matched_history() {
    let persisted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let persisted_clone = persisted.clone();

    let history = vec![
        ConversationItem::FunctionCall {
            id: "fc-1".to_string(),
            call_id: "call-1".to_string(),
            name: "Bash".to_string(),
            arguments: "{}".to_string(),
            timestamp: None,
        },
        ConversationItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: "ok".to_string(),
            timestamp: None,
        },
    ];

    let mut agent = test_agent()
        .with_history(history)
        .unwrap()
        .with_persist_callback(move |record| {
            persisted_clone.lock().unwrap().push(record.clone());
            Ok(())
        });

    agent.emit_history_repair_records().unwrap();

    assert!(persisted.lock().unwrap().is_empty());
    assert_eq!(agent.history().len(), 3);
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

/// Error handling tests using wiremock for HTTP mocking
#[cfg(test)]
mod error_tests {
    use super::*;
    use crate::clients::agent::agent_loop::SEMANTIC_RECOVERY_PROMPT;
    use crate::clients::judge::JudgeContext;
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

    #[derive(Debug)]
    struct SemanticRecoveryMatcher {
        call_id: String,
        output: String,
    }

    impl Match for SemanticRecoveryMatcher {
        fn matches(&self, request: &Request) -> bool {
            let Ok(body) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
                return false;
            };
            let Some(items) = body["input"].as_array() else {
                return false;
            };

            body.get("tools").is_none()
                && items.iter().any(|item| {
                    item["type"] == "function_call_output"
                        && item["call_id"] == self.call_id
                        && item["output"] == self.output
                })
                && items.iter().any(|item| item["type"] == "reasoning")
                && items.iter().any(|item| {
                    item["role"] == "user" && item["content"][0]["text"] == SEMANTIC_RECOVERY_PROMPT
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
        let mut agent = test_agent_with_url(&mock_server.uri())
            .with_streaming_json(move |json| {
                streamed_clone.lock().unwrap().push(json.to_string());
            })
            .with_tools(crate::clients::tools::read_tool_registry());

        let result = agent.send("run a command".to_string()).await.unwrap();

        assert_eq!(result, "done");
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
    async fn same_file_edits_in_one_turn_run_sequentially_in_issue_order() {
        let mock_server = MockServer::start().await;
        let dir = tempfile::TempDir::new_in(std::env::current_dir().unwrap()).unwrap();
        let file_path = dir.path().join("serialized.txt");
        std::fs::write(&file_path, "alpha\n").unwrap();

        let first_arguments = serde_json::json!({
            "path": file_path,
            "edits": [{ "old_text": "alpha", "new_text": "alpha\nbeta" }]
        })
        .to_string();
        // Matches only after the first edit has been applied.
        let second_arguments = serde_json::json!({
            "path": file_path,
            "edits": [{ "old_text": "beta", "new_text": "beta\ngamma" }]
        })
        .to_string();

        let two_edit_response = serde_json::json!({
            "id": "resp-tool",
            "output": [
                {
                    "type": "function_call",
                    "id": "fc-1",
                    "call_id": "call-1",
                    "name": "Edit",
                    "arguments": first_arguments
                },
                {
                    "type": "function_call",
                    "id": "fc-2",
                    "call_id": "call-2",
                    "name": "Edit",
                    "arguments": second_arguments
                }
            ],
            "usage": { "input_tokens": 1, "output_tokens": 1, "total_tokens": 2 }
        });

        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(two_edit_response))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(loop_final_response()))
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_with_url(&mock_server.uri())
            .with_tools(crate::clients::tools::default_tool_registry());

        let result = agent.send("edit the file twice".to_string()).await.unwrap();

        assert_eq!(result, "done");
        // The second edit operated on the first edit's result.
        assert_eq!(
            std::fs::read_to_string(&file_path).unwrap(),
            "alpha\nbeta\ngamma\n"
        );

        // Both calls succeeded and their outputs are recorded in issue order
        // with per-call attribution.
        let outputs: Vec<(&str, &str)> = agent
            .history()
            .iter()
            .filter_map(|item| match item {
                ConversationItem::FunctionCallOutput {
                    call_id, output, ..
                } => Some((call_id.as_str(), output.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].0, "call-1");
        assert_eq!(outputs[1].0, "call-2");
        assert!(
            !outputs[0].1.starts_with("Error:"),
            "first edit should succeed: {}",
            outputs[0].1
        );
        assert!(
            !outputs[1].1.starts_with("Error:"),
            "second edit should see the first edit's result: {}",
            outputs[1].1
        );
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
                session_writer: None,
                hook_event_sink: None,
                cwd: tmp.path().to_path_buf(),
                model: "test-model".to_string(),
            },
        ));
        let mut agent = test_agent_with_url(&mock_server.uri()).with_hook_runner(runner);

        let result = agent.send("run a command".to_string()).await.unwrap();

        assert_eq!(result, "Hello!");
        assert!(agent.history().iter().any(|item| matches!(
            item,
            ConversationItem::FunctionCallOutput { output, .. }
                if output.starts_with("Hook blocked tool execution:")
                    && output.contains("blocked")
        )));

        // Verify the blocked tool call was recorded as a permission denial.
        assert_eq!(agent.permission_denials().len(), 1);
        assert!(agent.permission_denials()[0].contains("Bash(call-1):"));
        assert!(agent.permission_denials()[0].contains("blocked"));

        // No dangling function_call items remain.
        assert_no_dangling_function_calls(agent.history());
    }

    /// Build a judge context whose judge client points at a wiremock server,
    /// mirroring the helper in `src/clients/tools/bash_tests.rs`. The agent
    /// runs the Responses API against `/responses`; the judge runs Chat
    /// Completions against `/chat/completions` on the same server.
    fn judge_context(mock_server: &MockServer) -> std::sync::Arc<JudgeContext> {
        use crate::config::model::ModelConfig;
        use std::collections::HashMap;

        let model_config = ModelConfig {
            model: "judge/model".to_string(),
            api_type: ApiType::ChatCompletions,
            base_url: mock_server.uri(),
            api_key_env: "JUDGE_TEST_KEY".to_string(),
            provider: None,
            provider_headers: None,
            temperature: Some(0.0),
            top_p: None,
            max_output_tokens: Some(128),
            reasoning_effort: None,
            reasoning_summary: None,
            reasoning_max_tokens: None,
            providers: vec![],
        };
        std::sync::Arc::new(JudgeContext {
            settings: crate::config::settings::JudgeSettings::default(),
            agent_model: crate::config::model::ResolvedModelConfig {
                model_config,
                api_key: "test-key".to_string(),
            },
            models: HashMap::new(),
            client: std::sync::OnceLock::new(),
            record_attempt: None,
        })
    }

    /// Chat-completions response carrying one judge verdict for a mock server.
    fn judge_chat_response(verdict_json: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "chatcmpl-judge",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": verdict_json },
                "finish_reason": "stop"
            }]
        })
    }

    /// Build an agent whose Bash tool runs behind a judge context pointing at
    /// `mock_server`, with the default tool registry so Bash exists.
    fn test_agent_with_judge(mock_server: &MockServer) -> Agent {
        test_agent_with_url(&mock_server.uri())
            .with_tools(crate::clients::tools::default_tool_registry())
            .with_tool_context(Arc::new(
                ToolContext::from_current_process().with_judge(Some(judge_context(mock_server))),
            ))
    }

    #[tokio::test]
    async fn judge_block_records_permission_denial_with_verdict_code() {
        // A judge `block` verdict prevents the Bash command from spawning and
        // appears in `task_complete.permission_denials` carrying the verdict
        // code, through the same path hook denials use (#123).
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
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(judge_chat_response(
                r#"{"verdict":"block","code":"git-force-push","message":"Prefer push --force-with-lease."}"#,
            )))
            .mount(&mock_server)
            .await;

        let captured = Arc::new(Mutex::new(String::new()));
        let captured_clone = captured.clone();
        let telemetry_dir = tempfile::TempDir::new().unwrap();
        let telemetry_path = telemetry_dir.path().join("sidecar.ndjson");
        let mut agent = test_agent_with_judge(&mock_server)
            .with_streaming_json(move |json| {
                *captured_clone.lock().unwrap() = json.to_string();
            })
            .with_session_telemetry(
                SessionTelemetryWriter::open(&telemetry_path).unwrap(),
                uuid::Uuid::new_v4(),
            );
        let result = agent.send("run a command".to_string()).await.unwrap();

        assert_eq!(result, "Hello!");
        assert_eq!(
            agent.permission_denials(),
            &["Bash(call-1): judge block: git-force-push".to_string()]
        );
        // The blocked call surfaced the judge's reason to the model.
        assert!(agent.history().iter().any(|item| matches!(
            item,
            ConversationItem::FunctionCallOutput { output, .. }
                if output.contains("BLOCKED") && output.contains("Prefer push --force-with-lease.")
        )));

        // The denial reaches `task_complete.permission_denials` with its
        // verdict code (#123).
        agent
            .emit_task_complete_record(TaskOutcome::Success { result: None }, 1000)
            .unwrap();
        drop(agent);
        let json: serde_json::Value = serde_json::from_str(&captured.lock().unwrap()).unwrap();
        assert_eq!(json["type"], "task_complete");
        assert_eq!(
            json["permission_denials"],
            serde_json::json!(["Bash(call-1): judge block: git-force-push"])
        );

        // The sidecar records the verdict as metadata only: the code and
        // latency, never the command or reason text.
        let sidecar = std::fs::read_to_string(&telemetry_path).unwrap();
        let records: Vec<serde_json::Value> = sidecar
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let compensation = records
            .iter()
            .find(|record| record["type"] == "compensation")
            .expect("judge verdict must reach the telemetry sidecar");
        assert_eq!(compensation["kind"], "judge_verdict");
        assert_eq!(compensation["detail"], "block:git-force-push");
        assert!(compensation.get("latency_ms").is_some());
        assert!(compensation.get("overridden").is_none());
        let attempt = records
            .iter()
            .find(|record| record["type"] == "judge_attempt")
            .expect("judge provider attempt must reach the telemetry sidecar");
        assert_eq!(attempt["attempt"], 1);
        assert_eq!(attempt["retry_ordinal"], 0);
        assert_eq!(attempt["status_code"], 200);
        assert_eq!(attempt["terminal_class"], "verdict");
        assert_eq!(attempt["tool_count"], 0);
        assert!(attempt["usage"].is_null());
        let attempt_index = records
            .iter()
            .position(|record| record["type"] == "judge_attempt")
            .unwrap();
        let compensation_index = records
            .iter()
            .position(|record| {
                record["type"] == "compensation" && record["kind"] == "judge_verdict"
            })
            .unwrap();
        assert!(attempt_index < compensation_index);
        assert!(
            !sidecar.contains("printf"),
            "telemetry must not carry the command text, got: {sidecar}"
        );
    }

    #[tokio::test]
    async fn judge_fail_closed_records_distinct_permission_denial() {
        // A judge failure (here: HTTP 500) fails closed and is recorded as a
        // denial distinct from both hook denials and verdict blocks.
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
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_with_judge(&mock_server);
        let result = agent.send("run a command".to_string()).await.unwrap();

        assert_eq!(result, "Hello!");
        assert_eq!(
            agent.permission_denials(),
            &["Bash(call-1): judge fail-closed: transport".to_string()]
        );
    }

    // =========================================================================
    // HTTP Error Response Tests (Non-retryable 4xx errors)
    // =========================================================================

    /// Responses API response whose output contains no items at all.
    fn empty_output_response() -> serde_json::Value {
        serde_json::json!({
            "id": "resp-empty",
            "output": [],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 0,
                "total_tokens": 10
            }
        })
    }

    /// Responses API response with reasoning but no final assistant message.
    fn reasoning_only_response() -> serde_json::Value {
        serde_json::json!({
            "id": "resp-reasoning",
            "output": [
                {
                    "type": "reasoning",
                    "id": "r-1",
                    "summary": ["thinking..."]
                }
            ],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "total_tokens": 15
            }
        })
    }

    async fn mount_response(mock_server: &MockServer, body: serde_json::Value) {
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(mock_server)
            .await;
    }

    fn prior_turn_history() -> Vec<ConversationItem> {
        vec![
            ConversationItem::Message {
                role: Role::User,
                content: "earlier question".to_string(),
                id: None,
                status: None,
                timestamp: None,
            },
            ConversationItem::Message {
                role: Role::Assistant,
                content: "earlier answer".to_string(),
                id: Some("msg-prior".to_string()),
                status: Some("completed".to_string()),
                timestamp: None,
            },
        ]
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the end-to-end recovery assertion covers identity, accounting, request shape, persistence, streaming, and progress together"
    )]
    #[tokio::test]
    async fn semantic_incomplete_responses_turn_recovers_in_place() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(reasoning_only_response()))
            .expect(1)
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
            .expect(1)
            .mount(&mock_server)
            .await;

        let persisted = Arc::new(Mutex::new(Vec::new()));
        let persisted_clone = Arc::clone(&persisted);
        let streamed = Arc::new(Mutex::new(Vec::new()));
        let streamed_clone = Arc::clone(&streamed);
        let progress = Arc::new(Mutex::new(Vec::<String>::new()));
        let progress_clone = Arc::clone(&progress);
        let session_id = uuid::uuid!("550e8400-e29b-41d4-a716-446655440000");
        let task_id = uuid::uuid!("550e8400-e29b-41d4-a716-446655440001");
        let mut agent = Agent::new(
            test_resolved_model_config(ApiType::Responses, &mock_server.uri()),
            &[(Role::System, "test system prompt".to_string())],
        )
        .with_session_id(session_id)
        .with_task_id(task_id)
        .with_persist_callback(move |record| {
            persisted_clone.lock().unwrap().push(record.clone());
            Ok(())
        })
        .with_streaming_json(move |json| {
            streamed_clone.lock().unwrap().push(json.to_string());
        })
        .with_progress_callback(move |message| {
            progress_clone.lock().unwrap().push(message.to_string());
        });

        let result = agent.send("hello".to_string()).await.unwrap();

        assert_eq!(result, "Hello!");
        assert_eq!(agent.session_id(), session_id);
        assert_eq!(agent.task_id(), task_id);
        assert_eq!(agent.turn_count(), 2);
        assert_eq!(agent.total_usage().input_tokens, 20);
        assert_eq!(agent.total_usage().output_tokens, 10);
        assert_eq!(agent.total_usage().total_tokens, 30);
        assert_eq!(
            agent
                .history()
                .iter()
                .map(|item| match item {
                    ConversationItem::Message { role, .. } => role.as_str(),
                    ConversationItem::Reasoning { .. } => "reasoning",
                    ConversationItem::FunctionCall { .. } => "function_call",
                    ConversationItem::FunctionCallOutput { .. } => "function_call_output",
                })
                .collect::<Vec<_>>(),
            vec!["system", "user", "reasoning", "user", "assistant"]
        );
        assert!(matches!(
            &agent.history()[3],
            ConversationItem::Message {
                role: Role::User,
                content,
                ..
            } if content == SEMANTIC_RECOVERY_PROMPT
        ));

        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        let first: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let second: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
        assert!(first["tools"].is_array());
        assert!(second.get("tools").is_none());
        assert!(second["input"].as_array().is_some_and(|items| {
            items.iter().any(|item| item["type"] == "reasoning")
                && items.iter().any(|item| {
                    item["role"] == "user" && item["content"][0]["text"] == SEMANTIC_RECOVERY_PROMPT
                })
        }));

        let persisted = persisted.lock().unwrap();
        assert_eq!(persisted.len(), 4);
        assert!(
            persisted
                .iter()
                .all(|record| !matches!(record, SessionRecord::TaskComplete { .. }))
        );
        assert!(matches!(persisted[0], SessionRecord::Message(_)));
        assert!(matches!(persisted[1], SessionRecord::Reasoning(_)));
        assert!(matches!(persisted[2], SessionRecord::Message(_)));
        assert!(matches!(persisted[3], SessionRecord::Message(_)));
        drop(persisted);
        let streamed = streamed.lock().unwrap();
        assert_eq!(streamed.len(), 4);
        assert!(
            streamed
                .iter()
                .all(|record| !record.contains("task_complete"))
        );
        drop(streamed);
        assert_eq!(
            progress.lock().unwrap().as_slice(),
            ["Retrying incomplete model turn (semantic_incomplete, attempt 1/1)"]
        );
    }

    #[tokio::test]
    async fn semantic_recovery_preserves_completed_tools_without_reexecuting_them() {
        let mock_server = MockServer::start().await;
        let fixture = loop_fixture();
        let mut tool_response = loop_tool_call_response(&fixture.read_arguments);
        tool_response["output"].as_array_mut().unwrap().insert(
            0,
            serde_json::json!({
                "type": "message",
                "id": "msg-progress",
                "status": "completed",
                "content": [{
                    "type": "output_text",
                    "text": "I am checking that now."
                }]
            }),
        );
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(body_partial_json(serde_json::json!({
                "input": [{
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": "run a command"
                    }]
                }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(tool_response))
            .expect(1)
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(FunctionCallOutputMatcher {
                call_id: "call-1".to_string(),
                output: fixture.expected_tool_output.clone(),
            })
            .respond_with(ResponseTemplate::new(200).set_body_json(reasoning_only_response()))
            .expect(1)
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(SemanticRecoveryMatcher {
                call_id: "call-1".to_string(),
                output: fixture.expected_tool_output.clone(),
            })
            .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
            .expect(1)
            .mount(&mock_server)
            .await;

        let mut agent = Agent::new(
            test_resolved_model_config(ApiType::Responses, &mock_server.uri()),
            &[(Role::System, "test system prompt".to_string())],
        );
        let result = agent.send("run a command".to_string()).await.unwrap();

        assert_eq!(result, "Hello!");
        assert_eq!(agent.turn_count(), 3);
        assert_eq!(agent.tool_call_count, 1);
        assert_eq!(
            agent
                .history()
                .iter()
                .filter(|item| matches!(item, ConversationItem::FunctionCallOutput { .. }))
                .count(),
            1
        );
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn semantic_incomplete_chat_turn_recovers_without_assistant_reasoning_replay() {
        let mock_server = MockServer::start().await;
        let reasoning = serde_json::json!({
            "id": "chatcmpl-reasoning",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "reasoning_content": "provider-private reasoning"
                },
                "finish_reason": "length"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        });
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(reasoning))
            .expect(1)
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_chat_response()))
            .expect(1)
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_chat_completions(&mock_server.uri());
        let result = agent.send("hello".to_string()).await.unwrap();

        assert_eq!(result, "Hello!");
        assert_eq!(agent.turn_count(), 2);
        let requests = mock_server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        let recovery: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
        let messages = recovery["messages"].as_array().unwrap();
        assert!(
            messages
                .iter()
                .all(|message| message["role"] != "assistant")
        );
        assert!(messages.iter().all(|message| {
            message.get("reasoning_content").is_none() || message["reasoning_content"].is_null()
        }));
        assert!(!recovery.to_string().contains("provider-private reasoning"));
        assert_eq!(
            messages.last().unwrap()["content"],
            SEMANTIC_RECOVERY_PROMPT
        );
    }

    #[tokio::test]
    async fn semantic_incomplete_content_filter_does_not_retry() {
        let mock_server = MockServer::start().await;
        let mut body = reasoning_only_response();
        body["status"] = serde_json::json!("incomplete");
        body["incomplete_details"] = serde_json::json!({"reason": "content_filter"});
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_with_url(&mock_server.uri());
        let error = agent.send("hello".to_string()).await.unwrap_err();
        let cutoff = error
            .downcast_ref::<crate::types::CutOffError>()
            .expect("expected CutOffError");

        assert_eq!(agent.turn_count(), 1);
        assert!(
            cutoff
                .detail
                .contains("Provider termination: content_filter")
        );
        assert!(!cutoff.detail.contains("cake --resume"));
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn semantic_incomplete_responses_partial_text_recovers() {
        let mock_server = MockServer::start().await;
        let partial = serde_json::json!({
            "id": "resp-partial",
            "output": [{
                "type": "message",
                "id": "msg-partial",
                "status": "incomplete",
                "content": [{
                    "type": "output_text",
                    "text": "partial answer"
                }]
            }],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "total_tokens": 15
            }
        });
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(partial))
            .expect(1)
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
            .expect(1)
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_with_url(&mock_server.uri());
        let response = agent.send("hello".to_string()).await.unwrap();

        assert_eq!(response, "Hello!");
        assert_eq!(agent.turn_count(), 2);
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn semantic_incomplete_responses_refusal_does_not_retry() {
        let mock_server = MockServer::start().await;
        let refusal = serde_json::json!({
            "id": "resp-refusal",
            "status": "completed",
            "output": [{
                "type": "message",
                "id": "msg-refusal",
                "status": "completed",
                "content": [{
                    "type": "refusal",
                    "refusal": "sensitive refusal text"
                }]
            }],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "total_tokens": 15
            }
        });
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(refusal))
            .expect(1)
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_with_url(&mock_server.uri());
        let error = agent.send("hello".to_string()).await.unwrap_err();
        let cutoff = error
            .downcast_ref::<crate::types::CutOffError>()
            .expect("expected CutOffError");

        assert_eq!(agent.turn_count(), 1);
        assert!(cutoff.detail.contains("Provider termination: failed"));
        assert!(!cutoff.detail.contains("sensitive refusal text"));
        assert!(!cutoff.detail.contains("cake --resume"));
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn semantic_incomplete_responses_failure_dominates_incomplete_item() {
        let mock_server = MockServer::start().await;
        let failed = serde_json::json!({
            "id": "resp-failed",
            "status": "failed",
            "output": [{
                "type": "reasoning",
                "id": "r-failed",
                "status": "incomplete",
                "summary": ["partial reasoning"]
            }],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "total_tokens": 15
            }
        });
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(failed))
            .expect(1)
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_with_url(&mock_server.uri());
        let error = agent.send("hello".to_string()).await.unwrap_err();
        let cutoff = error
            .downcast_ref::<crate::types::CutOffError>()
            .expect("expected CutOffError");

        assert_eq!(agent.turn_count(), 1);
        assert!(cutoff.detail.contains("Provider termination: failed"));
        assert!(!cutoff.detail.contains("cake --resume"));
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn semantic_incomplete_chat_partial_text_recovers() {
        let mock_server = MockServer::start().await;
        let partial = serde_json::json!({
            "id": "chatcmpl-partial",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "partial answer"
                },
                "finish_reason": "length"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        });
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(partial))
            .expect(1)
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_chat_response()))
            .expect(1)
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_chat_completions(&mock_server.uri());
        let response = agent.send("hello".to_string()).await.unwrap();

        assert_eq!(response, "Hello!");
        assert_eq!(agent.turn_count(), 2);
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn semantic_incomplete_chat_refusal_does_not_retry() {
        let mock_server = MockServer::start().await;
        let refusal = serde_json::json!({
            "id": "chatcmpl-refusal",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "refusal": "sensitive refusal text"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        });
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(refusal))
            .expect(1)
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_chat_completions(&mock_server.uri());
        let error = agent.send("hello".to_string()).await.unwrap_err();
        let cutoff = error
            .downcast_ref::<crate::types::CutOffError>()
            .expect("expected CutOffError");

        assert_eq!(agent.turn_count(), 1);
        assert!(cutoff.detail.contains("Provider termination: failed"));
        assert!(!cutoff.detail.contains("sensitive refusal text"));
        assert!(!cutoff.detail.contains("cake --resume"));
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn semantic_recovery_defers_stop_hook_and_task_completion_until_success() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(reasoning_only_response()))
            .expect(1)
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
            .expect(1)
            .mount(&mock_server)
            .await;

        let dir = tempfile::TempDir::new().unwrap();
        let marker = dir.path().join("stop-marker");
        let runner = Arc::new(HookRunner::new(
            LoadedHooks {
                groups: vec![HookGroup {
                    event: HookEvent::Stop,
                    matcher: HookMatcher::All,
                    hooks: vec![HookCommand {
                        command: "touch stop-marker".to_string(),
                        timeout: Duration::from_secs(2),
                        fail_closed: false,
                        status_message: None,
                        source_path: dir.path().join("hooks.json"),
                    }],
                }],
            },
            HookContext {
                session_id: uuid::Uuid::new_v4(),
                task_id: uuid::Uuid::new_v4(),
                transcript_path: None,
                session_writer: None,
                hook_event_sink: None,
                cwd: dir.path().to_path_buf(),
                model: "test-model".to_string(),
            },
        ));
        let streamed = Arc::new(Mutex::new(Vec::new()));
        let streamed_clone = Arc::clone(&streamed);
        let mut agent = test_agent_with_url(&mock_server.uri())
            .with_hook_runner(Arc::clone(&runner))
            .with_streaming_json(move |json| {
                streamed_clone.lock().unwrap().push(json.to_string());
            });

        let response = agent.send("hello".to_string()).await.unwrap();

        assert!(!marker.exists());
        assert!(
            streamed
                .lock()
                .unwrap()
                .iter()
                .all(|record| !record.contains("task_complete"))
        );

        let result: Result<String, anyhow::Error> = Ok(response);
        crate::CodingAssistant::handle_agent_turn_result(&mut agent, Some(&runner), &result, 100)
            .await
            .unwrap();

        assert!(marker.exists());
        assert_eq!(
            streamed
                .lock()
                .unwrap()
                .iter()
                .filter(|record| record.contains("task_complete"))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn cut_off_reasoning_only_response_returns_cut_off_error() {
        let mock_server = MockServer::start().await;
        mount_response(&mock_server, reasoning_only_response()).await;

        let mut agent = test_agent_with_url(&mock_server.uri());
        let err = agent.send("hello".to_string()).await.unwrap_err();

        let cutoff = err
            .downcast_ref::<crate::types::CutOffError>()
            .expect("expected CutOffError");
        assert_eq!(
            cutoff.detail,
            "The model's response was cut off during reasoning. To continue this session \
             explicitly, run: cake --resume 550e8400-e29b-41d4-a716-446655440000 \"try again\""
        );
        assert_eq!(agent.turn_count(), 2);
        assert_eq!(mock_server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn cut_off_diagnostic_includes_provider_termination_without_reasoning() {
        let mock_server = MockServer::start().await;
        let mut body = reasoning_only_response();
        body["status"] = serde_json::json!("incomplete");
        body["incomplete_details"] = serde_json::json!({"reason": "max_output_tokens"});
        mount_response(&mock_server, body).await;

        let mut agent = test_agent_with_url(&mock_server.uri());
        let err = agent.send("hello".to_string()).await.unwrap_err();
        let cutoff = err
            .downcast_ref::<crate::types::CutOffError>()
            .expect("expected CutOffError");

        assert!(cutoff.detail.contains("Provider termination: token_limit"));
        assert!(!cutoff.detail.contains("incomplete"));
        assert!(!cutoff.detail.contains("max_output_tokens"));
        assert!(!cutoff.detail.contains("thinking..."));
    }

    #[tokio::test]
    async fn cut_off_diagnostic_does_not_expose_unknown_provider_metadata() {
        let mock_server = MockServer::start().await;
        let mut body = reasoning_only_response();
        body["status"] = serde_json::json!("secret-status");
        body["incomplete_details"] =
            serde_json::json!({"reason": "private model output\nand control text"});
        mount_response(&mock_server, body).await;

        let mut agent = test_agent_with_url(&mock_server.uri());
        let err = agent.send("hello".to_string()).await.unwrap_err();
        let cutoff = err
            .downcast_ref::<crate::types::CutOffError>()
            .expect("expected CutOffError");

        assert!(cutoff.detail.contains("Provider termination: unknown"));
        assert!(!cutoff.detail.contains("secret-status"));
        assert!(!cutoff.detail.contains("private model output"));
    }

    #[tokio::test]
    async fn cut_off_in_resumed_session_is_not_masked_by_prior_assistant_message() {
        let mock_server = MockServer::start().await;
        mount_response(&mock_server, empty_output_response()).await;

        let mut agent = test_agent_with_url(&mock_server.uri())
            .with_history(prior_turn_history())
            .unwrap();
        let err = agent.send("follow-up".to_string()).await.unwrap_err();

        let cutoff = err
            .downcast_ref::<crate::types::CutOffError>()
            .expect("expected CutOffError, not the prior turn's answer");
        assert_eq!(
            cutoff.detail,
            "No response was received from the model. To continue this session explicitly, run: \
             cake --resume 550e8400-e29b-41d4-a716-446655440000 \"try again\""
        );
    }

    #[tokio::test]
    async fn cut_off_detail_ignores_prior_turn_reasoning() {
        let mock_server = MockServer::start().await;
        mount_response(&mock_server, empty_output_response()).await;

        let mut history = prior_turn_history();
        history.insert(
            1,
            ConversationItem::Reasoning {
                id: "r-prior".to_string(),
                summary: Some(vec!["earlier thinking".to_string()]),
                encrypted_content: None,
                content: None,
                timestamp: None,
            },
        );
        let mut agent = test_agent_with_url(&mock_server.uri())
            .with_history(history)
            .unwrap();
        let err = agent.send("follow-up".to_string()).await.unwrap_err();

        let cutoff = err
            .downcast_ref::<crate::types::CutOffError>()
            .expect("expected CutOffError");
        assert_eq!(
            cutoff.detail,
            "No response was received from the model. To continue this session explicitly, run: \
             cake --resume 550e8400-e29b-41d4-a716-446655440000 \"try again\"",
            "prior-turn reasoning must not mislabel the detail as cut off during reasoning"
        );
    }

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

        let result = agent.complete_turn(false).await;
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

        let result = agent.complete_turn(false).await;
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

        let result = agent.complete_turn(false).await;
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

        let result = agent.complete_turn(false).await;
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

        let result = agent.complete_turn(false).await;
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

        let mut agent = test_agent_with_url(&mock_server.uri());
        agent.history_mut().push(ConversationItem::Message {
            role: Role::User,
            content: "test".to_string(),
            id: None,
            status: None,
            timestamp: None,
        });

        let start = Instant::now();
        let result = agent.complete_turn(false).await;
        let elapsed = start.elapsed();

        assert!(result.is_ok());
        assert!(elapsed >= Duration::from_millis(900));
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

        let result = agent.complete_turn(false).await;
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

        let result = agent.complete_turn(false).await;
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

        let result = agent.complete_turn(false).await;
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

        let result = agent.complete_turn(false).await;
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

        let result = agent.complete_turn(false).await;
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

        let result = agent.complete_turn(false).await;
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

        let result = agent.complete_turn(false).await;
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

        let result = agent.complete_turn(false).await;
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

        let mut agent = test_agent_with_url(&mock_server.uri())
            .with_max_output_tokens(Some(5000))
            .with_reasoning_max_tokens(Some(4000));
        agent.history_mut().push(ConversationItem::Message {
            role: Role::User,
            content: "test".to_string(),
            id: None,
            status: None,
            timestamp: None,
        });

        let result = agent.complete_turn(false).await;
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

        let result = agent.complete_turn(false).await;
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

        let result = agent.complete_turn(false).await;
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

        let result = agent.complete_turn(false).await;
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

        let result = agent.complete_turn(false).await;
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

/// Assert that every `FunctionCall` item in `history` has a corresponding
/// `FunctionCallOutput` item with a matching `call_id`. Catches dangling
/// `function_call` items from correction-mode or hook-blocked paths.
fn assert_no_dangling_function_calls(history: &[ConversationItem]) {
    let call_ids: std::collections::HashSet<_> = history
        .iter()
        .filter_map(|item| {
            if let ConversationItem::FunctionCall { call_id, .. } = item {
                Some(call_id.as_str())
            } else {
                None
            }
        })
        .collect();
    let output_ids: std::collections::HashSet<_> = history
        .iter()
        .filter_map(|item| {
            if let ConversationItem::FunctionCallOutput { call_id, .. } = item {
                Some(call_id.as_str())
            } else {
                None
            }
        })
        .collect();
    let dangling: Vec<_> = call_ids.difference(&output_ids).copied().collect();
    assert!(
        dangling.is_empty(),
        "dangling FunctionCall items without matching FunctionCallOutput: {dangling:?}"
    );
}

/// Output-schema enforcement tests using wiremock for HTTP mocking.
#[cfg(test)]
mod output_schema_tests {
    use super::*;
    use crate::config::OutputSchema;
    use crate::config::model::ApiType;
    use crate::config::output_schema::OutputSchemaError;
    use std::sync::{Arc, Mutex};

    use wiremock::matchers::{method, path};
    use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

    fn test_schema() -> Arc<OutputSchema> {
        let raw = serde_json::json!({
            "type": "object",
            "properties": {
                "summary": {"type": "string"}
            },
            "required": ["summary"],
            "additionalProperties": false
        });
        let validator = jsonschema::draft202012::new(&raw).unwrap();
        Arc::new(OutputSchema {
            name: "final_output".to_string(),
            raw,
            validator,
        })
    }

    fn text_response(text: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "resp-1",
            "output": [
                {
                    "type": "message",
                    "id": "msg-1",
                    "status": "completed",
                    "content": [{"type": "output_text", "text": text}]
                }
            ],
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
        })
    }

    fn function_call_response() -> serde_json::Value {
        serde_json::json!({
            "id": "resp-fc",
            "output": [
                {
                    "type": "function_call",
                    "id": "fc-1",
                    "call_id": "call-1",
                    "name": "Read",
                    "arguments": "{\"path\":\"/tmp/x\"}"
                }
            ],
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
        })
    }

    /// Matches the initial request of a run: tools are offered.
    #[derive(Debug)]
    struct RequestWithTools;

    impl Match for RequestWithTools {
        fn matches(&self, request: &Request) -> bool {
            serde_json::from_slice::<serde_json::Value>(&request.body)
                .is_ok_and(|body| body.get("tools").is_some())
        }
    }

    /// Matches a correction-mode request: the corrective user message is in
    /// the input and no tools are offered.
    #[derive(Debug)]
    struct CorrectionRequest;

    impl Match for CorrectionRequest {
        fn matches(&self, request: &Request) -> bool {
            let Ok(body) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
                return false;
            };
            let has_corrective_message = body["input"].as_array().is_some_and(|items| {
                items.iter().any(|item| {
                    item["type"] == "message"
                        && item["role"] == "user"
                        && item["content"][0]["text"]
                            .as_str()
                            .is_some_and(|text| text.contains("failed output schema validation"))
                })
            });
            has_corrective_message
                && body.get("tools").is_none()
                && body.get("tool_choice").is_none()
        }
    }

    /// Matches a correction request that carries the synthetic output for a
    /// tool call returned by the previous, tool-disabled correction turn.
    #[derive(Debug)]
    struct CorrectionRequestWithToolOutput;

    impl Match for CorrectionRequestWithToolOutput {
        fn matches(&self, request: &Request) -> bool {
            let Ok(body) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
                return false;
            };
            body["input"].as_array().is_some_and(|items| {
                items.iter().any(|item| {
                    item["type"] == "function_call_output"
                        && item["call_id"] == "call-1"
                        && item["output"].as_str().is_some_and(|output| {
                            output.contains("not executed: correction turn offers no tools")
                        })
                })
            })
        }
    }

    #[tokio::test]
    async fn conforming_final_message_passes_through_trimmed() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(text_response("\n{\"summary\": \"ok\"}\n")),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_for(ApiType::Responses, &mock_server.uri())
            .with_output_schema(test_schema());

        let result = agent.send("go".to_string()).await.unwrap();

        assert_eq!(result, "{\"summary\": \"ok\"}");
        assert_eq!(agent.turn_count(), 1);
        // No corrective items were added: system, user, assistant.
        assert_eq!(agent.history().len(), 3);
    }

    #[tokio::test]
    async fn fenced_answer_is_corrected_on_a_toolless_turn() {
        let mock_server = MockServer::start().await;

        // Initial request offers tools and returns a fenced (non-conforming)
        // document.
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(RequestWithTools)
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(text_response("```json\n{\"summary\": \"ok\"}\n```")),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        // The correction request must carry the corrective user message and
        // offer no tools.
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(CorrectionRequest)
            .respond_with(
                ResponseTemplate::new(200).set_body_json(text_response("{\"summary\": \"ok\"}")),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let streamed = Arc::new(Mutex::new(Vec::new()));
        let streamed_clone = Arc::clone(&streamed);
        let mut agent = test_agent_for(ApiType::Responses, &mock_server.uri())
            .with_tools(crate::clients::tools::read_tool_registry())
            .with_output_schema(test_schema())
            .with_streaming_json(move |json| {
                streamed_clone.lock().unwrap().push(json.to_string());
            });

        let result = agent.send("go".to_string()).await.unwrap();

        assert_eq!(result, "{\"summary\": \"ok\"}");
        assert_eq!(agent.turn_count(), 2);
        // The corrective message is an ordinary user item in the transcript.
        assert!(agent.history().iter().any(|item| matches!(
            item,
            ConversationItem::Message { role: Role::User, content, .. }
                if content.contains("failed output schema validation")
        )));
        // And it streamed like any other item.
        let streamed_records: Vec<serde_json::Value> = streamed
            .lock()
            .unwrap()
            .iter()
            .map(|json| serde_json::from_str(json).unwrap())
            .collect();
        assert!(streamed_records.iter().any(|record| {
            record["type"] == "message"
                && record["role"] == "user"
                && record["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("failed output schema validation"))
        }));
    }

    #[tokio::test]
    async fn persistent_non_conformance_exhausts_with_unsatisfied() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(text_response("not json at all")),
            )
            .expect(3)
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_for(ApiType::Responses, &mock_server.uri())
            .with_output_schema(test_schema());

        let error = agent.send("go".to_string()).await.unwrap_err();

        let schema_error = error.downcast_ref::<OutputSchemaError>().unwrap();
        assert!(matches!(
            schema_error,
            OutputSchemaError::Unsatisfied { attempts: 3, .. }
        ));
        assert!(
            error
                .to_string()
                .contains("not a single valid JSON document"),
            "error: {error}"
        );
        assert_eq!(agent.turn_count(), 3);
    }

    #[tokio::test]
    async fn schema_invalid_json_reports_validation_detail_on_exhaustion() {
        let mock_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(text_response("{\"other\": 1}")))
            .expect(3)
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_for(ApiType::Responses, &mock_server.uri())
            .with_output_schema(test_schema());

        let error = agent.send("go".to_string()).await.unwrap_err();

        assert!(error.to_string().contains("summary"), "error: {error}");
    }

    #[tokio::test]
    async fn correction_turn_tool_calls_are_not_executed() {
        let mock_server = MockServer::start().await;

        // Turn 1: non-conforming text. Turn 2 (correction): a misbehaving
        // provider returns a tool call anyway. Turn 3 (correction): conforming.
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(text_response("not json")))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(function_call_response()))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(CorrectionRequestWithToolOutput)
            .respond_with(
                ResponseTemplate::new(200).set_body_json(text_response("{\"summary\": \"ok\"}")),
            )
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_for(ApiType::Responses, &mock_server.uri())
            .with_tools(crate::clients::tools::read_tool_registry())
            .with_output_schema(test_schema());

        let result = agent.send("go".to_string()).await.unwrap();

        assert_eq!(result, "{\"summary\": \"ok\"}");
        assert_eq!(agent.turn_count(), 3);
        // The tool call was recorded but never executed. The third request's
        // matcher proves its synthetic output was carried forward.
        assert_eq!(agent.tool_call_count, 0);
        assert_no_dangling_function_calls(agent.history());
    }
}

/// Milestone 3 tests: native structured-output constraint on correction turns.
#[cfg(test)]
mod output_schema_constraint_tests {
    use super::*;
    use crate::config::OutputSchema;
    use crate::config::model::ApiType;
    use std::sync::Arc;

    use wiremock::matchers::{method, path};
    use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

    fn test_schema() -> Arc<OutputSchema> {
        let raw = serde_json::json!({
            "type": "object",
            "properties": {
                "summary": {"type": "string"}
            },
            "required": ["summary"],
            "additionalProperties": false
        });
        let validator = jsonschema::draft202012::new(&raw).unwrap();
        Arc::new(OutputSchema {
            name: "final_output".to_string(),
            raw,
            validator,
        })
    }

    fn responses_text_response(text: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "resp-1",
            "output": [
                {
                    "type": "message",
                    "id": "msg-1",
                    "status": "completed",
                    "content": [{"type": "output_text", "text": text}]
                }
            ],
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
        })
    }

    fn chat_text_response(text: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "chatcmpl-1",
            "choices": [
                {
                    "index": 0,
                    "message": {"role": "assistant", "content": text},
                    "finish_reason": "stop"
                }
            ],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })
    }

    /// Matches a Responses API request whose body has no correction state.
    #[derive(Debug)]
    struct InitialResponsesRequest;

    impl Match for InitialResponsesRequest {
        fn matches(&self, request: &Request) -> bool {
            serde_json::from_slice::<serde_json::Value>(&request.body)
                .is_ok_and(|body| body.get("text").is_none())
        }
    }

    /// Matches a Responses API correction request carrying the native
    /// `json_schema` constraint with the expected shape.
    #[derive(Debug)]
    struct ConstrainedResponsesRequest;

    impl Match for ConstrainedResponsesRequest {
        fn matches(&self, request: &Request) -> bool {
            let Ok(body) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
                return false;
            };
            body["text"]["format"]["type"] == "json_schema"
                && body["text"]["format"]["name"] == "final_output"
                && body["text"]["format"]["strict"] == true
                && body["text"]["format"]["schema"]["required"][0] == "summary"
                && body.get("tools").is_none()
        }
    }

    /// Matches a Chat Completions correction request carrying the native
    /// `response_format` constraint with the expected shape.
    #[derive(Debug)]
    struct ConstrainedChatRequest;

    impl Match for ConstrainedChatRequest {
        fn matches(&self, request: &Request) -> bool {
            let Ok(body) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
                return false;
            };
            body["response_format"]["type"] == "json_schema"
                && body["response_format"]["json_schema"]["name"] == "final_output"
                && body["response_format"]["json_schema"]["strict"] == true
                && body.get("tools").is_none()
        }
    }

    #[tokio::test]
    async fn responses_correction_turn_attaches_native_constraint() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(InitialResponsesRequest)
            .respond_with(
                ResponseTemplate::new(200).set_body_json(responses_text_response("not json")),
            )
            .expect(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(ConstrainedResponsesRequest)
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(responses_text_response("{\"summary\": \"ok\"}")),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_for(ApiType::Responses, &mock_server.uri())
            .with_output_schema(test_schema());

        let result = agent.send("go".to_string()).await.unwrap();

        assert_eq!(result, "{\"summary\": \"ok\"}");
        assert_eq!(agent.turn_count(), 2);
    }

    #[tokio::test]
    async fn chat_completions_correction_turn_attaches_native_constraint() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(ConstrainedChatRequest)
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(chat_text_response("{\"summary\": \"ok\"}")),
            )
            .expect(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(chat_text_response("not json")))
            .expect(1)
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_for(ApiType::ChatCompletions, &mock_server.uri())
            .with_output_schema(test_schema());

        let result = agent.send("go".to_string()).await.unwrap();

        assert_eq!(result, "{\"summary\": \"ok\"}");
        assert_eq!(agent.turn_count(), 2);
    }

    #[tokio::test]
    async fn provider_400_on_constraint_falls_back_to_unconstrained_retry() {
        let mock_server = MockServer::start().await;

        // The constrained correction request is rejected by the provider.
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(ConstrainedResponsesRequest)
            .respond_with(ResponseTemplate::new(400).set_body_string("unsupported schema feature"))
            .expect(1)
            .mount(&mock_server)
            .await;
        // Initial turn and the unconstrained correction retry share this
        // matcher; the first response is non-conforming, the second conforms.
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(InitialResponsesRequest)
            .respond_with(
                ResponseTemplate::new(200).set_body_json(responses_text_response("not json")),
            )
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(InitialResponsesRequest)
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(responses_text_response("{\"summary\": \"ok\"}")),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let mut agent = test_agent_for(ApiType::Responses, &mock_server.uri())
            .with_output_schema(test_schema());

        let result = agent.send("go".to_string()).await.unwrap();

        assert_eq!(result, "{\"summary\": \"ok\"}");
        // The 400 turn errored before being counted: initial turn plus the
        // successful unconstrained correction retry.
        assert_eq!(agent.turn_count(), 2);
    }
}
