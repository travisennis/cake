use tracing::{debug, trace, warn};

use crate::config::model::ResolvedModelConfig;

use crate::clients::agent::TurnResult;
use crate::clients::backend::{FinalOutputConstraint, ResponseDecodeError};
use crate::clients::provider_strategy::ProviderStrategy;
use crate::clients::responses_types::{
    ApiResponse, ApiResponseEnvelope, ApiUsage, OutputMessage, ReasoningConfig, Request,
    ResponsesApiInputItem, ResponsesMessageContent, ResponsesReasoningSummary, TextConfig,
    TextFormat,
};
use crate::clients::retry::RequestOverrides;
use crate::clients::tools::Tool;
use crate::session_telemetry::{ProviderTermination, TerminationClassification};
use crate::types::{
    ConversationItem, InputTokensDetails, OutputTokensDetails, ReasoningContentKind,
    ReasoningSummary, Role, Usage,
};

// =============================================================================
// Responses API Backend
// =============================================================================

/// Send a request to the Responses API, returning the raw HTTP response.
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

/// Build the exact provider-transformed JSON body sent to the Responses API.
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
    let strategy = ProviderStrategy::from_config(config);
    let provider_config = strategy.responses_provider_config();

    let max_output_tokens = overrides
        .max_output_tokens
        .or(config.model_config.max_output_tokens);
    let reasoning_max_tokens = overrides
        .reasoning_max_tokens
        .or(config.model_config.reasoning_max_tokens);

    let reasoning_effort = config.model_config.reasoning_effort;
    let reasoning_summary = config.model_config.reasoning_summary.clone();
    let reasoning = (reasoning_effort.is_some()
        || reasoning_summary.is_some()
        || reasoning_max_tokens.is_some())
    .then_some(ReasoningConfig {
        effort: reasoning_effort,
        summary: reasoning_summary,
        max_tokens: reasoning_max_tokens,
    });

    let (instructions, non_system_history) = extract_instructions(history)?;

    let prompt = Request {
        model: &config.model_config.model,
        input: build_input(non_system_history),
        instructions,
        temperature: config.model_config.temperature,
        top_p: config.model_config.top_p,
        max_output_tokens,
        tools: if tools.is_empty() { None } else { Some(tools) },
        tool_choice: if tools.is_empty() {
            None
        } else {
            Some("auto".to_string())
        },
        provider: provider_config,
        reasoning,
        text: constraint.map(|constraint| TextConfig {
            format: TextFormat {
                format_type: "json_schema",
                name: constraint.name,
                strict: true,
                schema: constraint.schema,
            },
        }),
    };

    serde_json::to_vec(&prompt).map_err(Into::into)
}

/// Send one already-built Responses API JSON request.
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
        "{}/responses",
        config.model_config.base_url.trim_end_matches('/')
    );
    debug!(target: "cake", "{url}");
    if tracing::enabled!(tracing::Level::TRACE) {
        let prompt_json = String::from_utf8_lossy(&request);
        trace!(target: "cake", "{prompt_json}");
    }

    let request = strategy.apply_headers(
        client
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(request),
    );
    let response = crate::auth::apply_request_auth(request, config)?
        .send()
        .await?;

    Ok(response)
}

/// Read the response body and decode the Responses API envelope, attaching a
/// bounded preview of the raw body to the error when the 2xx body is not the
/// expected JSON shape, so an opaque provider or proxy body is diagnosable
/// from the error text.
async fn read_response_envelope(
    response: reqwest::Response,
) -> anyhow::Result<ApiResponseEnvelope> {
    let bytes = response.bytes().await?;
    serde_json::from_slice::<ApiResponseEnvelope>(&bytes)
        .map_err(|error| ResponseDecodeError::new("Responses API", &bytes, error).into())
}

