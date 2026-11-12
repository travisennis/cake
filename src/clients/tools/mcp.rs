//! Registry adapter for tools discovered from an MCP server.
//!
//! The protocol client lives in `clients::mcp`; this module turns one
//! [`McpTool`] into a [`ToolEntry`] and maps the server's content blocks into
//! Cake's model-visible tool result.

use std::sync::Arc;

use base64::Engine as _;
use serde_json::Value;

use crate::clients::mcp::{McpClient, McpTool};
use crate::clients::tools::{
    Tool, ToolContext, ToolEntry, ToolError, ToolResult, push_output_truncation_event_if,
    repair_json_args,
};
use crate::types::ImagePart;

/// Cap on text returned to the model, matching the toolbox and Bash inline
/// limits.
const MAX_OUTPUT_BYTES: usize = 50_000;

/// MIME types providers accept for inline images.
const SUPPORTED_IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];

/// Maximum inline images attached from one MCP tool result.
const MAX_MCP_IMAGES: usize = 8;

/// Hard per-image bound when the configured image limit is unlimited. MCP
/// servers are trusted, but one runaway result must not exhaust the process.
const MAX_MCP_IMAGE_BYTES: usize = 32 * 1024 * 1024;

/// Build a registry entry for one MCP tool.
pub(in crate::clients) fn mcp_tool_entry(
    server: &str,
    tool: &McpTool,
    client: Arc<McpClient>,
    supports_images: bool,
) -> ToolEntry {
    let definition = Tool {
        type_: "function".to_string(),
        name: tool.registered_name.clone(),
        description: tool.description.clone(),
        parameters: tool.input_schema.clone(),
    };
    let server = server.to_string();
    let tool_name = tool.name.clone();
    let registered_name = tool.registered_name.clone();
    // MCP arguments are JSON. Apply the same conservative repair pass as the
    // other external-tool protocols before parsing.
    ToolEntry::new(definition, move |context, _call_id, arguments| {
        let client = Arc::clone(&client);
        let server = server.clone();
        let tool_name = tool_name.clone();
        let registered_name = registered_name.clone();
        Box::pin(async move {
            execute_mcp_tool(
                &client,
                &server,
                &tool_name,
                &registered_name,
                supports_images,
                &context,
                &arguments,
            )
            .await
        })
    })
    .repairs_arguments()
}

/// Execute one MCP tool call.
async fn execute_mcp_tool(
    client: &McpClient,
    server: &str,
    tool_name: &str,
    registered_name: &str,
    supports_images: bool,
    context: &ToolContext,
    arguments: &str,
) -> Result<ToolResult, ToolError> {
    let arguments = parse_arguments(registered_name, arguments)?;
    let value = client
        .call_tool(tool_name, arguments)
        .await
        .map_err(|error| {
            ToolError::new(format!(
                "MCP tool '{registered_name}' from server '{server}' failed: {error}"
            ))
        })?;
    let result = McpCallResult::parse(&value).map_err(|error| {
        ToolError::new(format!(
            "MCP tool '{registered_name}' from server '{server}' returned an invalid result: {error}"
        ))
    })?;
    let rendered = render_content(
        &result,
        supports_images,
        context.limits.read_image_max_bytes,
    );
    if result.is_error {
        return Err(ToolError::new(rendered.output));
    }

    let mut compensation_events = Vec::new();
    push_output_truncation_event_if(
        &mut compensation_events,
        registered_name,
        rendered.truncated,
    );
    Ok(ToolResult {
        output: rendered.output,
        images: rendered.images,
        compensation_events,
        permission_denials: Vec::new(),
    })
}

/// Parse tool arguments into a JSON object. An empty argument string is the
/// parameterless form some models emit.
fn parse_arguments(registered_name: &str, arguments: &str) -> Result<Value, ToolError> {
    if arguments.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    let value: Value = serde_json::from_str(arguments)
        .or_else(|_| serde_json::from_str(&repair_json_args(arguments)))
        .map_err(|error| ToolError::new(format!("Invalid {registered_name} arguments: {error}")))?;
    if !value.is_object() {
        return Err(ToolError::new(format!(
            "Invalid {registered_name} arguments: expected a JSON object"
        )));
    }
    Ok(value)
}

/// The fields of a `tools/call` result this client consumes.
#[derive(Debug)]
struct McpCallResult {
    content: Vec<Value>,
    structured_content: Option<Value>,
    is_error: bool,
}

