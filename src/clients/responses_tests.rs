use super::*;
use crate::clients::responses_types::{OutputContent, OutputMessage, ProviderConfig};
use crate::clients::tools::default_tool_registry;
use crate::config::skills::{Skill, SkillScope};
use crate::config::{AgentsFile, SkillCatalog};
use crate::prompts::build_initial_prompt_messages;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn to_api_input_json(item: &ConversationItem) -> serde_json::Value {
    serde_json::to_value(ResponsesApiInputItem::from(item))
        .expect("Responses API input DTO serialization should be infallible")
}

fn input_json(history: &[ConversationItem]) -> Vec<serde_json::Value> {
    build_input(history)
        .into_iter()
        .map(|item| serde_json::to_value(item).unwrap())
        .collect()
}

fn full_prompt_history() -> Vec<ConversationItem> {
    let config_dir = TempDir::new().unwrap();
    let agents_files = vec![AgentsFile {
        path: "./AGENTS.md".to_string(),
        content: "Project instructions for contributors.\nPrefer small, focused changes."
            .to_string(),
    }];
    let mut skill_catalog = SkillCatalog::empty();
    skill_catalog.skills.push(Skill {
        name: "debugging".to_string(),
        description: "Debug failing Rust tests".to_string(),
        location: PathBuf::from("/project/.agents/skills/debugging/SKILL.md"),
        base_directory: PathBuf::from("/project/.agents/skills/debugging"),
        scope: SkillScope::Project,
    });

    let mut history = build_initial_prompt_messages(
        Path::new("/project"),
        config_dir.path(),
        &agents_files,
        &skill_catalog,
    )
    .into_iter()
    .map(|(role, content)| ConversationItem::Message {
        role,
        content,
        id: None,
        status: None,
        timestamp: None,
    })
    .collect::<Vec<_>>();
    history.push(ConversationItem::Message {
        role: Role::User,
        content: "List the files in the project root.".to_string(),
        id: None,
        status: None,
        timestamp: None,
    });
    history
}

fn assert_json_snapshot_with_environment_filters(name: &str, value: &serde_json::Value) {
    insta::with_settings!({
        filters => vec![
            (
                r"Today's date: \d{4}-\d{2}-\d{2}\\nPlatform: .*?\\nArchitecture: .*?\\nShell: .*?\\nTerminal: .*?\\n\\nUser message:",
                "Today's date: [DATE]\\nPlatform: [PLATFORM]\\nArchitecture: [ARCH]\\nShell: [SHELL]\\nTerminal: [TERMINAL]\\n\\nUser message:"
            ),
            (
                r#"Today's date: \d{4}-\d{2}-\d{2}\\nPlatform: .*?\\nArchitecture: .*?\\nShell: .*?\\nTerminal: [^"]+""#,
                "Today's date: [DATE]\\nPlatform: [PLATFORM]\\nArchitecture: [ARCH]\\nShell: [SHELL]\\nTerminal: [TERMINAL]\""
            ),
        ]
    }, {
        insta::assert_json_snapshot!(name, value);
    });
}

