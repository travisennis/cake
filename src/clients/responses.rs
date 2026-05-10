use tracing::{debug, trace};

use crate::config::model::ResolvedModelConfig;
use crate::models::Role;

use crate::clients::agent::TurnResult;
use crate::clients::provider_strategy::ProviderStrategy;
use crate::clients::retry::RequestOverrides;
use crate::clients::tools::Tool;
use crate::clients::types::{
    ApiResponse, ApiUsage, ConversationItem, InputTokensDetails, OutputMessage,
    OutputTokensDetails, ReasoningConfig, ReasoningContentKind, Request, ResponsesApiInputItem,
    Usage,
};

// =============================================================================
// Responses API Backend
// =============================================================================

/// Send a request to the Responses API, returning the raw HTTP response.
///
/// # Errors
///
/// Returns an error if the HTTP request fails.
pub(super) async fn send_request(
    client: &reqwest::Client,
    config: &ResolvedModelConfig,
    history: &[ConversationItem],
    tools: &[Tool],
    overrides: &RequestOverrides,
) -> anyhow::Result<reqwest::Response> {
    let strategy = ProviderStrategy::from_config(config);
    let provider_config = strategy.responses_provider_config();

    let max_output_tokens = overrides
        .max_output_tokens
        .or(config.config.max_output_tokens);
    let reasoning_max_tokens = overrides
        .reasoning_max_tokens
        .or(config.config.reasoning_max_tokens);

    let reasoning = (config.config.reasoning_effort.is_some()
        || config.config.reasoning_summary.is_some()
        || reasoning_max_tokens.is_some())
    .then(|| ReasoningConfig {
        effort: config.config.reasoning_effort,
        summary: config.config.reasoning_summary.clone(),
        max_tokens: reasoning_max_tokens,
    });

    let (instructions, non_system_history) = extract_instructions(history)?;

    let prompt = Request {
        model: &config.config.model,
        input: build_input(non_system_history),
        instructions,
        temperature: config.config.temperature,
        top_p: config.config.top_p,
        max_output_tokens,
        tools: Some(tools.to_vec()),
        tool_choice: Some("auto".to_string()),
        provider: provider_config,
        reasoning,
    };

    let url = format!("{}/responses", config.config.base_url.trim_end_matches('/'));
    debug!(target: "cake", "{url}");
    if tracing::enabled!(tracing::Level::TRACE) {
        let prompt_json = serde_json::to_string(&prompt)?;
        trace!(target: "cake", "{prompt_json}");
    }

    let response = strategy
        .apply_headers(client.post(&url).json(&prompt))
        .bearer_auth(&config.api_key)
        .send()
        .await?;

    Ok(response)
}

/// Parse an HTTP response from the Responses API into a `TurnResult`.
///
/// # Errors
///
/// Returns an error if the response body cannot be deserialized.
pub(super) async fn parse_response(response: reqwest::Response) -> anyhow::Result<TurnResult> {
    let api_response = response.json::<ApiResponse>().await?;
    trace!(target: "cake", "{api_response:?}");

    let usage = api_response.usage.as_ref().map(map_usage);
    let items = parse_output_items(&api_response)?;

    Ok(TurnResult { items, usage })
}

/// Map API-level usage to the canonical `Usage` type.
fn map_usage(api_usage: &ApiUsage) -> Usage {
    Usage {
        input_tokens: api_usage.input_tokens.unwrap_or(0),
        output_tokens: api_usage.output_tokens.unwrap_or(0),
        total_tokens: api_usage.total_tokens.unwrap_or(0),
        input_tokens_details: InputTokensDetails {
            cached_tokens: api_usage
                .input_tokens_details
                .as_ref()
                .map_or(0, |d| d.cached_tokens.unwrap_or(0)),
        },
        output_tokens_details: OutputTokensDetails {
            reasoning_tokens: api_usage
                .output_tokens_details
                .as_ref()
                .map_or(0, |d| d.reasoning_tokens.unwrap_or(0)),
        },
    }
}