/// Parse an HTTP response from the Responses API into a `TurnResult`.
///
/// # Errors
///
/// Returns an error if the response body cannot be read or deserialized. A
/// deserialization failure carries a bounded preview of the raw body so an
/// opaque provider or proxy 2xx (empty body, HTML error page, wrong envelope)
/// is diagnosable from the error text instead of reqwest's generic
/// "error decoding response body".
pub(super) async fn parse_response(response: reqwest::Response) -> anyhow::Result<TurnResult> {
    let envelope = read_response_envelope(response).await?;
    let api_response = envelope.response;
    let termination = responses_termination(
        envelope.status.as_deref(),
        api_response
            .output
            .iter()
            .filter_map(|output| output.status.as_deref()),
        envelope
            .incomplete_details
            .as_ref()
            .and_then(|details| details.reason.as_deref()),
        response_contains_refusal(&api_response),
    );
    trace!(target: "cake", "{api_response:?}");

    if api_response.id.is_none() {
        warn!(
            target: "cake",
            "Responses API response is missing 'id' field; this may indicate a provider incompatibility"
        );
    }

    let usage = api_response
        .usage
        .as_ref()
        .map(|u| map_usage(u, &api_response));
    let items = parse_output_items(&api_response)?;

    Ok(TurnResult {
        items,
        usage,
        termination,
        provider_request_id: api_response.id,
    })
}

fn responses_termination<'a>(
    envelope_status: Option<&str>,
    item_statuses: impl IntoIterator<Item = &'a str>,
    reason: Option<&str>,
    contains_refusal: bool,
) -> Option<ProviderTermination> {
    let item_statuses = item_statuses.into_iter().collect::<Vec<_>>();
    if envelope_status.is_none()
        && item_statuses.is_empty()
        && reason.is_none()
        && !contains_refusal
    {
        return None;
    }
    let statuses = envelope_status
        .into_iter()
        .chain(item_statuses.iter().copied());
    let classification = classify_response_termination(statuses, reason, contains_refusal);
    Some(ProviderTermination {
        classification,
        provider_status: envelope_status
            .or_else(|| item_statuses.first().copied())
            .map(str::to_string),
        provider_reason: reason.map(str::to_string),
    })
}

fn classify_response_termination<'a>(
    statuses: impl IntoIterator<Item = &'a str>,
    reason: Option<&str>,
    contains_refusal: bool,
) -> TerminationClassification {
    let statuses = statuses.into_iter().collect::<Vec<_>>();
    let has_status = |expected: &[&str]| statuses.iter().any(|status| expected.contains(status));

    if reason == Some("content_filter") || has_status(&["content_filter"]) {
        TerminationClassification::ContentFilter
    } else if contains_refusal
        || matches!(reason, Some("refusal" | "refused" | "failed" | "cancelled"))
        || has_status(&["refusal", "refused", "failed", "cancelled"])
    {
        TerminationClassification::Failed
    } else if matches!(reason, Some("max_output_tokens" | "max_tokens" | "length"))
        || has_status(&["max_output_tokens", "max_tokens", "length"])
    {
        TerminationClassification::TokenLimit
    } else if has_status(&["incomplete", "in_progress", "queued"]) {
        TerminationClassification::Incomplete
    } else if has_status(&["completed"]) {
        TerminationClassification::Completed
    } else {
        TerminationClassification::Unknown
    }
}

fn response_contains_refusal(api_response: &ApiResponse) -> bool {
    api_response.output.iter().any(|output| {
        output.msg_type == "refusal"
            || output
                .content
                .as_ref()
                .is_some_and(|content| content.iter().any(|item| item.content_type == "refusal"))
    })
}

/// Map API-level usage to the canonical `Usage` type.
fn map_usage(api_usage: &ApiUsage, api_response: &ApiResponse) -> Usage {
    let response_id = api_response.id.as_deref().unwrap_or("<missing id>");

    if api_usage.input_tokens.is_none() {
        warn!(
            target: "cake",
            response_id = response_id,
            field = "input_tokens",
            "Responses API usage missing field, defaulting to 0"
        );
    }
    if api_usage.output_tokens.is_none() {
        warn!(
            target: "cake",
            response_id = response_id,
            field = "output_tokens",
            "Responses API usage missing field, defaulting to 0"
        );
    }
    if api_usage.total_tokens.is_none() {
        warn!(
            target: "cake",
            response_id = response_id,
            field = "total_tokens",
            "Responses API usage missing field, defaulting to 0"
        );
    }

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
    history.iter().map(ResponsesApiInputItem::from).collect()
}

