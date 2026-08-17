use anyhow::{bail, ensure};
use std::borrow::Cow;
use tracing::{debug, trace, warn};

use crate::config::model::ResolvedModelConfig;

use crate::clients::agent::TurnResult;
use crate::clients::backend::{FinalOutputConstraint, ResponseDecodeError};
use crate::clients::chat_types::{
    ChatFunction, ChatFunctionCallRef, ChatMessage, ChatRequest, ChatResponse, ChatTool,
    ChatToolCallRef, ResponseFormat, ResponseFormatJsonSchema,
};
use crate::clients::provider_strategy::ProviderStrategy;
use crate::clients::retry::RequestOverrides;
use crate::clients::tools::Tool;
use crate::session_telemetry::{ProviderTermination, TerminationClassification};
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
pub(super) async fn send_request<'a>(
    client: &reqwest::Client,
    config: &ResolvedModelConfig,
    history: &'a [ConversationItem],
    tools: &'a [Tool],
    overrides: &RequestOverrides,
    constraint: Option<FinalOutputConstraint<'a>>,
) -> anyhow::Result<reqwest::Response> {
    let request = build_request_json(config, history, tools, overrides, constraint)?;
    send_request_json(client, config, request).await
}

/// Build the exact provider-transformed JSON body sent to Chat Completions.
///
/// Keeping construction separate from transport lets opt-in diagnostics show
/// the effective wire request without duplicating provider transformation.
///
/// The body is serialized straight from the typed request so `f32` sampling
/// fields keep their exact provider-facing form; round-tripping through
/// `serde_json::Value` would promote each `f32` to an `f64` (for example
/// turning `0.9` into `0.8999999761581421`) on the wire. Diagnostics parse
/// these bytes rather than re-serializing a promoted value.
pub(super) fn build_request_json<'a>(
    config: &ResolvedModelConfig,
    history: &'a [ConversationItem],
    tools: &'a [Tool],
    overrides: &RequestOverrides,
    constraint: Option<FinalOutputConstraint<'a>>,
) -> anyhow::Result<Vec<u8>> {
    let mut messages = build_messages(history);
    ProviderStrategy::transform_chat_messages(&mut messages);
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
        response_format: constraint.map(|constraint| ResponseFormat {
            format_type: "json_schema",
            json_schema: ResponseFormatJsonSchema {
                name: constraint.name,
                strict: true,
                schema: constraint.schema,
            },
        }),
    };

    serde_json::to_vec(&request).map_err(Into::into)
}

/// Send one already-built Chat Completions JSON request.
///
/// Takes ownership of the serialized body so the transport does not allocate a
/// second copy that stays live across the HTTP send.
pub(super) async fn send_request_json(
    client: &reqwest::Client,
    config: &ResolvedModelConfig,
    request: Vec<u8>,
) -> anyhow::Result<reqwest::Response> {
    let strategy = ProviderStrategy::from_config(config);

    let url = format!(
        "{}/chat/completions",
        config.model_config.base_url.trim_end_matches('/')
    );
    debug!(target: "cake", "{url}");
    if tracing::enabled!(tracing::Level::TRACE) {
        let request_json = String::from_utf8_lossy(&request);
        trace!(target: "cake", "{request_json}");
    }

    let response = strategy
        .apply_headers(
            client
                .post(&url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(request),
        )
        .bearer_auth(&config.api_key)
        .send()
        .await?;

    Ok(response)
}

/// Read the response body and decode the Chat Completions response, attaching
/// a bounded preview of the raw body to the error when the 2xx body is not the
/// expected JSON shape, so an opaque provider or proxy body is diagnosable
/// from the error text.
async fn read_chat_response(response: reqwest::Response) -> anyhow::Result<ChatResponse> {
    let bytes = response.bytes().await?;
    serde_json::from_slice::<ChatResponse>(&bytes)
        .map_err(|error| ResponseDecodeError::new("Chat Completions", &bytes, error).into())
}

/// Parse an HTTP response from the Chat Completions API into a `TurnResult`.
///
/// # Errors
///
/// Returns an error if the response body cannot be read or deserialized. A
/// deserialization failure carries a bounded preview of the raw body so an
/// opaque provider or proxy 2xx is diagnosable from the error text instead of
/// reqwest's generic "error decoding response body".
pub(super) async fn parse_response(response: reqwest::Response) -> anyhow::Result<TurnResult> {
    let chat_response = read_chat_response(response).await?;
    trace!(target: "cake", "{chat_response:?}");

    let response_id = chat_response.id.as_deref().unwrap_or("<missing id>");

    if chat_response.usage.is_none() {
        warn!(
            target: "cake",
            response_id,
            "Chat Completions response missing 'usage' field; token accounting will be unavailable"
        );
    }

    let usage = chat_response.usage.as_ref().map(|u| {
        if u.prompt_tokens.is_none() {
            warn!(
                target: "cake",
                response_id,
                field = "prompt_tokens",
                "Chat Completions usage missing field, defaulting to 0"
            );
        }
        if u.completion_tokens.is_none() {
            warn!(
                target: "cake",
                response_id,
                field = "completion_tokens",
                "Chat Completions usage missing field, defaulting to 0"
            );
        }
        if u.total_tokens.is_none() {
            warn!(
                target: "cake",
                response_id,
                field = "total_tokens",
                "Chat Completions usage missing field, defaulting to 0"
            );
        }
        Usage {
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
        }
    });

    let items = parse_choices(&chat_response)?;
    let termination = chat_response
        .choices
        .first()
        .and_then(chat_choice_termination);

    Ok(TurnResult {
        items,
        usage,
        termination,
        provider_request_id: chat_response.id,
    })
}

fn chat_choice_termination(
    choice: &crate::clients::chat_types::ChatChoice,
) -> Option<ProviderTermination> {
    if choice.message.refusal.is_some() {
        return Some(ProviderTermination {
            classification: TerminationClassification::Failed,
            provider_status: None,
            provider_reason: choice.finish_reason.clone(),
        });
    }
    choice.finish_reason.as_deref().map(chat_termination)
}

fn chat_termination(reason: &str) -> ProviderTermination {
    let classification = match reason {
        "stop" => TerminationClassification::Completed,
        "tool_calls" | "function_call" => TerminationClassification::ToolCalls,
        "length" | "max_tokens" | "max_output_tokens" => TerminationClassification::TokenLimit,
        "content_filter" => TerminationClassification::ContentFilter,
        "refusal" | "refused" => TerminationClassification::Failed,
        _ => TerminationClassification::Unknown,
    };
    ProviderTermination {
        classification,
        provider_status: None,
        provider_reason: Some(reason.to_string()),
    }
}

/// Convert internal conversation history to Chat Completions messages.
///
/// This handles the key translation:
/// - `ConversationItem::Message` → `ChatMessage` with role/content
/// - Consecutive `FunctionCall` items → one assistant message with `tool_calls`
/// - A `FunctionCall` that immediately follows an `Assistant` message → merged
///   backward into that assistant message (the canonical Responses API ordering)
/// - A `FunctionCall` that precedes an `Assistant` message → merged forward into
///   the next assistant message (legacy Chat Completions ordering)
/// - `FunctionCallOutput` → tool role message with `tool_call_id`
/// - `Reasoning` → preserved as provider-specific `reasoning_content` on the
///   next assistant message for providers like Moonshot/Kimi
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
}

