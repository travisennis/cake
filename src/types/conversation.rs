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

// =============================================================================
// Reasoning Content (preserved for API round-tripping)
// =============================================================================

/// A content item within a reasoning output, preserved verbatim for echoing
/// back to the API in multi-turn conversations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningContent {
    #[serde(rename = "type")]
    pub content_type: ReasoningContentKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
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

// =============================================================================
// Conversation Item Enum (for Responses API input/output)
// =============================================================================

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
    },
    Reasoning {
        id: String,
        summary: Vec<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_serialization_roundtrip() {
        let cases = [
            (Role::System, "\"system\""),
            (Role::Developer, "\"developer\""),
            (Role::Assistant, "\"assistant\""),
            (Role::User, "\"user\""),
            (Role::Tool, "\"tool\""),
        ];

        for (role, expected) in cases {
            let json = serde_json::to_string(&role).unwrap();
            assert_eq!(json, expected);
            let deserialized: Role = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, role);
        }
    }

    #[test]
    fn role_deserialization_case_insensitive() {
        let role: Role = serde_json::from_str("\"system\"").unwrap();
        assert_eq!(role, Role::System);
    }

    #[test]
    fn role_equality() {
        assert_eq!(Role::User, Role::User);
        assert_ne!(Role::User, Role::Assistant);
    }

    #[test]
    fn role_clone() {
        let role = Role::Assistant;
        let cloned = role;
        assert_eq!(role, cloned);
    }

    #[test]
    fn role_debug_format() {
        assert_eq!(format!("{:?}", Role::User), "User");
        assert_eq!(format!("{:?}", Role::Assistant), "Assistant");
    }

    #[test]
    fn role_as_str() {
        assert_eq!(Role::System.as_str(), "system");
        assert_eq!(Role::Developer.as_str(), "developer");
        assert_eq!(Role::Assistant.as_str(), "assistant");
        assert_eq!(Role::User.as_str(), "user");
        assert_eq!(Role::Tool.as_str(), "tool");
    }

    #[test]
    fn reasoning_content_kind_serializes_known_values() {
        let json = serde_json::to_value(ReasoningContent {
            content_type: ReasoningContentKind::ReasoningText,
            text: Some("thinking".to_string()),
        })
        .unwrap();

        assert_eq!(json["type"], "reasoning_text");
        assert_eq!(json["text"], "thinking");
    }

    #[test]
    fn reasoning_content_kind_preserves_unknown_values() {
        let content: ReasoningContent = serde_json::from_value(serde_json::json!({
            "type": "provider_specific_reasoning",
            "text": "opaque"
        }))
        .unwrap();

        assert_eq!(
            content.content_type,
            ReasoningContentKind::Unknown("provider_specific_reasoning".to_string())
        );

        let json = serde_json::to_value(content).unwrap();
        assert_eq!(json["type"], "provider_specific_reasoning");
        assert_eq!(json["text"], "opaque");
    }

    #[test]
    fn conversation_item_serialization_roundtrip() {
        let items = vec![
            ConversationItem::Message {
                role: Role::User,
                content: "test".to_string(),
                id: None,
                status: None,
                timestamp: None,
            },
            ConversationItem::FunctionCall {
                id: "fc".to_string(),
                call_id: "call".to_string(),
                name: "tool".to_string(),
                arguments: "{}".to_string(),
                timestamp: None,
            },
            ConversationItem::FunctionCallOutput {
                call_id: "call".to_string(),
                output: "out".to_string(),
                timestamp: None,
            },
            ConversationItem::Reasoning {
                id: "r".to_string(),
                summary: vec!["s".to_string()],
                encrypted_content: None,
                content: None,
                timestamp: None,
            },
        ];

        for item in items {
            let json = serde_json::to_string(&item).unwrap();
            let deserialized: ConversationItem = serde_json::from_str(&json).unwrap();
            assert_eq!(json, serde_json::to_string(&deserialized).unwrap());
        }
    }
}
