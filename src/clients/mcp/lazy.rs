//! On-demand connections for deferred MCP servers.
//!
//! A deferred server is configured but not started until `ToolSearch` needs
//! its catalog. Each server's first connection is memoized, so concurrent
//! searches share one process and a failed start is not retried within the
//! run.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use futures::StreamExt;
use tokio::sync::OnceCell;

use crate::clients::mcp::{McpServer, connect_server};
use crate::concurrency::MAX_CONCURRENT_AGENT_OPERATIONS;
use crate::config::mcp::{McpServerConfig, registered_server_prefix};

type Connection = OnceCell<Result<McpServer, String>>;

/// Deferred MCP servers and their memoized connections.
#[derive(Clone)]
pub struct LazyMcpRuntime {
    cwd: PathBuf,
    servers: Arc<Vec<LazyMcpServer>>,
    connected: Arc<RwLock<HashMap<String, McpServer>>>,
    enabled_tools: Option<Arc<Vec<String>>>,
}

struct LazyMcpServer {
    config: McpServerConfig,
    state: Arc<Connection>,
}

/// Servers available after one connection attempt and the ones that failed.
#[derive(Debug, Default)]
pub struct LazyConnectOutcome {
    /// Every connected lazy server, in configuration order.
    pub servers: Vec<McpServer>,
    /// Names of the selected servers that could not start.
    pub failed: Vec<String>,
}

impl LazyMcpRuntime {
    /// Build a runtime over deferred server definitions.
    #[must_use]
    pub fn new(
        cwd: PathBuf,
        configs: Vec<McpServerConfig>,
        enabled_tools: Option<&[String]>,
    ) -> Self {
        let servers = configs
            .into_iter()
            .map(|config| LazyMcpServer {
                config,
                state: Arc::new(OnceCell::new()),
            })
            .collect();
        Self {
            cwd,
            servers: Arc::new(servers),
            connected: Arc::new(RwLock::new(HashMap::new())),
            enabled_tools: enabled_tools.map(|names| Arc::new(names.to_vec())),
        }
    }

    /// Whether no deferred servers need a connection.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    /// Configured definitions in discovery order.
    pub fn configs(&self) -> impl Iterator<Item = &McpServerConfig> {
        self.servers.iter().map(|server| &server.config)
    }

    /// Configured server names in discovery order.
    #[must_use]
    pub fn server_names(&self) -> Vec<&str> {
        self.servers
            .iter()
            .map(|server| server.config.name.as_str())
            .collect()
    }

    /// Whether the exact-name allowlist permits one registered tool.
    #[must_use]
    pub fn tool_enabled(&self, name: &str) -> bool {
        self.enabled_tools
            .as_ref()
            .is_none_or(|names| names.iter().any(|enabled| enabled == name))
    }

    /// Whether one registered name could come from a deferred server.
    #[must_use]
    pub fn owns_tool(&self, name: &str) -> bool {
        self.servers.iter().any(|server| {
            registered_server_prefix(&server.config.name)
                .is_some_and(|prefix| name.starts_with(&prefix))
        })
    }

    /// Every lazy server already connected in this run.
    #[must_use]
    pub fn connected_servers(&self) -> Vec<McpServer> {
        let connected = self
            .connected
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.servers
            .iter()
            .filter_map(|server| connected.get(&server.config.name).cloned())
            .collect()
    }

    /// Replace one connected server's snapshot after a tool-list refresh.
    pub fn update_server(&self, server: McpServer) {
        self.connected
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(server.name.clone(), server);
    }

    /// Connect the named server, or every deferred server when unnamed.
    ///
    /// A server that is already connected is not started again.
    pub async fn connect(&self, server: Option<&str>) -> LazyConnectOutcome {
        self.ensure(|entry| {
            server.is_none_or(|name| entry.config.name.eq_ignore_ascii_case(name.trim()))
        })
        .await
    }

    /// Connect the servers that own any of the registered tool names.
    pub async fn connect_for_tools(&self, tools: &[String]) -> LazyConnectOutcome {
        let names: HashSet<&str> = self
            .servers
            .iter()
            .filter(|server| {
                registered_server_prefix(&server.config.name)
                    .is_some_and(|prefix| tools.iter().any(|tool| tool.starts_with(&prefix)))
            })
            .map(|server| server.config.name.as_str())
            .collect();
        self.ensure(|entry| names.contains(entry.config.name.as_str()))
            .await
    }

    async fn ensure<F>(&self, mut selected: F) -> LazyConnectOutcome
    where
        F: FnMut(&LazyMcpServer) -> bool,
    {
        let cwd = self.cwd.clone();
        let connected = Arc::clone(&self.connected);
        let targets: Vec<(McpServerConfig, Arc<Connection>)> = self
            .servers
            .iter()
            .filter(|server| selected(server))
            .map(|server| (server.config.clone(), Arc::clone(&server.state)))
            .collect();
        let results = futures::stream::iter(targets.into_iter().map(|(config, state)| {
            let cwd = cwd.clone();
            let connected = Arc::clone(&connected);
            async move {
                let result = state
                    .get_or_init(|| {
                        let config = config.clone();
                        let cwd = cwd.clone();
                        async move {
                            match connect_server(&config, &cwd).await {
                                Ok(server) => Ok(server),
                                Err(error) => {
                                    eprintln!(
                                        "warning: skipping deferred MCP server '{}': {error}",
                                        config.name
                                    );
                                    Err(error)
                                },
                            }
                        }
                    })
                    .await;
                let available = if let Ok(server) = result {
                    connected
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(config.name.clone(), server.clone());
                    true
                } else {
                    false
                };
                (config.name, available)
            }
        }))
        .buffered(MAX_CONCURRENT_AGENT_OPERATIONS)
        .collect::<Vec<_>>()
        .await;

        let failed = results
            .iter()
            .filter(|(_, connected)| !*connected)
            .map(|(name, _)| name.clone())
            .collect();
        LazyConnectOutcome {
            servers: self.connected_servers(),
            failed,
        }
    }
}

impl std::fmt::Debug for LazyMcpRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LazyMcpRuntime")
            .field("cwd", &self.cwd)
            .field("servers", &self.server_names())
            .field("enabled_tools", &self.enabled_tools)
            .finish_non_exhaustive()
    }
}