/// Extract the system prompt from the conversation history, returning it
/// separately as the `instructions` field for the Responses API.
///
/// The Responses API expects system-level instructions in a top-level
/// `instructions` field rather than as a message in the `input` array.
///
/// # Invariants
///
/// Returns `None` if no system message exists. Any system message in
/// the history must be first; if one appears at a later index the
/// function returns an error. This protects callers from accidentally
/// sending truncated or malformed conversation history.
fn extract_instructions(
    history: &[ConversationItem],
) -> anyhow::Result<(Option<&str>, &[ConversationItem])> {
    let system_idx = history.iter().position(|item| {
        matches!(
            item,
            ConversationItem::Message {
                role: Role::System,
                ..
            }
        )
    });

    match system_idx {
        Some(0) => {
            let ConversationItem::Message { content, .. } = &history[0] else {
                return Ok((None, history));
            };
            Ok((Some(content.as_str()), &history[1..]))
        },
        Some(idx @ 1..) => anyhow::bail!(
            "invalid Responses API conversation history: system message found at index {idx}; \
             system messages are only valid as the first history item"
        ),
        None => Ok((None, history)),
    }
}

/// Build the input array for the Responses API from conversation history.
fn build_input(history: &[ConversationItem]) -> Vec<ResponsesApiInputItem<'_>> {
    history
        .iter()
        .map(ConversationItem::to_api_input_item)
        .collect()
}

/// Parse the output items from an API response into `ConversationItem` values.
///
/// # Errors
///
/// Returns an error if a function call item is missing required fields.
fn parse_output_items(api_response: &ApiResponse) -> anyhow::Result<Vec<ConversationItem>> {
    let mut items = Vec::new();
    let mut unknown_output_types = Vec::new();

    for (index, output) in api_response.output.iter().enumerate() {
        match output.msg_type.as_str() {
            "reasoning" => {
                if let Some(id) = &output.id {
                    let summary = output
                        .summary
                        .clone()
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| {
                            output
                                .content
                                .as_ref()
                                .map(|c| {
                                    c.iter()
                                        .filter(|item| {
                                            item.content_type
                                                == ReasoningContentKind::ReasoningText.as_str()
                                        })
                                        .filter_map(|item| item.text.clone())
                                        .collect()
                                })
                                .unwrap_or_default()
                        });

                    let content = output.content.as_ref().map(|c| {
                        c.iter()
                            .map(|item| super::types::ReasoningContent {
                                content_type: item.content_type.clone().into(),
                                text: item.text.clone(),
                            })
                            .collect()
                    });

                    let timestamp = chrono::Utc::now();
                    items.push(ConversationItem::Reasoning {
                        id: id.clone(),
                        summary,
                        encrypted_content: output.encrypted_content.clone(),
                        content,
                        timestamp: Some(timestamp),
                    });
                }
            },
            "function_call" => {
                items.push(parse_function_call_output(api_response, output, index)?);
            },
            "message" => {
                let text = output
                    .content
                    .as_ref()
                    .and_then(|c| c.iter().find(|item| item.content_type == "output_text"))
                    .and_then(|item| item.text.clone())
                    .unwrap_or_default();

                let timestamp = chrono::Utc::now();
                items.push(ConversationItem::Message {
                    role: Role::Assistant,
                    content: text,
                    id: output.id.clone(),
                    status: output.status.clone(),
                    timestamp: Some(timestamp),
                });
            },
            unknown_type => {
                tracing::warn!(
                    response_id = api_response.id.as_deref().unwrap_or("<missing id>"),
                    output_index = index,
                    output_id = output.id.as_deref(),
                    output_type = unknown_type,
                    "Unknown Responses API output type"
                );
                unknown_output_types.push((index, unknown_type.to_string()));
            },
        }
    }

    if items.is_empty() && !unknown_output_types.is_empty() {
        return Err(unknown_output_type_error(
            api_response,
            &unknown_output_types,
        ));
    }

    Ok(items)
}

fn unknown_output_type_error(
    api_response: &ApiResponse,
    unknown_output_types: &[(usize, String)],
) -> anyhow::Error {
    let unknown_types = unknown_output_types
        .iter()
        .map(|(index, output_type)| format!("output[{index}] type '{output_type}'"))
        .collect::<Vec<_>>()
        .join(", ");

    anyhow::anyhow!(
        "Responses API response {} contained only unknown output type(s): {unknown_types}",
        api_response.id.as_deref().unwrap_or("<missing id>")
    )
}

fn parse_function_call_output(
    api_response: &ApiResponse,
    output: &OutputMessage,
    index: usize,
) -> anyhow::Result<ConversationItem> {
    let (Some(id), Some(call_id), Some(name), Some(arguments)) = (
        output.id.as_ref(),
        output.call_id.as_ref(),
        output.name.as_ref(),
        output.arguments.as_ref(),
    ) else {
        return Err(malformed_function_call_error(api_response, output, index));
    };

    if id.is_empty() || call_id.is_empty() || name.is_empty() || arguments.is_empty() {
        return Err(malformed_function_call_error(api_response, output, index));
    }

    let timestamp = chrono::Utc::now();
    Ok(ConversationItem::FunctionCall {
        id: id.clone(),
        call_id: call_id.clone(),
        name: name.clone(),
        arguments: arguments.clone(),
        timestamp: Some(timestamp),
    })
}