impl<'a> From<&'a ConversationItem> for ResponsesApiInputItem<'a> {
    fn from(item: &'a ConversationItem) -> Self {
        match item {
            ConversationItem::Message {
                role,
                content,
                id,
                status,
                ..
            } => {
                let content_type = if matches!(role, Role::Assistant) {
                    "output_text"
                } else {
                    "input_text"
                };
                // Provider quirk: the Responses API requires an `annotations`
                // field (even if empty) on assistant `output_text` content
                // blocks. Non-assistant `input_text` blocks must omit it.
                // Removing the empty array would send malformed assistant
                // turns to the provider.
                let annotations =
                    matches!(role, Role::Assistant).then(Vec::<serde_json::Value>::new);

                Self::Message {
                    role: role.as_str(),
                    content: vec![ResponsesMessageContent {
                        content_type,
                        text: content,
                        annotations,
                    }],
                    id: id.as_deref(),
                    status: status.as_deref(),
                }
            },
            ConversationItem::FunctionCall {
                id,
                call_id,
                name,
                arguments,
                ..
            } => Self::FunctionCall {
                id,
                call_id,
                name,
                arguments,
            },
            ConversationItem::FunctionCallOutput {
                call_id, output, ..
            } => Self::FunctionCallOutput { call_id, output },
            ConversationItem::Reasoning {
                id,
                summary,
                encrypted_content,
                content,
                ..
            } => {
                // When `summary` is `None`, we produce an empty array (`"summary": []`)
                // rather than omitting the field. This maps the domain type's `Option`
                // into the API DTO's non-optional `Vec`. The Responses API accepts
                // `"summary": []` equivalently to an absent field — it is treated as
                // "no summaries to echo". This behavior predates `summary` becoming
                // optional and has been in production use without issues.
                Self::Reasoning {
                    id,
                    summary: summary
                        .as_deref()
                        .unwrap_or_default()
                        .iter()
                        .map(|text| ResponsesReasoningSummary {
                            summary_type: &text.summary_type,
                            text: &text.text,
                        })
                        .collect(),
                    encrypted_content: encrypted_content.as_deref(),
                    content: content.as_deref(),
                }
            },
        }
    }
}

/// Parse the output items from an API response into `ConversationItem` values.
///
/// # Errors
///
/// Returns an error if a function call item is missing required fields.
fn parse_output_items(api_response: &ApiResponse) -> anyhow::Result<Vec<ConversationItem>> {
    let mut items = Vec::new();
    let mut unknown_output_types = Vec::new();
    let response_id = api_response.id.as_deref().unwrap_or("<missing id>");

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
                                        .map(ReasoningSummary::summary_text)
                                        .collect()
                                })
                                .unwrap_or_default()
                        });

                    let content = output.content.as_ref().map(|c| {
                        c.iter()
                            .map(|item| crate::types::ReasoningContent {
                                content_type: item.content_type.clone().into(),
                                text: item.text.clone(),
                            })
                            .collect()
                    });

                    let timestamp = chrono::Utc::now();
                    items.push(ConversationItem::Reasoning {
                        id: id.clone(),
                        summary: Some(summary),
                        encrypted_content: output.encrypted_content.clone(),
                        content,
                        timestamp: Some(timestamp),
                    });
                } else {
                    warn!(
                        target: "cake",
                        response_id = response_id,
                        output_index = index,
                        "Skipping Responses API reasoning output with missing 'id'"
                    );
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
                    .unwrap_or_else(|| {
                        warn!(
                            target: "cake",
                            response_id = response_id,
                            output_index = index,
                            output_id = output.id.as_deref(),
                            "Responses API message output has no 'output_text' content block; returning empty text"
                        );
                        String::new()
                    });

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
                    response_id,
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
    let (Some(id), Some(call_id), Some(name)) = (
        output.id.as_ref(),
        output.call_id.as_ref(),
        output.name.as_ref(),
    ) else {
        return Err(malformed_function_call_error(api_response, output, index));
    };

    if id.is_empty() || call_id.is_empty() || name.is_empty() {
        return Err(malformed_function_call_error(api_response, output, index));
    }

    // Normalize missing, empty, or whitespace-only arguments to "{}" — some
    // providers send none or an empty string for parameterless tool calls.
    // Established toolbox parse_arguments already does the same normalization.
    let arguments = output
        .arguments
        .as_deref()
        .filter(|a| !a.trim().is_empty())
        .unwrap_or("{}")
        .to_string();

    let timestamp = chrono::Utc::now();
    Ok(ConversationItem::FunctionCall {
        id: id.clone(),
        call_id: call_id.clone(),
        name: name.clone(),
        arguments,
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
#[path = "responses_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "responses_response_parsing_tests.rs"]
mod response_parsing_tests;