impl McpCallResult {
    fn parse(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "result is not a JSON object".to_string())?;
        let content = match object.get("content") {
            None => Vec::new(),
            Some(Value::Array(items)) => items.clone(),
            Some(_) => return Err("content is not an array".to_string()),
        };
        Ok(Self {
            content,
            structured_content: object.get("structuredContent").cloned(),
            is_error: object
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

/// Model-visible output plus any inline images.
struct RenderedContent {
    output: String,
    images: Vec<ImagePart>,
    truncated: bool,
}

/// Convert every MCP content block into text and inline images.
fn render_content(
    result: &McpCallResult,
    supports_images: bool,
    image_limit: Option<usize>,
) -> RenderedContent {
    let mut parts = Vec::new();
    let mut images = Vec::new();
    for block in &result.content {
        render_block(block, supports_images, image_limit, &mut parts, &mut images);
    }

    if parts.iter().all(|part| part.trim().is_empty())
        && let Some(structured) = &result.structured_content
    {
        parts.push(pretty_json(structured));
    }
    let output = if parts.is_empty() {
        "(no output)".to_string()
    } else {
        parts.join("\n")
    };
    let (output, truncated) = truncate(output);
    RenderedContent {
        output,
        images,
        truncated,
    }
}

/// Convert one content block.
fn render_block(
    block: &Value,
    supports_images: bool,
    image_limit: Option<usize>,
    parts: &mut Vec<String>,
    images: &mut Vec<ImagePart>,
) {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => {
            if let Some(text) = block.get("text").and_then(Value::as_str) {
                parts.push(text.to_string());
            }
        },
        Some("image") => render_image(block, supports_images, image_limit, parts, images),
        Some("audio") => {
            let mime = block
                .get("mimeType")
                .and_then(Value::as_str)
                .unwrap_or("audio");
            let size = encoded_size(block.get("data").and_then(Value::as_str));
            parts.push(format!("[audio content: {mime}, {size} bytes]"));
        },
        Some("resource") => render_resource(block, parts),
        Some("resource_link") => {
            let name = block
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("resource");
            let uri = block.get("uri").and_then(Value::as_str).unwrap_or("");
            parts.push(format!("[resource link: {name} <{uri}>]"));
        },
        Some(other) => parts.push(format!("[{other} content: {}]", compact_json(block))),
        None => parts.push(compact_json(block)),
    }
}

/// Convert one image block when the model supports images and the payload is
/// within the configured byte cap.
fn render_image(
    block: &Value,
    supports_images: bool,
    image_limit: Option<usize>,
    parts: &mut Vec<String>,
    images: &mut Vec<ImagePart>,
) {
    if !supports_images {
        parts
            .push("[image content omitted: the selected model does not accept images]".to_string());
        return;
    }
    if images.len() >= MAX_MCP_IMAGES {
        parts.push(format!(
            "[additional image content omitted: at most {MAX_MCP_IMAGES} images per result]"
        ));
        return;
    }
    let media_type = block
        .get("mimeType")
        .and_then(Value::as_str)
        .unwrap_or("image/png");
    if !SUPPORTED_IMAGE_TYPES.contains(&media_type) {
        parts.push(format!(
            "[image content omitted: unsupported media type '{media_type}']"
        ));
        return;
    }
    let Some(data) = block.get("data").and_then(Value::as_str) else {
        parts.push("[image content omitted: server sent no base64 data]".to_string());
        return;
    };
    let size = encoded_size(Some(data));
    let limit = image_limit.unwrap_or(MAX_MCP_IMAGE_BYTES);
    if size > limit {
        parts.push(format!(
            "[image content omitted: {size} bytes exceeds the image limit of {limit} bytes]"
        ));
        return;
    }
    if base64::engine::general_purpose::STANDARD
        .decode(data)
        .is_err()
    {
        parts.push("[image content omitted: server sent invalid base64 data]".to_string());
        return;
    }
    parts.push(format!("[image: {media_type}, {size} bytes]"));
    images.push(ImagePart {
        media_type: media_type.to_string(),
        data_base64: data.to_string(),
        content_id: None,
    });
}

/// Convert one embedded resource block.
fn render_resource(block: &Value, parts: &mut Vec<String>) {
    let Some(resource) = block.get("resource") else {
        parts.push("[resource content]".to_string());
        return;
    };
    let uri = resource.get("uri").and_then(Value::as_str).unwrap_or("");
    if let Some(text) = resource.get("text").and_then(Value::as_str) {
        parts.push(text.to_string());
        return;
    }
    if let Some(blob) = resource.get("blob").and_then(Value::as_str) {
        let mime = resource
            .get("mimeType")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        parts.push(format!(
            "[resource {uri}: {mime}, {} bytes, base64-encoded]",
            encoded_size(Some(blob))
        ));
        return;
    }
    parts.push(format!("[resource {uri}]"));
}

/// Approximate decoded size of a base64 string without allocating.
const fn encoded_size(data: Option<&str>) -> usize {
    match data {
        Some(data) => data.len().saturating_mul(3) / 4,
        None => 0,
    }
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unencodable>".to_string())
}

fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| compact_json(value))
}

/// Truncate oversized text at a character boundary with an explicit marker.
fn truncate(mut output: String) -> (String, bool) {
    if output.len() <= MAX_OUTPUT_BYTES {
        return (output, false);
    }
    let mut cut = MAX_OUTPUT_BYTES;
    while !output.is_char_boundary(cut) {
        cut -= 1;
    }
    output.truncate(cut);
    output.push_str("\n[... mcp output truncated at ");
    output.push_str(&MAX_OUTPUT_BYTES.to_string());
    output.push_str(" bytes ...]");
    (output, true)
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod mcp_tests;
