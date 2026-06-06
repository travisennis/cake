use super::*;
use crate::clients::chat_types::{
    ChatChoice, ChatFunctionCall, ChatResponse, ChatResponseMessage, ChatToolCall, ChatUsage,
    PromptTokensDetails,
};
use crate::clients::tools::default_tool_registry;
use crate::config::model::{ApiType, ModelConfig};
use crate::config::skills::{Skill, SkillScope};
use crate::config::{AgentsFile, SkillCatalog};
use crate::prompts::build_initial_prompt_messages;
use crate::types::{ReasoningContent, ReasoningContentKind};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn apply_test_strategy(model: &str, messages: &mut Vec<ChatMessage<'_>>) {
    let config = ResolvedModelConfig {
        model_config: ModelConfig {
            model: model.to_string(),
            api_type: ApiType::ChatCompletions,
            base_url: "https://api.example.com/v1".to_string(),
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
    };
    ProviderStrategy::from_config(&config).transform_chat_messages(messages);
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
                r#"Today's date: \d{4}-\d{2}-\d{2}\\nPlatform: .*?\\nArchitecture: .*?\\nShell: .*?\\nTerminal: [^"]+""#,
                "Today's date: [DATE]\\nPlatform: [PLATFORM]\\nArchitecture: [ARCH]\\nShell: [SHELL]\\nTerminal: [TERMINAL]\""
            ),
        ]
    }, {
        insta::assert_json_snapshot!(name, value);
    });
}

#[test]
fn build_messages_simple_conversation() {
    let history = vec![
        ConversationItem::Message {
            role: Role::System,
            content: "You are helpful.".to_string(),
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
    let msgs = build_messages(&history);
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].role, "system");
    assert_eq!(msgs[0].content.as_deref(), Some("You are helpful."));
    assert_eq!(msgs[1].role, "user");
    assert_eq!(msgs[1].content.as_deref(), Some("Hello"));
}