fn malformed_function_call_error(
    api_response: &ApiResponse,
    output: &OutputMessage,
    index: usize,
) -> anyhow::Error {
    let missing_fields = [
        ("id", output.id.as_deref()),
        ("call_id", output.call_id.as_deref()),
        ("name", output.name.as_deref()),
        ("arguments", output.arguments.as_deref()),
    ]
    .into_iter()
    .filter_map(|(field, value)| match value {
        Some(value) if !value.is_empty() => None,
        _ => Some(field),
    })
    .collect::<Vec<_>>();

    anyhow::anyhow!(
        "malformed Responses API function_call at output[{index}] in response {}: missing or empty required field(s): {}",
        api_response.id.as_deref().unwrap_or("<missing id>"),
        missing_fields.join(", ")
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::clients::types::{OutputContent, OutputMessage, ProviderConfig};

    fn input_json(history: &[ConversationItem]) -> Vec<serde_json::Value> {
        build_input(history)
            .into_iter()
            .map(|item| serde_json::to_value(item).unwrap())
            .collect()
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
                role: Some("assistant".to_string()),
                status: Some("completed".to_string()),
                content: Some(vec![OutputContent {
                    content_type: "output_text".to_string(),
                    text: Some("Hello!".to_string()),
                }]),
                encrypted_content: None,
                summary: None,
            }],
            usage: None,
            status: None,
            error: None,
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
                role: None,
                status: None,
                content: None,
                encrypted_content: None,
                summary: None,
            }],
            usage: None,
            status: None,
            error: None,
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
                role: None,
                status: None,
                content: Some(vec![OutputContent {
                    content_type: "reasoning_text".to_string(),
                    text: Some("thinking...".to_string()),
                }]),
                encrypted_content: None,
                summary: None,
            }],
            usage: None,
            status: None,
            error: None,
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
                role: None,
                status: None,
                content: None,
                encrypted_content: Some("gAAAAABencrypted...".to_string()),
                summary: Some(vec!["step 1".to_string(), "step 2".to_string()]),
            }],
            usage: None,
            status: None,
            error: None,
        };
        let items = parse_output_items(&response).unwrap();
        assert_eq!(items.len(), 1);
        if let ConversationItem::Reasoning {
            summary,
            encrypted_content,
            ..
        } = &items[0]
        {
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
                role: None,
                status: None,
                content: Some(vec![OutputContent {
                    content_type: "reasoning_text".to_string(),
                    text: Some("deep reasoning here".to_string()),
                }]),
                encrypted_content: None,
                summary: None,
            }],
            usage: None,
            status: None,
            error: None,
        };
        let items = parse_output_items(&response).unwrap();
        assert_eq!(items.len(), 1);
        let api_input = items[0].to_api_input();
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
                role: None,
                status: None,
                content: Some(vec![OutputContent {
                    content_type: "provider_specific_reasoning".to_string(),
                    text: Some("opaque reasoning".to_string()),
                }]),
                encrypted_content: None,
                summary: None,
            }],
            usage: None,
            status: None,
            error: None,
        };

        let items = parse_output_items(&response).unwrap();
        let api_input = items[0].to_api_input();

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
                role: None,
                status: None,
                content: None,
                encrypted_content: None,
                summary: None,
            }],
            usage: None,
            status: None,
            error: None,
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
                    role: None,
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
                    role: Some("assistant".to_string()),
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
            status: None,
            error: None,
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
                    role: None,
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
                    role: None,
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
            status: None,
            error: None,
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
                role: None,
                status: None,
                content: None,
                encrypted_content: None,
                summary: None,
            }],
            usage: None,
            status: None,
            error: None,
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
            tools: Some(tools),
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

    // =========================================================================
    // Malformed Response Tests
    // =========================================================================

    #[test]
    fn parse_output_items_empty_output_array() {
        let response = ApiResponse {
            id: None,
            output: vec![],
            usage: None,
            status: None,
            error: None,
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
                role: None,
                status: None,
                content: Some(vec![OutputContent {
                    content_type: "reasoning_text".to_string(),
                    text: Some("thinking...".to_string()),
                }]),
                encrypted_content: None,
                summary: None,
            }],
            usage: None,
            status: None,
            error: None,
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
                role: None,
                status: None,
                content: None,
                encrypted_content: None,
                summary: None,
            }],
            usage: None,
            status: None,
            error: None,
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
                role: None,
                status: Some("completed".to_string()),
                content: Some(vec![]), // Empty content array
                encrypted_content: None,
                summary: None,
            }],
            usage: None,
            status: None,
            error: None,
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
                role: None,
                status: Some("completed".to_string()),
                content: Some(vec![OutputContent {
                    content_type: "image".to_string(), // Not output_text
                    text: Some("image data".to_string()),
                }]),
                encrypted_content: None,
                summary: None,
            }],
            usage: None,
            status: None,
            error: None,
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
                role: None,
                status: None,
                content: None,
                encrypted_content: None,
                summary: Some(vec!["step 1".to_string(), "step 2".to_string()]),
            }],
            usage: None,
            status: None,
            error: None,
        };
        let items = parse_output_items(&response).unwrap();
        assert_eq!(items.len(), 1);
        if let ConversationItem::Reasoning { summary, .. } = &items[0] {
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
                role: None,
                status: None,
                content: Some(vec![OutputContent {
                    content_type: "reasoning_text".to_string(),
                    text: Some("thinking...".to_string()),
                }]),
                encrypted_content: None,
                summary: None, // No summary, should derive from content
            }],
            usage: None,
            status: None,
            error: None,
        };
        let items = parse_output_items(&response).unwrap();
        assert_eq!(items.len(), 1);
        if let ConversationItem::Reasoning { summary, .. } = &items[0] {
            // Summary should be derived from content
            assert_eq!(summary.len(), 1);
            assert_eq!(summary[0], "thinking...");
        } else {
            panic!("Expected Reasoning item");
        }
    }
}

