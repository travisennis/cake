use anyhow::{bail, ensure};
use std::borrow::Cow;
use tracing::{debug, trace};

use crate::config::model::ResolvedModelConfig;

use crate::clients::agent::TurnResult;
use crate::clients::chat_types::{
    ChatFunction, ChatFunctionCallRef, ChatMessage, ChatRequest, ChatResponse, ChatTool,
    ChatToolCallRef,
};
use crate::clients::provider_strategy::ProviderStrategy;
use crate::clients::retry::RequestOverrides;
use crate::clients::tools::Tool;
use crate::types::{
    ConversationItem, InputTokensDetails, OutputTokensDetails, ReasoningContentKind, Role, Usage,
};

// =============================================================================
// Chat Completions API Backend
// =============================================================================

/// Send a request to the Chat Completions API, returning the raw HTTP response.
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
    let mut messages = build_messages(history);
    strategy.transform_chat_messages(&mut messages);
    let chat_tools = convert_tools(tools);

    let request = ChatRequest {
        model: &config.model_config.model,
        messages,
        temperature: config.model_config.temperature,
        top_p: config.model_config.top_p,
        max_completion_tokens: overrides
            .max_output_tokens
            .or(config.model_config.max_output_tokens),
        tools: if chat_tools.is_empty() {
            None
        } else {
            Some(chat_tools)
        },
        tool_choice: if tools.is_empty() {
            None
        } else {
            Some("auto".to_string())
        },
        reasoning_effort: config.model_config.reasoning_effort,
    };

    let url = format!(
        "{}/chat/completions",
        config.model_config.base_url.trim_end_matches('/')
    );
    debug!(target: "cake", "{url}");
    if tracing::enabled!(tracing::Level::TRACE) {
        let request_json = serde_json::to_string(&request)?;
        trace!(target: "cake", "{request_json}");
    }

    let response = strategy
        .apply_headers(client.post(&url).json(&request))
        .bearer_auth(&config.api_key)
        .send()
        .await?;

    Ok(response)
}

/// Parse an HTTP response from the Chat Completions API into a `TurnResult`.
///
/// # Errors
///
/// Returns an error if the response body cannot be deserialized.
pub(super) async fn parse_response(response: reqwest::Response) -> anyhow::Result<TurnResult> {
    let chat_response = response.json::<ChatResponse>().await?;
    trace!(target: "cake", "{chat_response:?}");

    let usage = chat_response.usage.as_ref().map(|u| Usage {
        input_tokens: u.prompt_tokens.unwrap_or(0),
        output_tokens: u.completion_tokens.unwrap_or(0),
        total_tokens: u.total_tokens.unwrap_or(0),
        input_tokens_details: InputTokensDetails {
            cached_tokens: u
                .prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens)
                .unwrap_or(0),
        },
        output_tokens_details: OutputTokensDetails {
            reasoning_tokens: u
                .completion_tokens_details
                .as_ref()
                .and_then(|d| d.reasoning_tokens)
                .unwrap_or(0),
        },
    });

    let items = parse_choices(&chat_response)?;

    Ok(TurnResult { items, usage })
}

/// Convert internal conversation history to Chat Completions messages.
///
/// This handles the key translation:
/// - `ConversationItem::Message` → `ChatMessage` with role/content
/// - Consecutive `FunctionCall` items → one assistant message with `tool_calls`
/// - `FunctionCallOutput` → tool role message with `tool_call_id`
/// - `Reasoning` → preserved as provider-specific `reasoning_content` on the
///   next assistant message for providers like Moonshot/Kimi
///
/// When a `FunctionCall` is followed by an `Assistant` message, the tool calls
/// are merged into that assistant message rather than emitted separately.
fn build_messages(history: &[ConversationItem]) -> Vec<ChatMessage<'_>> {
    let mut builder = ChatMessageBuilder::new();
    for item in history {
        builder.push_item(item);
    }
    builder.finish()
}

struct ChatMessageBuilder<'a> {
    messages: Vec<ChatMessage<'a>>,
    pending_tool_calls: Vec<ChatToolCallRef<'a>>,
    pending_reasoning_content: Option<Cow<'a, str>>,
    pending_developer_context: Vec<&'a str>,
}

impl<'a> ChatMessageBuilder<'a> {
    const fn new() -> Self {
        Self {
            messages: Vec::new(),
            pending_tool_calls: Vec::new(),
            pending_reasoning_content: None,
            pending_developer_context: Vec::new(),
        }
    }

