//! Conversation domain types.
//!
//! These are backend-agnostic types describing a conversation between the
//! user, the assistant, tools, and the model's reasoning output.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Represents the role of a message sender in a conversation.
///
/// Roles distinguish between different participants in the conversation:
/// system prompts, developer-provided context, assistant responses, user inputs,
/// and tool outputs.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Represents a system role.
    System,
    /// Represents developer-provided instructions or mutable context.
    Developer,
    /// Represents an assistant.
    Assistant,
    /// Represents a user.
    User,
    /// Represents a tool result.
    Tool,
}

impl Role {
    /// Returns the string representation of the role.
    pub const fn as_str(&self) -> &str {
        match self {
            Self::System => "system",
            Self::Developer => "developer",
            Self::Assistant => "assistant",
            Self::User => "user",
            Self::Tool => "tool",
        }
    }
}

/// A content item within a reasoning output, preserved verbatim for echoing
/// back to the API in multi-turn conversations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningContent {
    #[serde(rename = "type")]
    pub content_type: ReasoningContentKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// A typed summary item from a reasoning output.
///
/// `OpenAI`'s Responses API represents reasoning summaries as objects, while
/// some compatible providers return an array of plain strings. Deserialization
/// accepts both forms and normalizes legacy strings to `summary_text` so the
/// internal conversation representation remains typed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReasoningSummary {
    #[serde(rename = "type")]
    pub summary_type: String,
    pub text: String,
}

impl ReasoningSummary {
    pub fn summary_text(text: impl Into<String>) -> Self {
        Self {
            summary_type: "summary_text".to_string(),
            text: text.into(),
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ReasoningSummaryInput {
    Object {
        #[serde(rename = "type")]
        summary_type: String,
        text: String,
    },
    LegacyText(String),
}

impl<'de> Deserialize<'de> for ReasoningSummary {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match ReasoningSummaryInput::deserialize(deserializer)? {
            ReasoningSummaryInput::Object { summary_type, text } => Ok(Self { summary_type, text }),
            ReasoningSummaryInput::LegacyText(text) => Ok(Self::summary_text(text)),
        }
    }
}

/// Protocol-defined kind for reasoning content items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReasoningContentKind {
    ReasoningText,
    SummaryText,
    Unknown(String),
}

impl ReasoningContentKind {
    pub const fn as_str(&self) -> &str {
        match self {
            Self::ReasoningText => "reasoning_text",
            Self::SummaryText => "summary_text",
            Self::Unknown(value) => value.as_str(),
        }
    }
}

impl From<&str> for ReasoningContentKind {
    fn from(value: &str) -> Self {
        match value {
            "reasoning_text" => Self::ReasoningText,
            "summary_text" => Self::SummaryText,
            other => Self::Unknown(other.to_string()),
        }
    }
}

impl From<String> for ReasoningContentKind {
    fn from(value: String) -> Self {
        match value.as_str() {
            "reasoning_text" => Self::ReasoningText,
            "summary_text" => Self::SummaryText,
            _ => Self::Unknown(value),
        }
    }
}

impl Serialize for ReasoningContentKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ReasoningContentKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Self::from)
    }
}

/// One image attached to a message or a function-call output.
///
/// In memory, the bytes are carried as standard base64 with no data-URL prefix
/// so the internal representation stays backend-agnostic: each backend builds
/// the shape its API expects. On disk, a record carries either `data_base64`
/// (legacy inline shape) or `content_id` (a digest into the session's blob
/// store), never both. Empty on every text-only item, so records written before
/// images existed load unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImagePart {
    /// The image's MIME type, for example `image/png`.
    pub media_type: String,
    /// The image bytes encoded as standard base64.
    ///
    /// Empty when the persisted record refers to a blob instead.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub data_base64: String,
    /// The lowercase SHA-256 digest of the decoded image bytes.
    ///
    /// Hydrated in-memory images may carry this alongside `data_base64` so the
    /// reference survives another persistence pass. Stream output strips it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_id: Option<String>,
}

impl ImagePart {
    /// This image as an inline data URL, `data:<media_type>;base64,<data>`.
    ///
    /// Both API backends carry an image to the provider as a data URL, so the
    /// exact encoding lives with the type instead of in each backend's
    /// translation.
    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.media_type, self.data_base64)
    }
}

/// Represents a single item in the conversation history, mapping directly to
/// the Responses API input/output array format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationItem {
    Message {
        role: Role,
        content: String,
        /// Assistant message ID (required for assistant messages in input)
        id: Option<String>,
        /// "completed" or "incomplete" (required for assistant messages in input)
        status: Option<String>,
        /// Timestamp when this item was created
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<DateTime<Utc>>,
        /// Inline images attached to this message. Empty for text-only
        /// messages; serialization skips the field when empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImagePart>,
    },
    FunctionCall {
        id: String,
        call_id: String,
        name: String,
        arguments: String,
        /// Timestamp when this item was created
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<DateTime<Utc>>,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
        /// Timestamp when this item was created
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<DateTime<Utc>>,
        /// Inline images attached to this tool result. Empty for text-only
        /// outputs; serialization skips the field when empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImagePart>,
    },
    Reasoning {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<Vec<ReasoningSummary>>,
        /// Opaque encrypted reasoning content that must be echoed back to the
        /// API for multi-turn conversations with reasoning models.
        #[serde(skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<String>,
        /// Original content array from the API response (e.g., `reasoning_text` items).
        /// Must be echoed back so the router can reconstruct `reasoning_content`
        /// for Chat Completions providers like Moonshot AI.
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<Vec<ReasoningContent>>,
        /// Timestamp when this item was created
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<DateTime<Utc>>,
    },
}

impl ConversationItem {
    /// Number of inline images attached to this item.
    ///
    /// Images can ride on a message or a tool result, so callers that need a
    /// history-wide count must sum this across every item.
    pub const fn image_count(&self) -> usize {
        match self {
            Self::Message { images, .. } | Self::FunctionCallOutput { images, .. } => images.len(),
            Self::FunctionCall { .. } | Self::Reasoning { .. } => 0,
        }
    }
}