/// Tests for parsing raw HTTP responses
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod response_parsing_tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Create a minimal valid response JSON
    fn minimal_valid_response() -> serde_json::Value {
        serde_json::json!({
            "output": [{
                "type": "message",
                "id": "msg-1",
                "status": "completed",
                "content": [{
                    "type": "output_text",
                    "text": "Hello!"
                }]
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
            .post(format!("{}/responses", mock_server.uri()))
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
            .post(format!("{}/responses", mock_server.uri()))
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
            .post(format!("{}/responses", mock_server.uri()))
            .send()
            .await
            .unwrap();

        let result = parse_response(response).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn parse_response_missing_output_field_fails() {
        let mock_server = MockServer::start().await;

        // Response without "output" field - should fail because output is required
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "resp-123",
                "status": "completed"
            })))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let response = client
            .post(format!("{}/responses", mock_server.uri()))
            .send()
            .await
            .unwrap();

        let result = parse_response(response).await;
        // Should fail because "output" is a required field
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn parse_response_function_call_missing_required_fields_fails() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "resp-123",
                "output": [{
                    "type": "function_call",
                    "call_id": "call-1",
                    "arguments": "{}"
                }]
            })))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let response = client
            .post(format!("{}/responses", mock_server.uri()))
            .send()
            .await
            .unwrap();

        let error = parse_response(response).await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("malformed Responses API function_call"));
        assert!(message.contains("resp-123"));
        assert!(message.contains("id, name"));
    }

    #[tokio::test]
    async fn parse_response_with_usage() {
        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "output": [{
                    "type": "message",
                    "id": "msg-1",
                    "status": "completed",
                    "content": [{
                        "type": "output_text",
                        "text": "Hello!"
                    }]
                }],
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 50,
                    "total_tokens": 150,
                    "input_tokens_details": {
                        "cached_tokens": 20
                    },
                    "output_tokens_details": {
                        "reasoning_tokens": 10
                    }
                }
            })))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let response = client
            .post(format!("{}/responses", mock_server.uri()))
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
        assert_eq!(usage.input_tokens_details.cached_tokens, 20);
        assert_eq!(usage.output_tokens_details.reasoning_tokens, 10);
    }

    #[tokio::test]
    async fn parse_response_partial_usage() {
        let mock_server = MockServer::start().await;

        // Response with partial usage (some fields missing)
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "output": [{
                    "type": "message",
                    "id": "msg-1",
                    "content": [{
                        "type": "output_text",
                        "text": "Hello!"
                    }]
                }],
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 50
                    // total_tokens and details are missing
                }
            })))
            .mount(&mock_server)
            .await;

        let client = reqwest::Client::new();
        let response = client
            .post(format!("{}/responses", mock_server.uri()))
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
        assert_eq!(usage.input_tokens_details.cached_tokens, 0); // Default
        assert_eq!(usage.output_tokens_details.reasoning_tokens, 0); // Default
    }
}