    fn push_item(&mut self, item: &'a ConversationItem) {
        match item {
            ConversationItem::Message { role, content, .. } => {
                self.push_message(*role, content);
            },
            ConversationItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                self.push_function_call(call_id, name, arguments);
            },
            ConversationItem::FunctionCallOutput {
                call_id, output, ..
            } => {
                self.push_function_call_output(call_id, output);
            },
            ConversationItem::Reasoning { content, .. } => {
                self.remember_reasoning(content.as_deref());
            },
        }
    }

    fn push_message(&mut self, role: Role, content: &'a str) {
        let Some(role_str) = chat_role_name(role) else {
            self.pending_developer_context.push(content);
            return;
        };

        let content = self.content_with_developer_context(role, content);

        if matches!(role, Role::Assistant) && !self.pending_tool_calls.is_empty() {
            let tool_calls = self.take_pending_tool_calls();
            self.push_assistant_message(Some(content), Some(tool_calls));
            return;
        }

        self.flush_pending_tool_calls();

        let reasoning_content = matches!(role, Role::Assistant)
            .then(|| self.pending_reasoning_content.take())
            .flatten();
        self.messages.push(ChatMessage {
            role: Cow::Borrowed(role_str),
            content: Some(content),
            reasoning_content,
            tool_calls: None,
            tool_call_id: None,
        });
    }

    fn push_function_call(&mut self, call_id: &'a str, name: &'a str, arguments: &'a str) {
        self.pending_tool_calls.push(ChatToolCallRef {
            id: Cow::Borrowed(call_id),
            type_: Cow::Borrowed("function"),
            function: ChatFunctionCallRef {
                name: Cow::Borrowed(name),
                arguments: Cow::Borrowed(arguments),
            },
        });
    }

    fn push_function_call_output(&mut self, call_id: &'a str, output: &'a str) {
        self.flush_pending_tool_calls();

        self.messages.push(ChatMessage {
            role: Cow::Borrowed("tool"),
            content: Some(Cow::Borrowed(output)),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(Cow::Borrowed(call_id)),
        });
    }

    fn remember_reasoning(&mut self, content: Option<&'a [crate::types::ReasoningContent]>) {
        self.pending_reasoning_content = extract_reasoning_content(content).map(Cow::Borrowed);
    }

    fn content_with_developer_context(&mut self, role: Role, content: &'a str) -> Cow<'a, str> {
        if !matches!(role, Role::User) {
            return Cow::Borrowed(content);
        }

        if self.pending_developer_context.is_empty() {
            return Cow::Borrowed(content);
        }

        let content = Cow::Owned(format!(
            "{}\n\nUser message:\n{}",
            self.pending_developer_context.join("\n\n"),
            content
        ));
        self.pending_developer_context.clear();
        content
    }

    fn flush_pending_tool_calls(&mut self) {
        if self.pending_tool_calls.is_empty() {
            return;
        }

        let tool_calls = self.take_pending_tool_calls();
        self.push_assistant_message(None, Some(tool_calls));
    }

    fn push_assistant_message(
        &mut self,
        content: Option<Cow<'a, str>>,
        tool_calls: Option<Vec<ChatToolCallRef<'a>>>,
    ) {
        self.messages.push(ChatMessage {
            role: Cow::Borrowed("assistant"),
            content,
            reasoning_content: self.pending_reasoning_content.take(),
            tool_calls,
            tool_call_id: None,
        });
    }

    fn take_pending_tool_calls(&mut self) -> Vec<ChatToolCallRef<'a>> {
        std::mem::take(&mut self.pending_tool_calls)
    }

    fn finish(mut self) -> Vec<ChatMessage<'a>> {
        self.flush_pending_tool_calls();
        self.messages
    }
}

const fn chat_role_name(role: Role) -> Option<&'static str> {
    match role {
        Role::System => Some("system"),
        Role::Developer => None,
        Role::Assistant => Some("assistant"),
        Role::User => Some("user"),
        Role::Tool => Some("tool"),
    }
}

fn extract_reasoning_content(content: Option<&[crate::types::ReasoningContent]>) -> Option<&str> {
    content.and_then(|items| items.iter().find_map(|item| item.text.as_deref()))
}

/// Convert internal tool definitions to Chat Completions format.
fn convert_tools(tools: &[Tool]) -> Vec<ChatTool> {
    tools
        .iter()
        .map(|tool| ChatTool {
            type_: "function".to_string(),
            function: ChatFunction {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.parameters.clone(),
            },
        })
        .collect()
}

/// Parse the choices from a Chat Completions response into `ConversationItem` values.
fn parse_choices(response: &ChatResponse) -> anyhow::Result<Vec<ConversationItem>> {
    let mut items = Vec::new();
    let response_id = required_response_id(response)?;

    let Some(choice) = response.choices.first() else {
        return Ok(items);
    };

    let message = &choice.message;
    let timestamp = chrono::Utc::now();

    if let Some(reasoning_content) = &message.reasoning_content {
        items.push(ConversationItem::Reasoning {
            id: response_id.clone(),
            summary: None,
            encrypted_content: None,
            content: Some(vec![crate::types::ReasoningContent {
                content_type: ReasoningContentKind::ReasoningText,
                text: Some(reasoning_content.clone()),
            }]),
            timestamp: Some(timestamp),
        });
    }

    // Extract tool calls first
    if let Some(tool_calls) = &message.tool_calls {
        for tc in tool_calls {
            items.push(ConversationItem::FunctionCall {
                id: tc.id.clone(),
                call_id: tc.id.clone(),
                name: tc.function.name.clone(),
                arguments: tc.function.arguments.clone(),
                timestamp: Some(timestamp),
            });
        }
    }

    // Extract text content (may coexist with tool calls)
    if let Some(content) = &message.content
        && !content.is_empty()
    {
        items.push(ConversationItem::Message {
            role: Role::Assistant,
            content: content.clone(),
            id: Some(response_id.clone()),
            status: Some("completed".to_string()),
            timestamp: Some(timestamp),
        });
    }

    // If we got tool calls but no text content, that's fine — the agent loop
    // will execute the tools and continue. But if we got neither, add an
    // empty assistant message so the caller knows the model responded.
    if items.is_empty() {
        items.push(ConversationItem::Message {
            role: Role::Assistant,
            content: String::new(),
            id: Some(response_id),
            status: Some("completed".to_string()),
            timestamp: Some(timestamp),
        });
    }

    Ok(items)
}

fn required_response_id(response: &ChatResponse) -> anyhow::Result<String> {
    let Some(id) = &response.id else {
        bail!("Chat Completions response is missing required id");
    };

    ensure!(
        !id.is_empty(),
        "Chat Completions response is missing required id"
    );

    Ok(id.clone())
}

#[cfg(test)]
#[path = "chat_completions_tests.rs"]
mod tests;