#[test]
fn build_messages_preserves_developer_messages_separately() {
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
            content: "AGENTS.md context".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
        ConversationItem::Message {
            role: Role::Developer,
            content: "Environment context".to_string(),
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

    let msgs = build_messages(&history);
    assert_eq!(msgs.len(), 4);
    assert_eq!(msgs[0].role, "system");
    assert_eq!(msgs[0].content.as_deref(), Some("You are cake."));
    assert_eq!(msgs[1].role, "developer");
    assert_eq!(msgs[1].content.as_deref(), Some("AGENTS.md context"));
    assert_eq!(msgs[2].role, "developer");
    assert_eq!(msgs[2].content.as_deref(), Some("Environment context"));
    assert_eq!(msgs[3].role, "user");
    assert_eq!(msgs[3].content.as_deref(), Some("Hello"));
}

#[test]
fn build_messages_keeps_developer_messages_before_assistant() {
    let history = vec![
        ConversationItem::Message {
            role: Role::Developer,
            content: "Project context".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
        ConversationItem::Message {
            role: Role::Assistant,
            content: "Ready.".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
        ConversationItem::Message {
            role: Role::User,
            content: "Start now".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
    ];

    let msgs = build_messages(&history);
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0].role, "developer");
    assert_eq!(msgs[0].content.as_deref(), Some("Project context"));
    assert_eq!(msgs[1].role, "assistant");
    assert_eq!(msgs[1].content.as_deref(), Some("Ready."));
    assert_eq!(msgs[2].role, "user");
    assert_eq!(msgs[2].content.as_deref(), Some("Start now"));
}

#[test]
fn build_messages_flushes_pending_tool_calls_before_user_message() {
    let history = vec![
        ConversationItem::Message {
            role: Role::User,
            content: "inspect".to_string(),
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
        ConversationItem::Message {
            role: Role::User,
            content: "Actually stop".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
    ];

    let msgs = build_messages(&history);
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0].role, "user");
    assert_eq!(msgs[1].role, "assistant");
    assert!(msgs[1].content.is_none());
    assert_eq!(msgs[1].tool_calls.as_ref().unwrap().len(), 1);
    assert_eq!(msgs[2].role, "user");
    assert_eq!(msgs[2].content.as_deref(), Some("Actually stop"));
}

#[test]
fn build_messages_groups_consecutive_function_calls() {
    let history = vec![
        ConversationItem::Message {
            role: Role::User,
            content: "do stuff".to_string(),
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
        ConversationItem::FunctionCall {
            id: "fc-2".to_string(),
            call_id: "call-2".to_string(),
            name: "read".to_string(),
            arguments: r#"{"path":"foo.txt"}"#.to_string(),
            timestamp: None,
        },
        ConversationItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: "file.txt".to_string(),
            timestamp: None,
        },
        ConversationItem::FunctionCallOutput {
            call_id: "call-2".to_string(),
            output: "contents".to_string(),
            timestamp: None,
        },
    ];
    let msgs = build_messages(&history);
    // user + assistant(with 2 tool_calls) + tool + tool = 4 messages
    assert_eq!(msgs.len(), 4);

    // First: user message
    assert_eq!(msgs[0].role, "user");

    // Second: assistant with grouped tool_calls
    assert_eq!(msgs[1].role, "assistant");
    assert!(msgs[1].content.is_none());
    assert!(msgs[1].reasoning_content.is_none());
    let tcs = msgs[1].tool_calls.as_ref().unwrap();
    assert_eq!(tcs.len(), 2);
    assert_eq!(tcs[0].function.name, "bash");
    assert_eq!(tcs[1].function.name, "read");

    // Third and fourth: tool results
    assert_eq!(msgs[2].role, "tool");
    assert_eq!(msgs[2].tool_call_id.as_deref(), Some("call-1"));
    assert_eq!(msgs[3].role, "tool");
    assert_eq!(msgs[3].tool_call_id.as_deref(), Some("call-2"));
}

#[test]
fn build_messages_preserves_reasoning_content_for_assistant_messages() {
    let history = vec![
        ConversationItem::Message {
            role: Role::User,
            content: "think".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
        ConversationItem::Reasoning {
            id: "r-1".to_string(),
            summary: Some(vec!["thinking...".to_string()]),
            encrypted_content: None,
            content: Some(vec![ReasoningContent {
                content_type: ReasoningContentKind::ReasoningText,
                text: Some("internal reasoning".to_string()),
            }]),
            timestamp: None,
        },
        ConversationItem::Message {
            role: Role::Assistant,
            content: "done".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
    ];
    let msgs = build_messages(&history);
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].role, "user");
    assert_eq!(msgs[1].role, "assistant");
    assert_eq!(
        msgs[1].reasoning_content.as_deref(),
        Some("internal reasoning")
    );
}

#[test]
fn build_messages_preserves_reasoning_content_for_assistant_tool_calls() {
    let history = vec![
        ConversationItem::Message {
            role: Role::User,
            content: "do stuff".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
        ConversationItem::Reasoning {
            id: "r-1".to_string(),
            summary: Some(vec!["thinking...".to_string()]),
            encrypted_content: None,
            content: Some(vec![ReasoningContent {
                content_type: ReasoningContentKind::ReasoningText,
                text: Some("preserved reasoning".to_string()),
            }]),
            timestamp: None,
        },
        ConversationItem::FunctionCall {
            id: "fc-1".to_string(),
            call_id: "call-1".to_string(),
            name: "bash".to_string(),
            arguments: r#"{"cmd":"ls"}"#.to_string(),
            timestamp: None,
        },
    ];

    let msgs = build_messages(&history);
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[1].role, "assistant");
    assert_eq!(
        msgs[1].reasoning_content.as_deref(),
        Some("preserved reasoning")
    );
    assert!(msgs[1].tool_calls.is_some());
}

#[test]
fn build_messages_combines_tool_calls_with_assistant_text() {
    let history = vec![
        ConversationItem::Message {
            role: Role::User,
            content: "do stuff".to_string(),
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
        ConversationItem::Message {
            role: Role::Assistant,
            content: "Let me check that.".to_string(),
            id: Some("msg-1".to_string()),
            status: Some("completed".to_string()),
            timestamp: None,
        },
        ConversationItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: "files".to_string(),
            timestamp: None,
        },
    ];

    let msgs = build_messages(&history);
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[1].role, "assistant");
    assert_eq!(msgs[1].content.as_deref(), Some("Let me check that."));
    assert!(msgs[1].tool_calls.is_some());
    assert_eq!(msgs[2].role, "tool");
}

#[test]
fn kimi_strategy_adds_reasoning_placeholder_to_tool_call_messages() {
    let history = vec![
        ConversationItem::Message {
            role: Role::User,
            content: "do stuff".to_string(),
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
    ];

    let mut msgs = build_messages(&history);
    apply_test_strategy("moonshot/kimi-k2.6", &mut msgs);
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[1].role, "assistant");
    assert_eq!(msgs[1].reasoning_content.as_deref(), Some(" "));
    assert!(msgs[1].tool_calls.is_some());
}

#[test]
fn kimi_strategy_preserves_existing_reasoning_content() {
    let history = vec![
        ConversationItem::Message {
            role: Role::User,
            content: "do stuff".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
        ConversationItem::Reasoning {
            id: "r-1".to_string(),
            summary: Some(vec!["thinking...".to_string()]),
            encrypted_content: None,
            content: Some(vec![ReasoningContent {
                content_type: ReasoningContentKind::ReasoningText,
                text: Some("actual reasoning".to_string()),
            }]),
            timestamp: None,
        },
        ConversationItem::FunctionCall {
            id: "fc-1".to_string(),
            call_id: "call-1".to_string(),
            name: "bash".to_string(),
            arguments: r#"{"cmd":"ls"}"#.to_string(),
            timestamp: None,
        },
    ];

    let mut msgs = build_messages(&history);
    apply_test_strategy("moonshot/kimi-k2.6", &mut msgs);
    assert_eq!(
        msgs[1].reasoning_content.as_deref(),
        Some("actual reasoning")
    );
}

#[test]
fn kimi_strategy_does_not_affect_messages_without_tool_calls() {
    let history = vec![
        ConversationItem::Message {
            role: Role::User,
            content: "hello".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
        ConversationItem::Message {
            role: Role::Assistant,
            content: "hi there".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
    ];

    let mut msgs = build_messages(&history);
    apply_test_strategy("moonshot/kimi-k2.6", &mut msgs);
    assert_eq!(msgs.len(), 2);
    assert!(msgs[1].reasoning_content.is_none());
}

#[test]
fn non_kimi_strategy_does_not_add_reasoning_placeholder() {
    let history = vec![
        ConversationItem::Message {
            role: Role::User,
            content: "do stuff".to_string(),
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
    ];

    let mut msgs = build_messages(&history);
    apply_test_strategy("gpt-4.1", &mut msgs);
    assert_eq!(msgs.len(), 2);
    assert!(msgs[1].reasoning_content.is_none());
}

#[test]
fn convert_tools_wraps_under_function() {
    let tools = vec![Tool {
        type_: "function".to_string(),
        name: "bash".to_string(),
        description: "Run a command".to_string(),
        parameters: serde_json::json!({"type": "object"}),
    }];
    let chat_tools = convert_tools(&tools);
    assert_eq!(chat_tools.len(), 1);
    assert_eq!(chat_tools[0].type_, "function");
    assert_eq!(chat_tools[0].function.name, "bash");
    assert_eq!(chat_tools[0].function.description, "Run a command");
}

#[test]
fn parse_choices_text_response() {
    let response = ChatResponse {
        id: Some("chatcmpl-123".to_string()),
        choices: vec![ChatChoice {
            message: ChatResponseMessage {
                content: Some("Hello!".to_string()),
                reasoning_content: None,
                tool_calls: None,
            },
        }],
        usage: None,
    };
    let items = parse_choices(&response).unwrap();
    assert_eq!(items.len(), 1);
    assert!(matches!(&items[0], ConversationItem::Message {
        role: Role::Assistant,
        content,
        ..
    } if content == "Hello!"));
}

#[test]
fn parse_choices_tool_calls() {
    let response = ChatResponse {
        id: Some("chatcmpl-456".to_string()),
        choices: vec![ChatChoice {
            message: ChatResponseMessage {
                content: None,
                reasoning_content: None,
                tool_calls: Some(vec![ChatToolCall {
                    id: "call-abc".to_string(),
                    function: ChatFunctionCall {
                        name: "bash".to_string(),
                        arguments: r#"{"cmd":"ls"}"#.to_string(),
                    },
                }]),
            },
        }],
        usage: None,
    };
    let items = parse_choices(&response).unwrap();
    assert_eq!(items.len(), 1);
    assert!(matches!(&items[0], ConversationItem::FunctionCall {
        name, call_id, ..
    } if name == "bash" && call_id == "call-abc"));
}

#[test]
fn parse_choices_preserves_reasoning_content_for_tool_calls() {
    let response = ChatResponse {
        id: Some("chatcmpl-456".to_string()),
        choices: vec![ChatChoice {
            message: ChatResponseMessage {
                content: None,
                reasoning_content: Some("preserved reasoning".to_string()),
                tool_calls: Some(vec![ChatToolCall {
                    id: "call-abc".to_string(),
                    function: ChatFunctionCall {
                        name: "bash".to_string(),
                        arguments: r#"{"cmd":"ls"}"#.to_string(),
                    },
                }]),
            },
        }],
        usage: None,
    };

    let items = parse_choices(&response).unwrap();
    assert_eq!(items.len(), 2);
    assert!(matches!(&items[0], ConversationItem::Reasoning {
        content: Some(content), ..
    } if content[0].text.as_deref() == Some("preserved reasoning")));
    assert!(matches!(&items[1], ConversationItem::FunctionCall {
        name, call_id, ..
    } if name == "bash" && call_id == "call-abc"));
}

#[test]
fn parse_choices_empty_response() {
    let response = ChatResponse {
        id: Some("chatcmpl-empty".to_string()),
        choices: vec![],
        usage: None,
    };
    let items = parse_choices(&response).unwrap();
    assert!(items.is_empty());
}

#[test]
fn parse_choices_with_usage() {
    let response = ChatResponse {
        id: Some("chatcmpl-usage".to_string()),
        choices: vec![ChatChoice {
            message: ChatResponseMessage {
                content: Some("Hi".to_string()),
                reasoning_content: None,
                tool_calls: None,
            },
        }],
        usage: Some(ChatUsage {
            prompt_tokens: Some(100),
            completion_tokens: Some(50),
            total_tokens: Some(150),
            prompt_tokens_details: None,
            completion_tokens_details: None,
        }),
    };
    // parse_choices doesn't handle usage — the caller does
    let items = parse_choices(&response).unwrap();
    assert_eq!(items.len(), 1);
}

#[test]
fn parse_response_extracts_cached_tokens() {
    let usage = ChatUsage {
        prompt_tokens: Some(200),
        completion_tokens: Some(80),
        total_tokens: Some(280),
        prompt_tokens_details: Some(PromptTokensDetails {
            cached_tokens: Some(150),
        }),
        completion_tokens_details: None,
    };
    let mapped = Usage {
        input_tokens: usage.prompt_tokens.unwrap_or(0),
        output_tokens: usage.completion_tokens.unwrap_or(0),
        total_tokens: usage.total_tokens.unwrap_or(0),
        input_tokens_details: InputTokensDetails {
            cached_tokens: usage
                .prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens)
                .unwrap_or(0),
        },
        output_tokens_details: OutputTokensDetails {
            reasoning_tokens: usage
                .completion_tokens_details
                .as_ref()
                .and_then(|d| d.reasoning_tokens)
                .unwrap_or(0),
        },
    };
    assert_eq!(mapped.input_tokens_details.cached_tokens, 150);
}

#[test]
fn parse_response_defaults_cached_tokens_when_missing() {
    let usage = ChatUsage {
        prompt_tokens: Some(100),
        completion_tokens: Some(50),
        total_tokens: Some(150),
        prompt_tokens_details: None,
        completion_tokens_details: None,
    };
    let cached = usage
        .prompt_tokens_details
        .as_ref()
        .and_then(|d| d.cached_tokens)
        .unwrap_or(0);
    assert_eq!(cached, 0);
}

#[test]
fn build_messages_empty_history() {
    let history: Vec<ConversationItem> = vec![];
    let msgs = build_messages(&history);
    assert!(msgs.is_empty());
}

#[test]
fn snapshot_simple_conversation() {
    let history = vec![
        ConversationItem::Message {
            role: Role::System,
            content: "You are helpful.".to_string(),
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
    let msgs = build_messages(&history);
    insta::assert_json_snapshot!("build_messages_simple_conversation", msgs);
}

#[test]
fn snapshot_grouped_function_calls() {
    let history = vec![
        ConversationItem::Message {
            role: Role::User,
            content: "do stuff".to_string(),
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
        ConversationItem::FunctionCall {
            id: "fc-2".to_string(),
            call_id: "call-2".to_string(),
            name: "read".to_string(),
            arguments: r#"{"path":"foo.txt"}"#.to_string(),
            timestamp: None,
        },
        ConversationItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: "file.txt".to_string(),
            timestamp: None,
        },
        ConversationItem::FunctionCallOutput {
            call_id: "call-2".to_string(),
            output: "contents".to_string(),
            timestamp: None,
        },
    ];
    let msgs = build_messages(&history);
    insta::assert_json_snapshot!("build_messages_grouped_function_calls", msgs);
}

#[test]
fn snapshot_reasoning_with_assistant_text() {
    let history = vec![
        ConversationItem::Message {
            role: Role::User,
            content: "think".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
        ConversationItem::Reasoning {
            id: "r-1".to_string(),
            summary: Some(vec!["thinking...".to_string()]),
            encrypted_content: None,
            content: Some(vec![ReasoningContent {
                content_type: ReasoningContentKind::ReasoningText,
                text: Some("internal reasoning".to_string()),
            }]),
            timestamp: None,
        },
        ConversationItem::Message {
            role: Role::Assistant,
            content: "done".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
    ];
    let msgs = build_messages(&history);
    insta::assert_json_snapshot!("build_messages_reasoning_with_assistant_text", msgs);
}

#[test]
fn snapshot_reasoning_with_tool_calls() {
    let history = vec![
        ConversationItem::Message {
            role: Role::User,
            content: "do stuff".to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
        ConversationItem::Reasoning {
            id: "r-1".to_string(),
            summary: Some(vec!["thinking...".to_string()]),
            encrypted_content: None,
            content: Some(vec![ReasoningContent {
                content_type: ReasoningContentKind::ReasoningText,
                text: Some("preserved reasoning".to_string()),
            }]),
            timestamp: None,
        },
        ConversationItem::FunctionCall {
            id: "fc-1".to_string(),
            call_id: "call-1".to_string(),
            name: "bash".to_string(),
            arguments: r#"{"cmd":"ls"}"#.to_string(),
            timestamp: None,
        },
    ];
    let msgs = build_messages(&history);
    insta::assert_json_snapshot!("build_messages_reasoning_with_tool_calls", msgs);
}

#[test]
fn snapshot_assistant_text_with_tool_calls() {
    let history = vec![
        ConversationItem::Message {
            role: Role::User,
            content: "do stuff".to_string(),
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
        ConversationItem::Message {
            role: Role::Assistant,
            content: "Let me check that.".to_string(),
            id: Some("msg-1".to_string()),
            status: Some("completed".to_string()),
            timestamp: None,
        },
        ConversationItem::FunctionCallOutput {
            call_id: "call-1".to_string(),
            output: "files".to_string(),
            timestamp: None,
        },
    ];
    let msgs = build_messages(&history);
    insta::assert_json_snapshot!("build_messages_assistant_text_with_tool_calls", msgs);
}

#[test]
fn snapshot_empty_history() {
    let msgs = build_messages(&[]);
    insta::assert_json_snapshot!("build_messages_empty_history", msgs);
}

#[test]
fn snapshot_reasoning_placeholder_injection() {
    let history = vec![
        ConversationItem::Message {
            role: Role::User,
            content: "do stuff".to_string(),
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
    ];

    let mut msgs = build_messages(&history);
    apply_test_strategy("moonshot/kimi-k2.6", &mut msgs);
    insta::assert_json_snapshot!("build_messages_with_reasoning_placeholder", msgs);
}

#[test]
fn snapshot_chat_request_kimi_tool_calls() {
    let history = vec![
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
    ];
    let mut messages = build_messages(&history);
    apply_test_strategy("moonshot/kimi-k2.6", &mut messages);
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
    let request = ChatRequest {
        model: "moonshot/kimi-k2.6",
        messages,
        temperature: Some(0.2),
        top_p: Some(0.9),
        max_completion_tokens: Some(1024),
        tools: Some(convert_tools(&tools)),
        tool_choice: Some("auto".to_string()),
        reasoning_effort: Some(crate::config::ReasoningEffort::High),
    };

    insta::assert_json_snapshot!(
        "chat_request_kimi_tool_calls",
        serde_json::to_value(&request).unwrap()
    );
}

#[test]
fn snapshot_chat_request_full_with_agents_and_skills() {
    let history = full_prompt_history();
    let registry = default_tool_registry();
    let request = ChatRequest {
        model: "test-chat-model",
        messages: build_messages(&history),
        temperature: Some(0.2),
        top_p: Some(0.9),
        max_completion_tokens: None,
        tools: Some(convert_tools(registry.definitions())),
        tool_choice: Some("auto".to_string()),
        reasoning_effort: None,
    };

    assert_json_snapshot_with_environment_filters(
        "chat_request_full_with_agents_and_skills",
        &serde_json::to_value(&request).unwrap(),
    );
}

// =========================================================================
// Malformed Response Tests
// =========================================================================

#[test]
fn parse_choices_empty_message_content() {
    let response = ChatResponse {
        id: Some("chatcmpl-123".to_string()),
        choices: vec![ChatChoice {
            message: ChatResponseMessage {
                content: Some(String::new()), // Empty content
                reasoning_content: None,
                tool_calls: None,
            },
        }],
        usage: None,
    };
    let items = parse_choices(&response).unwrap();
    // Empty content should not create a message item
    assert_eq!(items.len(), 1);
    // But it should create an empty assistant message
    assert!(matches!(&items[0], ConversationItem::Message {
        role: Role::Assistant,
        content,
        ..
    } if content.is_empty()));
}

#[test]
fn parse_choices_none_content_creates_empty_message() {
    let response = ChatResponse {
        id: Some("chatcmpl-123".to_string()),
        choices: vec![ChatChoice {
            message: ChatResponseMessage {
                content: None, // No content
                reasoning_content: None,
                tool_calls: None,
            },
        }],
        usage: None,
    };
    let items = parse_choices(&response).unwrap();
    // Should create an empty assistant message
    assert_eq!(items.len(), 1);
    assert!(matches!(&items[0], ConversationItem::Message {
        role: Role::Assistant,
        content,
        ..
    } if content.is_empty()));
}

#[test]
fn parse_choices_multiple_tool_calls() {
    let response = ChatResponse {
        id: Some("chatcmpl-456".to_string()),
        choices: vec![ChatChoice {
            message: ChatResponseMessage {
                content: None,
                reasoning_content: None,
                tool_calls: Some(vec![
                    ChatToolCall {
                        id: "call-1".to_string(),
                        function: ChatFunctionCall {
                            name: "bash".to_string(),
                            arguments: r#"{"cmd":"ls"}"#.to_string(),
                        },
                    },
                    ChatToolCall {
                        id: "call-2".to_string(),
                        function: ChatFunctionCall {
                            name: "read".to_string(),
                            arguments: r#"{"path":"file.txt"}"#.to_string(),
                        },
                    },
                ]),
            },
        }],
        usage: None,
    };
    let items = parse_choices(&response).unwrap();
    assert_eq!(items.len(), 2);
    assert!(matches!(&items[0], ConversationItem::FunctionCall {
        name, ..
    } if name == "bash"));
    assert!(matches!(&items[1], ConversationItem::FunctionCall {
        name, ..
    } if name == "read"));
}

#[test]
fn parse_choices_tool_calls_with_text_content() {
    // Some models return both tool calls and text content
    let response = ChatResponse {
        id: Some("chatcmpl-789".to_string()),
        choices: vec![ChatChoice {
            message: ChatResponseMessage {
                content: Some("Let me help you with that.".to_string()),
                reasoning_content: None,
                tool_calls: Some(vec![ChatToolCall {
                    id: "call-1".to_string(),
                    function: ChatFunctionCall {
                        name: "bash".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
            },
        }],
        usage: None,
    };
    let items = parse_choices(&response).unwrap();
    // Should have both tool call and message
    assert_eq!(items.len(), 2);
    // Tool call comes first
    assert!(matches!(&items[0], ConversationItem::FunctionCall { .. }));
    // Then the message
    assert!(matches!(&items[1], ConversationItem::Message {
        content,
        ..
    } if content == "Let me help you with that."));
}

#[test]
fn parse_choices_missing_id_fails() {
    let response = ChatResponse {
        id: None, // Missing id
        choices: vec![ChatChoice {
            message: ChatResponseMessage {
                content: Some("Hello".to_string()),
                reasoning_content: None,
                tool_calls: None,
            },
        }],
        usage: None,
    };
    let err = parse_choices(&response).unwrap_err();
    assert_eq!(
        err.to_string(),
        "Chat Completions response is missing required id"
    );
}

#[test]
fn parse_choices_message_with_content_only() {
    let response = ChatResponse {
        id: Some("chatcmpl-123".to_string()),
        choices: vec![ChatChoice {
            message: ChatResponseMessage {
                content: Some("Hello".to_string()),
                reasoning_content: None,
                tool_calls: None,
            },
        }],
        usage: None,
    };
    let items = parse_choices(&response).unwrap();
    assert_eq!(items.len(), 1);
    assert!(matches!(&items[0], ConversationItem::Message {
        content, ..
    } if content == "Hello"));
}

/// Tests for parsing raw HTTP responses via wiremock.
mod response_parsing_tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Create a minimal valid Chat Completions response
    fn minimal_valid_response() -> serde_json::Value {
        serde_json::json!({
            "id": "chatcmpl-123",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello!"
                },
                "finish_reason": "stop"
            }]
        })
    }

    #[tokio::test]
    async fn parse_response_valid_json() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(minimal_valid_response()))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let response = client
            .post(format!("{}/chat/completions", mock_server.uri()))
            .send()
            .await
            .unwrap();

        let result = parse_response(response).await;
        assert!(result.is_ok());
        let turn_result = result.unwrap();
        assert_eq!(turn_result.items.len(), 1);
    }

    #[tokio::test]
    async fn parse_response_invalid_json() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not valid json{broken"))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let response = client
            .post(format!("{}/chat/completions", mock_server.uri()))
            .send()
            .await
            .unwrap();

        let result = parse_response(response).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn parse_response_empty_body() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(""))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let response = client
            .post(format!("{}/chat/completions", mock_server.uri()))
            .send()
            .await
            .unwrap();

        let result = parse_response(response).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn parse_response_missing_choices_fails() {
        let mock_server = MockServer::start().await;

        // Response without "choices" field - should fail because choices is required
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "chatcmpl-123",
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 5
                }
            })))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let response = client
            .post(format!("{}/chat/completions", mock_server.uri()))
            .send()
            .await
            .unwrap();

        let result = parse_response(response).await;
        // Should fail because "choices" is a required field
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn parse_response_missing_id_fails() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Hello!"
                    },
                    "finish_reason": "stop"
                }]
            })))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let response = client
            .post(format!("{}/chat/completions", mock_server.uri()))
            .send()
            .await
            .unwrap();

        let err = parse_response(response).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "Chat Completions response is missing required id"
        );
    }

    #[tokio::test]
    async fn parse_response_with_usage() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "chatcmpl-123",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Hello!"
                    },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 100,
                    "completion_tokens": 50,
                    "total_tokens": 150
                }
            })))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let response = client
            .post(format!("{}/chat/completions", mock_server.uri()))
            .send()
            .await
            .unwrap();

        let result = parse_response(response).await;
        assert!(result.is_ok());
        let turn_result = result.unwrap();
        assert!(turn_result.usage.is_some());
        let usage = turn_result.usage.unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.total_tokens, 150);
    }

    #[tokio::test]
    async fn parse_response_partial_usage() {
        let mock_server = MockServer::start().await;

        // Response with partial usage (some fields missing)
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "chatcmpl-123",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Hello!"
                    },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 100,
                    "completion_tokens": 50
                    // total_tokens is missing
                }
            })))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let response = client
            .post(format!("{}/chat/completions", mock_server.uri()))
            .send()
            .await
            .unwrap();

        let result = parse_response(response).await;
        assert!(result.is_ok());
        let turn_result = result.unwrap();
        let usage = turn_result.usage.unwrap();
        // Should use defaults for missing fields
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.total_tokens, 0); // Default
    }

    #[tokio::test]
    async fn parse_response_with_tool_calls() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "chatcmpl-123",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "reasoning_content": "preserved reasoning",
                        "tool_calls": [{
                            "id": "call-abc",
                            "type": "function",
                            "function": {
                                "name": "bash",
                                "arguments": "{\"cmd\":\"ls\"}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let response = client
            .post(format!("{}/chat/completions", mock_server.uri()))
            .send()
            .await
            .unwrap();

        let result = parse_response(response).await;
        assert!(result.is_ok());
        let turn_result = result.unwrap();
        assert_eq!(turn_result.items.len(), 2);
        assert!(
            matches!(&turn_result.items[0], ConversationItem::Reasoning {
            content: Some(content),
            ..
        } if content[0].text.as_deref() == Some("preserved reasoning"))
        );
        assert!(
            matches!(&turn_result.items[1], ConversationItem::FunctionCall {
            name,
            call_id,
            ..
        } if name == "bash" && call_id == "call-abc")
        );
    }
}
