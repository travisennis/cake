//! Model Context Protocol client over stdio.
//!
//! This module owns the process, the JSON-RPC transport, the dual-era
//! handshake, and tool discovery. It does not know about Cake's tool registry;
//! `clients::tools::mcp` adapts one [`McpTool`] into a registry entry.
//!
//! # Protocol eras
//!
//! A modern server (`2026-07-28` and later) carries the protocol version,
//! client identity, and client capabilities in `_meta` on every request. A
//! legacy server uses an `initialize` request followed by a
//! `notifications/initialized` notification. The stdio binding requires a
//! client that supports both to probe `server/discover` first and to fall back
//! to `initialize` only when the probe returns a non-modern error or times out.
//!
//! Servers are trusted executables configured by the user. They run outside
//! the Bash sandbox, with Cake's ambient authority.

mod client;

use std::sync::Arc;

use serde_json::Value;
use tracing::warn;

pub use client::McpClient;

use crate::config::mcp::{McpServerConfig, registered_tool_name};

/// Protocol version used for modern per-request metadata.
pub const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";

/// Protocol version requested in the legacy `initialize` handshake.
pub const LEGACY_PROTOCOL_VERSION: &str = "2025-11-25";

/// Bound on `tools/list` pagination. A server that never stops returning a
/// cursor is broken; the client stops rather than looping forever.
const MAX_TOOL_PAGES: usize = 100;

/// One MCP tool, ready for registration.
#[derive(Clone, Debug)]
pub struct McpTool {
    /// Tool name as the server advertises it.
    pub name: String,
    /// Provider-safe registered name, `mcp__<server>__<tool>`.
    pub registered_name: String,
    /// Model-visible description, prefixed with the server name.
    pub description: String,
    /// JSON Schema for the tool's arguments.
    pub input_schema: Value,
    /// Optional JSON Schema for structured output. Recorded for diagnostics;
    /// this client does not validate results against it.
    pub output_schema: Option<Value>,
}

/// One connected MCP server and the tools it advertised at startup.
#[derive(Clone)]
pub struct McpServer {
    /// Local instance name from configuration.
    pub name: String,
    /// Tools discovered at startup, in server order.
    pub tools: Vec<McpTool>,
    /// Live connection used by tool executors.
    pub client: Arc<McpClient>,
    /// Negotiated protocol version.
    pub protocol_version: String,
    /// Server's self-reported name, when supplied.
    pub server_info: Option<String>,
}

impl std::fmt::Debug for McpServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpServer")
            .field("name", &self.name)
            .field("tools", &self.tools.len())
            .field("protocol_version", &self.protocol_version)
            .field("server_info", &self.server_info)
            .field("client", &self.client)
            .finish()
    }
}

impl McpServer {
    /// Stop the server process and its transport tasks.
    pub async fn shutdown(&self) {
        self.client.shutdown().await;
    }
}

/// Start one configured server, negotiate the protocol, and list its tools.
///
/// The process runs in `cwd` with Cake's environment plus the configured
/// `env`, `AGENT=cake`, and `AI_AGENT=cake`.
///
/// # Errors
///
/// Returns a human-readable diagnostic when the process cannot start, the
/// handshake fails, or the tool list cannot be read. The caller skips a server
/// that fails; it is not a fatal run error.
pub async fn connect_server(
    config: &McpServerConfig,
    cwd: &std::path::Path,
) -> Result<McpServer, String> {
    let client = McpClient::start(config, cwd)?;
    match client.handshake().await {
        Ok(protocol) => match list_tools(&client, config).await {
            Ok(tools) => {
                let server_info = client.server_info();
                Ok(McpServer {
                    name: config.name.clone(),
                    tools,
                    client: Arc::new(client),
                    protocol_version: protocol.version.clone(),
                    server_info,
                })
            },
            Err(error) => {
                client.shutdown().await;
                Err(error)
            },
        },
        Err(error) => {
            client.shutdown().await;
            Err(error)
        },
    }
}

/// Read every page of `tools/list`.
async fn list_tools(client: &McpClient, config: &McpServerConfig) -> Result<Vec<McpTool>, String> {
    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    let mut seen = std::collections::HashSet::new();

    for _ in 0..MAX_TOOL_PAGES {
        let params = cursor
            .as_ref()
            .map(|cursor| serde_json::json!({ "cursor": cursor }));
        let result = client
            .request("tools/list", params)
            .await
            .map_err(|error| format!("MCP server '{}' tools/list failed: {error}", config.name))?;
        let entries = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                format!(
                    "MCP server '{}' returned a tools/list result without a tools array",
                    config.name
                )
            })?;
        for entry in entries {
            match parse_tool(&config.name, entry) {
                Some(tool) if seen.insert(tool.registered_name.clone()) => tools.push(tool),
                Some(tool) => warn!(
                    target: "cake",
                    "MCP server '{}' advertised duplicate registered tool '{}'",
                    config.name,
                    tool.registered_name
                ),
                None => {},
            }
        }
        cursor = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .filter(|cursor| !cursor.is_empty())
            .map(str::to_string);
        if cursor.is_none() {
            return Ok(tools);
        }
    }

    Err(format!(
        "MCP server '{}' returned more than {MAX_TOOL_PAGES} pages of tools",
        config.name
    ))
}

/// Parse one `tools/list` entry.
fn parse_tool(server: &str, entry: &Value) -> Option<McpTool> {
    let name = entry
        .get("name")
        .and_then(Value::as_str)?
        .trim()
        .to_string();
    if name.is_empty() {
        warn!(target: "cake", "MCP server '{server}' advertised a tool without a name");
        return None;
    }
    let registered_name = registered_tool_name(server, &name)?;
    let description = entry
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|description| !description.is_empty())
        .map_or_else(
            || format!("MCP tool '{name}' from server '{server}'"),
            |description| format!("MCP server '{server}': {description}"),
        );
    let input_schema = match entry.get("inputSchema") {
        None => serde_json::json!({"type": "object", "properties": {}}),
        Some(schema) if schema.is_object() => schema.clone(),
        Some(_) => {
            warn!(
                target: "cake",
                "MCP server '{}' advertised tool '{}' with a non-object inputSchema; skipping it",
                server,
                name
            );
            return None;
        },
    };
    Some(McpTool {
        name,
        registered_name,
        description,
        input_schema,
        output_schema: entry.get("outputSchema").cloned(),
    })
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