impl<'a> ChatMessageBuilder<'a> {
    const fn new() -> Self {
        Self {
            messages: Vec::new(),
            pending_tool_calls: Vec::new(),
            pending_reasoning_content: None,
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
        let role_str = chat_role_name(role);

        if matches!(role, Role::Assistant) && !self.pending_tool_calls.is_empty() {
            let tool_calls = self.take_pending_tool_calls();
            self.push_assistant_message(Some(Cow::Borrowed(content)), Some(tool_calls));
            return;
        }

        if !matches!(role, Role::Assistant) {
            self.pending_reasoning_content = None;
        }
        self.flush_pending_tool_calls();

        let reasoning_content = matches!(role, Role::Assistant)
            .then(|| self.pending_reasoning_content.take())
            .flatten();
        self.messages.push(ChatMessage {
            role: Cow::Borrowed(role_str),
            content: Some(Cow::Borrowed(content)),
            reasoning_content,
            tool_calls: None,
            tool_call_id: None,
        });
    }

    fn push_function_call(&mut self, call_id: &'a str, name: &'a str, arguments: &'a str) {
        let tool_call = ChatToolCallRef {
            id: Cow::Borrowed(call_id),
            type_: Cow::Borrowed("function"),
            function: ChatFunctionCallRef {
                name: Cow::Borrowed(name),
                arguments: Cow::Borrowed(arguments),
            },
        };

        // Backward merge: when a FunctionCall immediately follows an assistant
        // message, attach the tool call to that message. This handles the new
        // [Message(assistant), FunctionCall] conversation ordering.
        if let Some(last) = self.messages.last_mut()
            && last.role == "assistant"
        {
            if let Some(tool_calls) = last.tool_calls.as_mut() {
                tool_calls.push(tool_call);
            } else {
                last.tool_calls = Some(vec![tool_call]);
            }
            return;
        }

        self.pending_tool_calls.push(tool_call);
    }

    fn push_function_call_output(&mut self, call_id: &'a str, output: &'a str) {
        self.flush_pending_tool_calls();
        self.pending_reasoning_content = None;

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

const fn chat_role_name(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::Developer => "developer",
        Role::Assistant => "assistant",
        Role::User => "user",
        Role::Tool => "tool",
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
        warn!(
            target: "cake",
            response_id = response_id,
            "Chat Completions response has no choices; returning empty turn"
        );
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

    // Extract text content first so that `stream-json` events match the
    // canonical Responses API ordering: [reasoning, message, function_call].
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

    // Extract tool calls (may coexist with text content)
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

    // If we got tool calls but no text content, that's fine — the agent loop
    // will execute the tools and continue. But if we got neither, add an
    // empty assistant message so the caller knows the model responded.
    if items.is_empty() {
        warn!(
            target: "cake",
            response_id = response_id,
            "Chat Completions response has no content, tool_calls, or reasoning_content; returning empty assistant message"
        );
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
