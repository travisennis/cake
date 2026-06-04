//! Responses API request and response DTOs.
//!
//! All types in this module are `pub(super)` so they remain internal to the
//! `clients` module. They model the JSON wire format used by the `OpenAI`
//! Responses API and OpenAI-compatible providers (Fireworks, Moonshot AI,
//! Together, etc.).
//!
//! Conversion between the domain `ConversationItem` and the API
//! `ResponsesApiInputItem` lives in [`crate::clients::responses`].

use serde::{Deserialize, Serialize};

use crate::config::ReasoningEffort;
use crate::types::ReasoningContent;

/// Typed Responses API input item serialized into the request `input` array.
///
/// **Construction boundary:** Instances are built only via the
/// [`From<&ConversationItem>`] impl in [`crate::clients::responses`], never by
/// hand. This keeps the API wire shape owned in one place.
///
/// [`From<&ConversationItem>`]: core::convert::From
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ResponsesApiInputItem<'a> {
    Message {
        role: &'a str,
        content: Vec<ResponsesMessageContent<'a>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<&'a str>,
    },
    FunctionCall {
        id: &'a str,
        call_id: &'a str,
        name: &'a str,
        arguments: &'a str,
    },
    FunctionCallOutput {
        call_id: &'a str,
        output: &'a str,
    },
    Reasoning {
        id: &'a str,
        summary: Vec<ResponsesReasoningSummary<'a>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<&'a [ReasoningContent]>,
    },
}

/// Content block used by Responses API message input.
///
/// **Construction boundary:** Instances are built only through the
/// [`From<&ConversationItem>`] impl on [`ResponsesApiInputItem`]. Do not
/// construct by hand.
///
/// [`From<&ConversationItem>`]: core::convert::From
#[derive(Debug, Serialize)]
pub(super) struct ResponsesMessageContent<'a> {
    #[serde(rename = "type")]
    pub(super) content_type: &'static str,
    pub(super) text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) annotations: Option<Vec<serde_json::Value>>,
}

/// Summary block used by Responses API reasoning input.
///
/// **Construction boundary:** Instances are built only through the
/// [`From<&ConversationItem>`] impl on [`ResponsesApiInputItem`]. Do not
/// construct by hand.
///
/// [`From<&ConversationItem>`]: core::convert::From
#[derive(Debug, Serialize)]
pub(super) struct ResponsesReasoningSummary<'a> {
    #[serde(rename = "type")]
    pub(super) summary_type: &'static str,
    pub(super) text: &'a str,
}

#[derive(Clone, Serialize)]
pub(super) struct ProviderConfig {
    pub(super) only: Vec<String>,
}

#[derive(Clone, Serialize)]
pub(super) struct ReasoningConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) effort: Option<ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) max_tokens: Option<u32>,
}

#[derive(Serialize)]
pub(super) struct Request<'a> {
    pub(super) model: &'a str,
    pub(super) input: Vec<ResponsesApiInputItem<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) instructions: Option<&'a str>,
    pub(super) temperature: Option<f32>,
    pub(super) top_p: Option<f32>,
    pub(super) max_output_tokens: Option<u32>,
    pub(super) tools: Option<&'a [super::tools::Tool]>,
    pub(super) tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) provider: Option<ProviderConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reasoning: Option<ReasoningConfig>,
}

#[derive(Deserialize, Debug)]
pub(super) struct ApiResponse {
    pub(super) id: Option<String>,
    pub(super) output: Vec<OutputMessage>,
    pub(super) usage: Option<ApiUsage>,
}

#[derive(Deserialize, Debug, Clone)]
pub(super) struct OutputMessage {
    #[serde(rename = "type")]
    pub(super) msg_type: String,
    pub(super) id: Option<String>,
    pub(super) call_id: Option<String>,
    pub(super) name: Option<String>,
    pub(super) arguments: Option<String>,
    pub(super) status: Option<String>,
    pub(super) content: Option<Vec<OutputContent>>,
    /// Opaque encrypted reasoning content returned by reasoning models.
    pub(super) encrypted_content: Option<String>,
    /// Top-level summary strings on reasoning items (alternative to content-based summaries).
    pub(super) summary: Option<Vec<String>>,
}

#[derive(Deserialize, Debug, Clone)]
pub(super) struct OutputContent {
    #[serde(rename = "type")]
    pub(super) content_type: String,
    pub(super) text: Option<String>,
}

/// Internal usage struct for API response deserialization (with optional fields).
#[derive(Deserialize, Debug, Clone, Default)]
pub(super) struct ApiUsage {
    pub(super) input_tokens: Option<u64>,
    pub(super) input_tokens_details: Option<ApiInputTokensDetails>,
    pub(super) output_tokens: Option<u64>,
    pub(super) output_tokens_details: Option<ApiOutputTokensDetails>,
    pub(super) total_tokens: Option<u64>,
}

/// Internal input tokens details for API response deserialization.
#[derive(Deserialize, Debug, Clone, Default)]
pub(super) struct ApiInputTokensDetails {
    pub(super) cached_tokens: Option<u64>,
}

/// Internal output tokens details for API response deserialization.
#[derive(Deserialize, Debug, Clone, Default)]
pub(super) struct ApiOutputTokensDetails {
    pub(super) reasoning_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_config_serialization() {
        let config = ProviderConfig {
            only: vec!["Fireworks".to_string(), "Moonshot AI".to_string()],
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("\"only\":["));
        assert!(json.contains("\"Fireworks\""));
        assert!(json.contains("\"Moonshot AI\""));
    }

    #[test]
    fn provider_config_single_provider() {
        let config = ProviderConfig {
            only: vec!["OpenAI".to_string()],
        };
        let json = serde_json::to_string(&config).unwrap();
        let expected = r#"{"only":["OpenAI"]}"#;
        assert_eq!(json, expected);
    }
}
