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

pub use client::{McpClient, ToolListChangeMode};

use crate::config::mcp::{McpExposure, McpServerConfig, registered_tool_name};

/// Protocol version used for modern per-request metadata.
pub const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";

/// Protocol version requested in the legacy `initialize` handshake.
pub const LEGACY_PROTOCOL_VERSION: &str = "2025-11-25";

/// Bound on `tools/list` pagination. A server that never stops returning a
/// cursor is broken; the client stops rather than looping forever.
const MAX_TOOL_PAGES: usize = 100;

/// One MCP tool, ready for registration.
#[derive(Clone, Debug, PartialEq, Eq)]
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

/// One connected MCP server and its current tool inventory.
#[derive(Clone)]
pub struct McpServer {
    /// Local instance name from configuration.
    pub name: String,
    /// Optional one-line description from configuration.
    pub description: Option<String>,
    /// How this server's tools reach the model.
    pub exposure: McpExposure,
    /// Current tools advertised by the server, in server order.
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
            .field("description", &self.description)
            .field("exposure", &self.exposure)
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

    /// Re-read every `tools/list` page after a list-change notification.
    ///
    /// Returns whether the advertised set differs from the current snapshot.
    /// On success the snapshot is replaced; on failure the caller keeps the
    /// previous tools and can retry after the next notification.
    ///
    /// # Errors
    ///
    /// Returns the server's diagnostic when the list cannot be read.
    pub async fn reload_tools(&mut self) -> Result<bool, String> {
        let tools = list_tools(&self.client, &self.name).await?;
        let changed = tools != self.tools;
        self.tools = tools;
        Ok(changed)
    }

    /// Count of accepted tool-list-change notifications for this server.
    #[must_use]
    pub fn tools_generation(&self) -> u64 {
        self.client.tools_generation()
    }

    /// How this server delivers tool-list changes.
    #[must_use]
    pub fn tool_list_change_mode(&self) -> ToolListChangeMode {
        self.client.tool_list_change_mode()
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
        Ok(protocol) => {
            // Subscribe before the initial listing: a change that lands during
            // startup is then counted and refreshed instead of being lost
            // between the listing and the subscription.
            if let Err(error) = client.subscribe_tools_list_changed() {
                warn!(
                    target: "cake",
                    "MCP server '{}' could not open a tool-list-change subscription: {error}",
                    config.name
                );
            }
            match list_tools(&client, &config.name).await {
                Ok(tools) => {
                    let server_info = client.server_info();
                    Ok(McpServer {
                        name: config.name.clone(),
                        description: config.description.clone(),
                        exposure: config.exposure,
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
            }
        },
        Err(error) => {
            client.shutdown().await;
            Err(error)
        },
    }
}

/// Read every page of `tools/list`.
async fn list_tools(client: &McpClient, server: &str) -> Result<Vec<McpTool>, String> {
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
            .map_err(|error| format!("MCP server '{server}' tools/list failed: {error}"))?;
        let entries = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                format!("MCP server '{server}' returned a tools/list result without a tools array")
            })?;
        for entry in entries {
            match parse_tool(server, entry) {
                Some(tool) if seen.insert(tool.registered_name.clone()) => tools.push(tool),
                Some(tool) => warn!(
                    target: "cake",
                    "MCP server '{server}' advertised duplicate registered tool '{}'",
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
        "MCP server '{server}' returned more than {MAX_TOOL_PAGES} pages of tools"
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
pub mod test_support {
    //! Shared fixture helpers for MCP client and agent tests.

    use std::collections::BTreeMap;
    use std::path::Path;

    use crate::clients::mcp::{McpServer, connect_server};
    use crate::config::mcp::{McpExposure, McpServerConfig};

    pub const FIXTURE: &str = include_str!("fixtures/fake_mcp_server.py");

    /// Write the fixture script into `directory` and describe it as a server.
    pub fn fixture_config(
        directory: &Path,
        era: &str,
        dynamic: bool,
        exposure: McpExposure,
        timeout_secs: u64,
    ) -> McpServerConfig {
        let script = directory.join("fake_mcp_server.py");
        std::fs::write(&script, FIXTURE).unwrap();
        let mut env = BTreeMap::from([("FAKE_MCP_ERA".to_string(), era.to_string())]);
        if dynamic {
            env.insert("FAKE_MCP_DYNAMIC".to_string(), "1".to_string());
        }
        McpServerConfig {
            name: "fake".to_string(),
            command: "python3".to_string(),
            args: vec![script.display().to_string()],
            env,
            timeout_secs,
            description: Some("Fixture server".to_string()),
            exposure,
            source: directory.join("mcp.json"),
        }
    }

    /// Start the Python fixture in `era`, optionally in dynamic mode.
    pub async fn connect_fixture(
        era: &str,
        dynamic: bool,
        exposure: McpExposure,
        timeout_secs: u64,
    ) -> (tempfile::TempDir, McpServer) {
        let directory = tempfile::tempdir().unwrap();
        let config = fixture_config(directory.path(), era, dynamic, exposure, timeout_secs);
        let server = connect_server(&config, directory.path())
            .await
            .unwrap_or_else(|error| panic!("{era} fixture failed to connect: {error}"));
        (directory, server)
    }

    /// Wait until a server's accepted tool-change counter advances.
    pub async fn wait_for_tools_generation(server: &McpServer, previous: u64) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while server.tools_generation() == previous {
            assert!(
                std::time::Instant::now() < deadline,
                "the fixture's tool-change notification did not arrive"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
