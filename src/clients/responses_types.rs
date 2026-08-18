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
use crate::types::{ReasoningContent, ReasoningSummary};

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
    pub(super) summary_type: &'a str,
    pub(super) text: &'a str,
}

#[derive(Clone, Serialize)]
pub(super) struct ProviderConfig {
    pub(super) only: Vec<String>,
}

/// Structured-output configuration for correction turns:
/// `{"format": {"type": "json_schema", ...}}`.
#[derive(Debug, Serialize)]
pub(super) struct TextConfig<'a> {
    pub(super) format: TextFormat<'a>,
}

/// The `json_schema` format payload inside [`TextConfig`].
#[derive(Debug, Serialize)]
pub(super) struct TextFormat<'a> {
    #[serde(rename = "type")]
    pub(super) format_type: &'static str,
    pub(super) name: &'a str,
    pub(super) strict: bool,
    pub(super) schema: &'a serde_json::Value,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) store: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) stream: Option<bool>,
    pub(super) input: Vec<ResponsesApiInputItem<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) instructions: Option<&'a str>,
    pub(super) temperature: Option<f32>,
    pub(super) top_p: Option<f32>,
    pub(super) max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tools: Option<&'a [super::tools::Tool]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) provider: Option<ProviderConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reasoning: Option<ReasoningConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) text: Option<TextConfig<'a>>,
}

#[derive(Deserialize, Debug)]
pub(super) struct ApiResponse {
    pub(super) id: Option<String>,
    pub(super) output: Vec<OutputMessage>,
    pub(super) usage: Option<ApiUsage>,
}

#[derive(Deserialize, Debug)]
pub(super) struct ApiResponseEnvelope {
    #[serde(flatten)]
    pub(super) response: ApiResponse,
    pub(super) status: Option<String>,
    pub(super) incomplete_details: Option<IncompleteDetails>,
}

#[derive(Deserialize, Debug)]
pub(super) struct IncompleteDetails {
    pub(super) reason: Option<String>,
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
    /// Typed summary items on reasoning outputs. The shared type also accepts
    /// legacy provider responses where each item is a plain string.
    pub(super) summary: Option<Vec<ReasoningSummary>>,
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

    #[test]
    fn response_envelope_retains_optional_termination_fields() {
        let envelope: ApiResponseEnvelope = serde_json::from_value(serde_json::json!({
            "id": "resp-123",
            "status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"},
            "output": []
        }))
        .unwrap();

        assert_eq!(envelope.status.as_deref(), Some("incomplete"));
        assert_eq!(
            envelope.incomplete_details.unwrap().reason.as_deref(),
            Some("max_output_tokens")
        );
        assert_eq!(envelope.response.id.as_deref(), Some("resp-123"));
    }

    #[test]
    fn response_envelope_tolerates_unknown_and_missing_termination_fields() {
        let envelope: ApiResponseEnvelope = serde_json::from_value(serde_json::json!({
            "id": "resp-123",
            "output": [],
            "future_metadata": {"value": true}
        }))
        .unwrap();

        assert!(envelope.status.is_none());
        assert!(envelope.incomplete_details.is_none());
    }

    #[test]
    fn response_reasoning_summary_accepts_objects_and_legacy_strings() {
        let envelope: ApiResponseEnvelope = serde_json::from_value(serde_json::json!({
            "id": "resp-123",
            "output": [{
                "type": "reasoning",
                "id": "reasoning-123",
                "summary": [
                    {"type": "summary_text", "text": "typed summary"},
                    "legacy summary"
                ]
            }]
        }))
        .unwrap();

        let summaries = envelope.response.output[0].summary.as_ref().unwrap();
        assert_eq!(summaries[0].summary_type, "summary_text");
        assert_eq!(summaries[0].text, "typed summary");
        assert_eq!(
            summaries[1],
            ReasoningSummary::summary_text("legacy summary")
        );
    }
}