#[test]
fn extract_instructions_with_system_message() {
    let history = vec![
        ConversationItem::Message {
            role: Role::System,
            content: "You are cake.".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
        ConversationItem::Message {
            role: Role::User,
            content: "Hello".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
    ];
    let (instructions, remaining) = extract_instructions(&history).unwrap();
    assert_eq!(instructions, Some("You are cake."));
    assert_eq!(remaining.len(), 1);
    assert!(matches!(
        &remaining[0],
        ConversationItem::Message {
            role: Role::User,
            ..
        }
    ));
}

#[test]
fn extract_instructions_keeps_developer_messages_in_input() {
    let history = vec![
        ConversationItem::Message {
            role: Role::System,
            content: "You are cake.".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
        ConversationItem::Message {
            role: Role::Developer,
            content: "Mutable context".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
        ConversationItem::Message {
            role: Role::User,
            content: "Hello".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
    ];

    let (instructions, remaining) = extract_instructions(&history).unwrap();
    assert_eq!(instructions, Some("You are cake."));
    assert_eq!(remaining.len(), 2);

    let input = input_json(remaining);
    assert_eq!(input[0]["role"], "developer");
    assert_eq!(input[0]["content"][0]["text"], "Mutable context");
    assert_eq!(input[1]["role"], "user");
}

#[test]
fn extract_instructions_without_system_message() {
    let history = vec![ConversationItem::Message {
        role: Role::User,
        content: "Hello".to_string(),
        id: None,
        status: None,
        timestamp: None,
    }];
    let (instructions, remaining) = extract_instructions(&history).unwrap();
    assert!(instructions.is_none());
    assert_eq!(remaining.len(), 1);
}

#[test]
fn extract_instructions_empty_history() {
    let history: Vec<ConversationItem> = vec![];
    let (instructions, remaining) = extract_instructions(&history).unwrap();
    assert!(instructions.is_none());
    assert!(remaining.is_empty());
}

#[test]
fn extract_instructions_system_message_non_first_position_errors() {
    let history = vec![
        ConversationItem::Message {
            role: Role::User,
            content: "Hello".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
        ConversationItem::Message {
            role: Role::System,
            content: "You are cake.".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
        ConversationItem::Message {
            role: Role::Assistant,
            content: "Later history must not be silently dropped.".to_string(),
            id: Some("msg-1".to_string()),
            status: Some("completed".to_string()),
            timestamp: None,
        },
    ];
    let err = extract_instructions(&history).unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid Responses API conversation history: system message found at index 1; \
             system messages are only valid as the first history item"
    );
}

#[test]
fn build_input_converts_history() {
    let history = vec![ConversationItem::Message {
        role: Role::User,
        content: "hi".to_string(),
        id: None,
        status: None,
        timestamp: None,
    }];
    let input = input_json(&history);
    assert_eq!(input.len(), 1);
    assert_eq!(input[0]["type"], "message");
}

#[test]
fn build_input_empty_history() {
    let history: Vec<ConversationItem> = vec![];
    let input = build_input(&history);
    assert!(input.is_empty());
}

#[test]
fn parse_output_items_message() {
    let response = ApiResponse {
        id: None,
        output: vec![OutputMessage {
            msg_type: "message".to_string(),
            id: Some("msg-1".to_string()),
            call_id: None,
            name: None,
            arguments: None,
            status: Some("completed".to_string()),
            content: Some(vec![OutputContent {
                content_type: "output_text".to_string(),
                text: Some("Hello!".to_string()),
            }]),
            encrypted_content: None,
            summary: None,
        }],
        usage: None,
    };
    let items = parse_output_items(&response).unwrap();
    assert_eq!(items.len(), 1);
    assert!(matches!(&items[0], ConversationItem::Message {
            role: Role::Assistant, content, ..
        } if content == "Hello!"));
}

#[test]
fn parse_output_items_function_call() {
    let response = ApiResponse {
        id: None,
        output: vec![OutputMessage {
            msg_type: "function_call".to_string(),
            id: Some("fc-1".to_string()),
            call_id: Some("call-1".to_string()),
            name: Some("bash".to_string()),
            arguments: Some(r#"{"cmd":"ls"}"#.to_string()),
            status: None,
            content: None,
            encrypted_content: None,
            summary: None,
        }],
        usage: None,
    };
    let items = parse_output_items(&response).unwrap();
    assert_eq!(items.len(), 1);
    assert!(matches!(&items[0], ConversationItem::FunctionCall {
            name, ..
        } if name == "bash"));
}

#[test]
fn parse_output_items_reasoning() {
    let response = ApiResponse {
        id: None,
        output: vec![OutputMessage {
            msg_type: "reasoning".to_string(),
            id: Some("r-1".to_string()),
            call_id: None,
            name: None,
            arguments: None,
            status: None,
            content: Some(vec![OutputContent {
                content_type: "reasoning_text".to_string(),
                text: Some("thinking...".to_string()),
            }]),
            encrypted_content: None,
            summary: None,
        }],
        usage: None,
    };
    let items = parse_output_items(&response).unwrap();
    assert_eq!(items.len(), 1);
    assert!(matches!(&items[0], ConversationItem::Reasoning { .. }));
}

#[test]
fn parse_output_items_reasoning_with_encrypted_content() {
    let response = ApiResponse {
        id: None,
        output: vec![OutputMessage {
            msg_type: "reasoning".to_string(),
            id: Some("r-1".to_string()),
            call_id: None,
            name: None,
            arguments: None,
            status: None,
            content: None,
            encrypted_content: Some("gAAAAABencrypted...".to_string()),
            summary: Some(vec!["step 1".to_string(), "step 2".to_string()]),
        }],
        usage: None,
    };
    let items = parse_output_items(&response).unwrap();
    assert_eq!(items.len(), 1);
    if let ConversationItem::Reasoning {
        summary,
        encrypted_content,
        ..
    } = &items[0]
    {
        let summary = summary.as_ref().unwrap();
        assert_eq!(summary.len(), 2);
        assert_eq!(summary[0], "step 1");
        assert_eq!(encrypted_content.as_deref(), Some("gAAAAABencrypted..."));
    } else {
        panic!("Expected Reasoning item");
    }
}

#[test]
fn parse_output_items_reasoning_preserves_content_for_roundtrip() {
    let response = ApiResponse {
        id: None,
        output: vec![OutputMessage {
            msg_type: "reasoning".to_string(),
            id: Some("r-1".to_string()),
            call_id: None,
            name: None,
            arguments: None,
            status: None,
            content: Some(vec![OutputContent {
                content_type: "reasoning_text".to_string(),
                text: Some("deep reasoning here".to_string()),
            }]),
            encrypted_content: None,
            summary: None,
        }],
        usage: None,
    };
    let items = parse_output_items(&response).unwrap();
    assert_eq!(items.len(), 1);
    let api_input = to_api_input_json(&items[0]);
    assert_eq!(api_input["content"][0]["type"], "reasoning_text");
    assert_eq!(api_input["content"][0]["text"], "deep reasoning here");
}

#[test]
fn parse_output_items_reasoning_preserves_unknown_content_kind() {
    let response = ApiResponse {
        id: None,
        output: vec![OutputMessage {
            msg_type: "reasoning".to_string(),
            id: Some("r-1".to_string()),
            call_id: None,
            name: None,
            arguments: None,
            status: None,
            content: Some(vec![OutputContent {
                content_type: "provider_specific_reasoning".to_string(),
                text: Some("opaque reasoning".to_string()),
            }]),
            encrypted_content: None,
            summary: None,
        }],
        usage: None,
    };

    let items = parse_output_items(&response).unwrap();
    let api_input = to_api_input_json(&items[0]);

    assert_eq!(
        api_input["content"][0]["type"],
        "provider_specific_reasoning"
    );
    assert_eq!(api_input["content"][0]["text"], "opaque reasoning");
}

#[test]
fn parse_output_items_unknown_type_errors_when_no_items_are_recognized() {
    let response = ApiResponse {
        id: Some("resp-123".to_string()),
        output: vec![OutputMessage {
            msg_type: "unknown_type".to_string(),
            id: Some("out-1".to_string()),
            call_id: None,
            name: None,
            arguments: None,
            status: None,
            content: None,
            encrypted_content: None,
            summary: None,
        }],
        usage: None,
    };
    let error = parse_output_items(&response).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("contained only unknown output type(s)"));
    assert!(message.contains("resp-123"));
    assert!(message.contains("output[0] type 'unknown_type'"));
}

#[test]
fn parse_output_items_unknown_type_is_skipped_when_known_items_exist() {
    let response = ApiResponse {
        id: Some("resp-123".to_string()),
        output: vec![
            OutputMessage {
                msg_type: "unknown_type".to_string(),
                id: Some("out-1".to_string()),
                call_id: None,
                name: None,
                arguments: None,
                status: None,
                content: None,
                encrypted_content: None,
                summary: None,
            },
            OutputMessage {
                msg_type: "message".to_string(),
                id: Some("msg-1".to_string()),
                call_id: None,
                name: None,
                arguments: None,
                status: Some("completed".to_string()),
                content: Some(vec![OutputContent {
                    content_type: "output_text".to_string(),
                    text: Some("Hello!".to_string()),
                }]),
                encrypted_content: None,
                summary: None,
            },
        ],
        usage: None,
    };
    let items = parse_output_items(&response).unwrap();
    assert_eq!(items.len(), 1);
    assert!(matches!(&items[0], ConversationItem::Message {
            role: Role::Assistant, content, ..
        } if content == "Hello!"));
}

#[test]
fn parse_output_items_multiple_items() {
    let response = ApiResponse {
        id: None,
        output: vec![
            OutputMessage {
                msg_type: "reasoning".to_string(),
                id: Some("r-1".to_string()),
                call_id: None,
                name: None,
                arguments: None,
                status: None,
                content: Some(vec![OutputContent {
                    content_type: "reasoning_text".to_string(),
                    text: Some("thinking...".to_string()),
                }]),
                encrypted_content: None,
                summary: None,
            },
            OutputMessage {
                msg_type: "message".to_string(),
                id: Some("msg-1".to_string()),
                call_id: None,
                name: None,
                arguments: None,
                status: None,
                content: Some(vec![OutputContent {
                    content_type: "output_text".to_string(),
                    text: Some("Hello!".to_string()),
                }]),
                encrypted_content: None,
                summary: None,
            },
        ],
        usage: None,
    };
    let items = parse_output_items(&response).unwrap();
    assert_eq!(items.len(), 2);
}

#[test]
fn parse_output_items_message_without_content() {
    let response = ApiResponse {
        id: None,
        output: vec![OutputMessage {
            msg_type: "message".to_string(),
            id: Some("msg-1".to_string()),
            call_id: None,
            name: None,
            arguments: None,
            status: None,
            content: None,
            encrypted_content: None,
            summary: None,
        }],
        usage: None,
    };
    let items = parse_output_items(&response).unwrap();
    assert_eq!(items.len(), 1);
    assert!(matches!(&items[0], ConversationItem::Message {
            content, ..
        } if content.is_empty()));
}

#[test]
fn provider_config_with_all_returns_none() {
    let providers = vec!["all".to_string()];
    let config = if providers.is_empty() || (providers.len() == 1 && providers[0] == "all") {
        None
    } else {
        Some(ProviderConfig { only: providers })
    };
    assert!(config.is_none());
}

#[test]
fn snapshot_responses_request_minimal() {
    let history = vec![ConversationItem::Message {
        role: Role::User,
        content: "Hello".to_string(),
        id: None,
        status: None,
        timestamp: None,
    }];
    let request = Request {
        model: "openai/gpt-4.1",
        input: build_input(&history),
        instructions: None,
        temperature: None,
        top_p: None,
        max_output_tokens: None,
        tools: None,
        tool_choice: None,
        provider: None,
        reasoning: None,
    };

    insta::assert_json_snapshot!(
        "responses_request_minimal",
        serde_json::to_value(&request).unwrap()
    );
}

#[test]
fn snapshot_responses_request_with_tools_provider_and_reasoning() {
    let history = vec![
        ConversationItem::Message {
            role: Role::System,
            content: "You are cake.".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
        ConversationItem::Message {
            role: Role::User,
            content: "List files".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
        ConversationItem::FunctionCall {
            id: "fc-1".to_string(),
            call_id: "call-1".to_string(),
            name: "bash".to_string(),
            arguments: r#"{"cmd":"ls"}"#.to_string(),
            timestamp: None,
        },
        ConversationItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: "Cargo.toml\nsrc".to_string(),
            timestamp: None,
        },
    ];
    let tools = vec![Tool {
        type_: "function".to_string(),
        name: "bash".to_string(),
        description: "Run a shell command".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "cmd": { "type": "string" }
            },
            "required": ["cmd"]
        }),
    }];
    let (instructions, non_system_history) = extract_instructions(&history).unwrap();
    let request = Request {
        model: "openai/gpt-5",
        input: build_input(non_system_history),
        instructions,
        temperature: Some(0.3),
        top_p: Some(0.95),
        max_output_tokens: Some(2048),
        tools: Some(&tools),
        tool_choice: Some("auto".to_string()),
        provider: Some(ProviderConfig {
            only: vec!["OpenAI".to_string(), "Anthropic".to_string()],
        }),
        reasoning: Some(ReasoningConfig {
            effort: Some(crate::config::ReasoningEffort::Medium),
            summary: Some("auto".to_string()),
            max_tokens: Some(512),
        }),
    };

    insta::assert_json_snapshot!(
        "responses_request_with_tools_provider_and_reasoning",
        serde_json::to_value(&request).unwrap()
    );
}

#[test]
fn snapshot_responses_request_full_with_agents_and_skills() {
    let history = full_prompt_history();
    let registry = default_tool_registry();
    let (instructions, non_system_history) = extract_instructions(&history).unwrap();
    let request = Request {
        model: "test-responses-model",
        input: build_input(non_system_history),
        instructions,
        temperature: Some(0.2),
        top_p: Some(0.9),
        max_output_tokens: None,
        tools: Some(registry.definitions()),
        tool_choice: Some("auto".to_string()),
        provider: None,
        reasoning: None,
    };

    assert_json_snapshot_with_environment_filters(
        "responses_request_full_with_agents_and_skills",
        &serde_json::to_value(&request).unwrap(),
    );
}

// =========================================================================
// Malformed Response Tests
// =========================================================================

#[test]
fn parse_output_items_empty_output_array() {
    let response = ApiResponse {
        id: None,
        output: vec![],
        usage: None,
    };
    let items = parse_output_items(&response).unwrap();
    assert!(items.is_empty());
}

#[test]
fn parse_output_items_missing_id_for_reasoning() {
    // Reasoning without an id should be skipped (id is required for reasoning)
    let response = ApiResponse {
        id: None,
        output: vec![OutputMessage {
            msg_type: "reasoning".to_string(),
            id: None, // Missing required id
            call_id: None,
            name: None,
            arguments: None,
            status: None,
            content: Some(vec![OutputContent {
                content_type: "reasoning_text".to_string(),
                text: Some("thinking...".to_string()),
            }]),
            encrypted_content: None,
            summary: None,
        }],
        usage: None,
    };
    let items = parse_output_items(&response).unwrap();
    // Reasoning without id is skipped
    assert!(items.is_empty());
}

#[test]
fn parse_output_items_function_call_missing_fields() {
    let response = ApiResponse {
        id: Some("resp-123".to_string()),
        output: vec![OutputMessage {
            msg_type: "function_call".to_string(),
            id: None,
            call_id: None,
            name: None,
            arguments: None,
            status: None,
            content: None,
            encrypted_content: None,
            summary: None,
        }],
        usage: None,
    };
    let error = parse_output_items(&response).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("malformed Responses API function_call"));
    assert!(message.contains("output[0]"));
    assert!(message.contains("resp-123"));
    assert!(message.contains("id, call_id, name, arguments"));
}

#[test]
fn parse_output_items_message_with_empty_content_array() {
    let response = ApiResponse {
        id: None,
        output: vec![OutputMessage {
            msg_type: "message".to_string(),
            id: Some("msg-1".to_string()),
            call_id: None,
            name: None,
            arguments: None,
            status: Some("completed".to_string()),
            content: Some(vec![]), // Empty content array
            encrypted_content: None,
            summary: None,
        }],
        usage: None,
    };
    let items = parse_output_items(&response).unwrap();
    assert_eq!(items.len(), 1);
    // Should default to empty string
    assert!(matches!(&items[0], ConversationItem::Message {
            content,
            ..
        } if content.is_empty()));
}

#[test]
fn parse_output_items_message_with_non_text_content() {
    // Message with content type that isn't output_text
    let response = ApiResponse {
        id: None,
        output: vec![OutputMessage {
            msg_type: "message".to_string(),
            id: Some("msg-1".to_string()),
            call_id: None,
            name: None,
            arguments: None,
            status: Some("completed".to_string()),
            content: Some(vec![OutputContent {
                content_type: "image".to_string(), // Not output_text
                text: Some("image data".to_string()),
            }]),
            encrypted_content: None,
            summary: None,
        }],
        usage: None,
    };
    let items = parse_output_items(&response).unwrap();
    assert_eq!(items.len(), 1);
    // Should default to empty string since no output_text found
    assert!(matches!(&items[0], ConversationItem::Message {
            content,
            ..
        } if content.is_empty()));
}

#[test]
fn parse_output_items_reasoning_with_summary_fallback() {
    // Reasoning with summary but no content
    let response = ApiResponse {
        id: None,
        output: vec![OutputMessage {
            msg_type: "reasoning".to_string(),
            id: Some("r-1".to_string()),
            call_id: None,
            name: None,
            arguments: None,
            status: None,
            content: None,
            encrypted_content: None,
            summary: Some(vec!["step 1".to_string(), "step 2".to_string()]),
        }],
        usage: None,
    };
    let items = parse_output_items(&response).unwrap();
    assert_eq!(items.len(), 1);
    if let ConversationItem::Reasoning { summary, .. } = &items[0] {
        let summary = summary.as_ref().unwrap();
        assert_eq!(summary.len(), 2);
        assert_eq!(summary[0], "step 1");
    } else {
        panic!("Expected Reasoning item");
    }
}

#[test]
fn parse_output_items_reasoning_content_fallback_to_summary() {
    // Reasoning with content containing reasoning_text
    let response = ApiResponse {
        id: None,
        output: vec![OutputMessage {
            msg_type: "reasoning".to_string(),
            id: Some("r-1".to_string()),
            call_id: None,
            name: None,
            arguments: None,
            status: None,
            content: Some(vec![OutputContent {
                content_type: "reasoning_text".to_string(),
                text: Some("thinking...".to_string()),
            }]),
            encrypted_content: None,
            summary: None, // No summary, should derive from content
        }],
        usage: None,
    };
    let items = parse_output_items(&response).unwrap();
    assert_eq!(items.len(), 1);
    if let ConversationItem::Reasoning { summary, .. } = &items[0] {
        let summary = summary.as_ref().unwrap();
        // Summary should be derived from content
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0], "thinking...");
    } else {
        panic!("Expected Reasoning item");
    }
}
