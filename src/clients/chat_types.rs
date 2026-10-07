use serde::{Deserialize, Serialize};
use std::borrow::Cow;

use crate::config::ReasoningEffort;
use crate::types::{ImagePart, Role};

// =============================================================================
// Chat Completions API Request DTOs (serialization only - can borrow)
// =============================================================================

#[derive(Serialize)]
pub(super) struct ChatRequest<'a> {
    pub(super) model: &'a str,
    pub(super) messages: Vec<ChatMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tools: Option<Vec<ChatTool<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reasoning_effort: Option<ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) response_format: Option<ResponseFormat<'a>>,
}

/// Structured-output configuration for correction turns:
/// `{"type": "json_schema", "json_schema": {...}}`.
#[derive(Debug, Serialize)]
pub(super) struct ResponseFormat<'a> {
    #[serde(rename = "type")]
    pub(super) format_type: &'static str,
    pub(super) json_schema: ResponseFormatJsonSchema<'a>,
}

/// The named schema payload inside [`ResponseFormat`].
#[derive(Debug, Serialize)]
pub(super) struct ResponseFormatJsonSchema<'a> {
    pub(super) name: &'a str,
    pub(super) strict: bool,
    pub(super) schema: &'a serde_json::Value,
}

/// Request message type that borrows strings from history to avoid cloning.
///
/// The role is the shared typed [`Role`]; its lowercase serde names match the
/// Chat Completions wire values exactly.
#[derive(Serialize, Clone, Debug)]
pub(super) struct ChatMessage<'a> {
    pub(super) role: Role,
    pub(super) content: Option<ChatContent<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reasoning_content: Option<Cow<'a, str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tool_calls: Option<Vec<ChatToolCallRef<'a>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tool_call_id: Option<Cow<'a, str>>,
}

impl ChatMessage<'_> {
    /// This message's content as plain text, or `None` when the message is
    /// content-free or carries a multi-part content array.
    ///
    /// Test-only: assertions compare the text of text-only messages, which the
    /// request builder itself never needs to read back.
    #[cfg(test)]
    pub(super) fn text(&self) -> Option<&str> {
        match self.content.as_ref()? {
            ChatContent::Text(text) => Some(text),
            ChatContent::Parts(_) => None,
        }
    }
}

/// A request message's content: a bare string for every text-only message, or
/// an array of parts when the message carries images.
///
/// Serialization is untagged so a plain string stays byte-identical to the
/// pre-image wire format and existing request snapshots do not change.
#[derive(Serialize, Clone, Debug)]
#[serde(untagged)]
pub(super) enum ChatContent<'a> {
    Text(Cow<'a, str>),
    Parts(Vec<ChatContentPart<'a>>),
}

impl<'a> ChatContent<'a> {
    /// A content array that leads with `text` and then carries each image.
    ///
    /// The leading text names the tool result the images came from, so the
    /// message still reads as that tool's output even though a tool-role
    /// message cannot carry images.
    pub(super) fn with_images(text: &'a str, images: &[ImagePart]) -> Self {
        let mut parts = Vec::with_capacity(images.len() + 1);
        parts.push(ChatContentPart::Text {
            text: Cow::Borrowed(text),
        });
        parts.extend(images.iter().map(|image| ChatContentPart::ImageUrl {
            image_url: ChatImageUrl {
                url: Cow::Owned(image.data_url()),
            },
        }));
        Self::Parts(parts)
    }
}

impl<'a> From<&'a str> for ChatContent<'a> {
    fn from(text: &'a str) -> Self {
        Self::Text(Cow::Borrowed(text))
    }
}

/// One piece of a multi-part message content array.
#[derive(Serialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ChatContentPart<'a> {
    Text { text: Cow<'a, str> },
    ImageUrl { image_url: ChatImageUrl<'a> },
}

/// The `image_url` payload of a [`ChatContentPart::ImageUrl`] part.
#[derive(Serialize, Clone, Debug)]
pub(super) struct ChatImageUrl<'a> {
    /// An inline `data:<media_type>;base64,<data>` URL.
    pub(super) url: Cow<'a, str>,
}

/// Borrowed tool wrapper for request serialization. Borrows the name,
/// description, and schema from [`crate::clients::tools::Tool`] so a request
/// build does not clone any tool schema.
#[derive(Serialize, Debug)]
pub(super) struct ChatTool<'a> {
    #[serde(rename = "type")]
    pub(super) type_: &'static str,
    pub(super) function: ChatFunction<'a>,
}

#[derive(Serialize, Debug)]
pub(super) struct ChatFunction<'a> {
    pub(super) name: &'a str,
    pub(super) description: &'a str,
    pub(super) parameters: &'a serde_json::Value,
}

/// Borrowed tool call type for request serialization.
#[derive(Serialize, Clone, Debug)]
pub(super) struct ChatToolCallRef<'a> {
    pub(super) id: Cow<'a, str>,
    #[serde(rename = "type")]
    pub(super) type_: Cow<'a, str>,
    pub(super) function: ChatFunctionCallRef<'a>,
}

/// Borrowed function call type for request serialization.
#[derive(Serialize, Clone, Debug)]
pub(super) struct ChatFunctionCallRef<'a> {
    pub(super) name: Cow<'a, str>,
    pub(super) arguments: Cow<'a, str>,
}

// =============================================================================
// Chat Completions API Response DTOs (deserialization - owned types)
// =============================================================================

#[derive(Deserialize, Debug)]
pub(super) struct ChatResponse {
    pub(super) id: Option<String>,
    /// The provider's own model identifier, which may differ from the
    /// configured model when a gateway routes an alias.
    pub(super) model: Option<String>,
    pub(super) choices: Vec<ChatChoice>,
    pub(super) usage: Option<ChatUsage>,
}

#[derive(Deserialize, Debug)]
pub(super) struct ChatChoice {
    pub(super) message: ChatResponseMessage,
    pub(super) finish_reason: Option<String>,
}

#[derive(Deserialize, Debug)]
pub(super) struct ChatResponseMessage {
    pub(super) content: Option<String>,
    /// Some OpenAI-compatible gateways emit reasoning text as `reasoning`
    /// rather than `reasoning_content`.
    #[serde(alias = "reasoning")]
    pub(super) reasoning_content: Option<String>,
    pub(super) refusal: Option<String>,
    pub(super) tool_calls: Option<Vec<ChatToolCall>>,
}

/// Owned tool call type for response deserialization.
#[derive(Deserialize, Clone, Debug)]
pub(super) struct ChatToolCall {
    pub(super) id: String,
    pub(super) function: ChatFunctionCall,
}

/// Owned function call type for response deserialization.
#[derive(Deserialize, Clone, Debug)]
pub(super) struct ChatFunctionCall {
    pub(super) name: String,
    pub(super) arguments: String,
}

#[derive(Deserialize, Debug, Default)]
pub(super) struct CompletionTokensDetails {
    pub(super) reasoning_tokens: Option<u64>,
}

#[derive(Deserialize, Debug, Default)]
pub(super) struct PromptTokensDetails {
    pub(super) cached_tokens: Option<u64>,
    pub(super) cache_write_tokens: Option<u64>,
}

#[derive(Deserialize, Debug)]
pub(super) struct ChatUsage {
    pub(super) prompt_tokens: Option<u64>,
    pub(super) completion_tokens: Option<u64>,
    pub(super) total_tokens: Option<u64>,
    pub(super) prompt_tokens_details: Option<PromptTokensDetails>,
    pub(super) completion_tokens_details: Option<CompletionTokensDetails>,
}
