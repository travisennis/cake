//! Process and JSON-RPC transport for one MCP server.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tracing::{debug, warn};

use crate::config::mcp::McpServerConfig;
use crate::config::toolbox::place_in_own_process_group;

use crate::clients::mcp::{LEGACY_PROTOCOL_VERSION, MODERN_PROTOCOL_VERSION};

/// Bound on the `server/discover` probe. A legacy server commonly answers with
/// a method-not-found error immediately; one that ignores the probe falls back
/// after this wait.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Bound on the legacy `initialize` request.
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(30);

/// Grace period between `SIGKILL` and dropping a child.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Negotiated protocol.
#[derive(Clone, Debug)]
pub(super) struct Protocol {
    /// Version string used in `_meta` or reported by `initialize`.
    pub(super) version: String,
    /// True for per-request metadata; false for the initialize handshake.
    pub(super) modern: bool,
}

/// How a connected server delivers tool-list changes to this client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolListChangeMode {
    /// The handshake has not completed, so no watch is known yet.
    None,
    /// The legacy era sends the notification without a subscription.
    Notifications,
    /// The modern era delivers it on a `subscriptions/listen` stream.
    Subscription,
}

impl ToolListChangeMode {
    /// Short label for diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Notifications => "notifications",
            Self::Subscription => "subscription",
        }
    }
}

/// One JSON-RPC error returned by the server.
#[derive(Debug)]
struct RpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "error {}: {}", self.code, self.message)
    }
}

/// Failure while waiting for a request.
#[derive(Debug)]
enum RequestError {
    Rpc(RpcError),
    Closed(String),
    Timeout { method: String, seconds: u64 },
    Protocol(String),
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rpc(error) => write!(formatter, "{error}"),
            Self::Closed(server) => write!(formatter, "server '{server}' closed the connection"),
            Self::Timeout { method, seconds } => {
                write!(formatter, "'{method}' timed out after {seconds}s")
            },
            Self::Protocol(message) => formatter.write_str(message),
        }
    }
}

/// Shared request/response state for one server.
struct Transport {
    server: String,
    timeout: Duration,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, RequestError>>>>,
    writer: mpsc::UnboundedSender<String>,
    closed: AtomicBool,
    /// Accepted `notifications/tools/list_changed` messages. The agent keeps
    /// its own last-observed value, so a notification that arrives during a
    /// re-list triggers one more pass instead of being lost.
    tools_changed: AtomicU64,
    /// Open `subscriptions/listen` request ids. Their responses mean graceful
    /// closure, not a normal request result, so they never enter `pending`.
    subscriptions: std::sync::Mutex<HashSet<u64>>,
}

impl Transport {
    /// Send one request and wait for its response.
    async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        limit: Duration,
    ) -> Result<Value, RequestError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(RequestError::Closed(self.server.clone()));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().await.insert(id, sender);

        let frame = request_frame(id, method, params);
        if self.writer.send(frame).is_err() {
            self.pending.lock().await.remove(&id);
            return Err(RequestError::Closed(self.server.clone()));
        }

        match timeout(limit, receiver).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => Err(RequestError::Closed(self.server.clone())),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                self.notify(
                    "notifications/cancelled",
                    Some(json!({ "requestId": id, "reason": "request timed out" })),
                );
                Err(RequestError::Timeout {
                    method: method.to_string(),
                    seconds: limit.as_secs(),
                })
            },
        }
    }

    /// Send one request whose response may never arrive, returning its id.
    fn send_open_request(&self, method: &str, params: Option<Value>) -> Result<u64, RequestError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(RequestError::Closed(self.server.clone()));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = request_frame(id, method, params);
        if self.writer.send(frame).is_err() {
            return Err(RequestError::Closed(self.server.clone()));
        }
        Ok(id)
    }

    /// Remember an open subscription id.
    fn add_subscription(&self, id: u64) {
        self.subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id);
    }

    /// Whether `id` names a subscription this client opened.
    fn has_subscription(&self, id: u64) -> bool {
        self.subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&id)
    }

    /// Forget a subscription id, returning whether it was present.
    fn remove_subscription(&self, id: u64) -> bool {
        self.subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id)
    }

    /// Send one notification. Write failures are ignored: the next request
    /// reports the closed connection.
    fn notify(&self, method: &str, params: Option<Value>) {
        let mut frame = Map::new();
        frame.insert("jsonrpc".to_string(), json!("2.0"));
        frame.insert("method".to_string(), json!(method));
        if let Some(params) = params {
            frame.insert("params".to_string(), params);
        }
        _ = self.send_value(&Value::Object(frame));
    }

    /// Send one already-built frame.
    fn send_value(&self, frame: &Value) -> Result<(), String> {
        let line = serde_json::to_string(frame).map_err(|error| error.to_string())?;
        self.writer.send(line).map_err(|error| {
            format!(
                "MCP server '{}' is not accepting input: {error:?}",
                self.server
            )
        })
    }

    /// Fail every waiting request and stop accepting new ones.
    async fn close_pending(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let mut pending = self.pending.lock().await;
        for (_, sender) in pending.drain() {
            _ = sender.send(Err(RequestError::Closed(self.server.clone())));
        }
    }
}

fn request_frame(id: u64, method: &str, params: Option<Value>) -> String {
    let mut frame = Map::new();
    frame.insert("jsonrpc".to_string(), json!("2.0"));
    frame.insert("id".to_string(), json!(id));
    frame.insert("method".to_string(), json!(method));
    if let Some(params) = params {
        frame.insert("params".to_string(), params);
    }
    serde_json::to_string(&Value::Object(frame)).unwrap_or_default()
}

/// A live connection to one MCP server.
pub struct McpClient {
    server: String,
    transport: Arc<Transport>,
    protocol: std::sync::RwLock<Option<Protocol>>,
    server_info: std::sync::RwLock<Option<String>>,
    child: std::sync::Mutex<Option<Child>>,
    tasks: std::sync::Mutex<Vec<JoinHandle<()>>>,
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpClient")
            .field("server", &self.server)
            .field("protocol", &self.protocol_version())
            .finish_non_exhaustive()
    }
}

impl McpClient {
    /// Spawn the configured server and start its transport tasks.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic when the process cannot start or its pipes cannot
    /// be captured.
    pub(super) fn start(config: &McpServerConfig, cwd: &Path) -> Result<Self, String> {
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .envs(&config.env)
            .env("AGENT", "cake")
            .env("AI_AGENT", "cake")
            .current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        place_in_own_process_group(&mut command);

        let mut child = command.spawn().map_err(|error| {
            format!(
                "MCP server '{}' failed to start '{}': {error}",
                config.name, config.command
            )
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("MCP server '{}' did not provide stdin", config.name))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("MCP server '{}' did not provide stdout", config.name))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| format!("MCP server '{}' did not provide stderr", config.name))?;

        let (writer, receiver) = mpsc::unbounded_channel();
        let transport = Arc::new(Transport {
            server: config.name.clone(),
            timeout: Duration::from_secs(config.timeout_secs),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            writer,
            closed: AtomicBool::new(false),
            tools_changed: AtomicU64::new(0),
            subscriptions: std::sync::Mutex::new(HashSet::new()),
        });

        let tasks = vec![
            tokio::spawn(writer_loop(receiver, stdin)),
            tokio::spawn(reader_loop(Arc::clone(&transport), stdout)),
            tokio::spawn(stderr_loop(config.name.clone(), stderr)),
        ];

        Ok(Self {
            server: config.name.clone(),
            transport,
            protocol: std::sync::RwLock::new(None),
            server_info: std::sync::RwLock::new(None),
            child: std::sync::Mutex::new(Some(child)),
            tasks: std::sync::Mutex::new(tasks),
        })
    }

    /// Negotiate the protocol era and protocol version.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic when the server answers the probe or handshake
    /// with an unusable result.
    pub(super) async fn handshake(&self) -> Result<Protocol, String> {
        match self.probe().await {
            Probe::Modern { protocol, info } => {
                self.set_protocol(protocol.clone());
                self.store_server_info(info);
                Ok(protocol)
            },
            Probe::Legacy => {
                let protocol = self.legacy_handshake().await?;
                self.set_protocol(protocol.clone());
                Ok(protocol)
            },
            Probe::Failed(error) => Err(error),
        }
    }

    /// Send one protocol request, adding modern `_meta` when required.
    pub(super) async fn request(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, String> {
        let params = self.with_meta(params);
        self.transport
            .request(method, params, self.transport.timeout)
            .await
            .map_err(|error| error.to_string())
    }

    /// Open the modern tool-list-change subscription.
    ///
    /// The response to `subscriptions/listen` is a graceful-closure signal, so
    /// this does not wait for one. A legacy server needs no subscription and
    /// this is a no-op there.
    pub(super) fn subscribe_tools_list_changed(&self) -> Result<(), String> {
        let modern = self
            .protocol
            .read()
            .ok()
            .and_then(|protocol| protocol.as_ref().map(|protocol| protocol.modern))
            .unwrap_or(false);
        if !modern {
            return Ok(());
        }
        let params = self.with_meta(Some(json!({
            "notifications": { "toolsListChanged": true },
        })));
        let id = self
            .transport
            .send_open_request("subscriptions/listen", params)
            .map_err(|error| error.to_string())?;
        self.transport.add_subscription(id);
        debug!(
            target: "cake",
            "MCP server '{}' opened tool-list-change subscription {id}",
            self.server
        );
        Ok(())
    }

    /// Count of accepted tool-list-change notifications.
    #[must_use]
    pub fn tools_generation(&self) -> u64 {
        self.transport.tools_changed.load(Ordering::SeqCst)
    }

    /// How this server delivers tool-list changes.
    #[must_use]
    pub fn tool_list_change_mode(&self) -> ToolListChangeMode {
        match self
            .protocol
            .read()
            .ok()
            .and_then(|protocol| protocol.clone())
        {
            Some(protocol) if protocol.modern => ToolListChangeMode::Subscription,
            Some(_) => ToolListChangeMode::Notifications,
            None => ToolListChangeMode::None,
        }
    }

    /// Invoke one tool and return its raw `tools/call` result.
    pub async fn call_tool(&self, tool: &str, arguments: Value) -> Result<Value, String> {
        self.request(
            "tools/call",
            Some(json!({ "name": tool, "arguments": arguments })),
        )
        .await
    }

    /// Negotiated protocol version, or an empty string before the handshake.
    #[must_use]
    pub fn protocol_version(&self) -> String {
        self.protocol
            .read()
            .ok()
            .and_then(|protocol| protocol.as_ref().map(|protocol| protocol.version.clone()))
            .unwrap_or_default()
    }

    /// Server's self-reported name, when supplied.
    #[must_use]
    pub fn server_info(&self) -> Option<String> {
        self.server_info.read().ok().and_then(|info| info.clone())
    }

    /// Stop the process and abort its transport tasks.
    pub async fn shutdown(&self) {
        self.transport.closed.store(true, Ordering::SeqCst);
        let child = self.take_child();
        if let Some(mut child) = child {
            _ = child.start_kill();
            _ = timeout(SHUTDOWN_GRACE, child.wait()).await;
        }
        self.transport.close_pending().await;
        self.abort_tasks();
    }

    /// Probe for a modern server, falling back to the legacy handshake.
    async fn probe(&self) -> Probe {
        let params = Some(modern_meta(MODERN_PROTOCOL_VERSION));
        match self
            .transport
            .request("server/discover", params, PROBE_TIMEOUT)
            .await
        {
            Ok(result) => probe_result(&result),
            Err(RequestError::Rpc(RpcError {
                code: -32022, data, ..
            })) => probe_unsupported(data.as_ref()),
            Err(_) => Probe::Legacy,
        }
    }

    /// Run the legacy `initialize` and `notifications/initialized` exchange.
    async fn legacy_handshake(&self) -> Result<Protocol, String> {
        let params = json!({
            "protocolVersion": LEGACY_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": client_info(),
        });
        let result = self
            .transport
            .request("initialize", Some(params), INITIALIZE_TIMEOUT)
            .await
            .map_err(|error| format!("MCP server '{}' initialize failed: {error}", self.server))?;
        let version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(LEGACY_PROTOCOL_VERSION)
            .to_string();
        if version.as_str() >= MODERN_PROTOCOL_VERSION {
            return Err(format!(
                "MCP server '{}' negotiated modern protocol {version} through initialize; \
                 this cake build supports it only through server/discover",
                self.server
            ));
        }
        self.transport.notify("notifications/initialized", None);
        self.store_server_info(server_info_from(&result));
        Ok(Protocol {
            version,
            modern: false,
        })
    }

    /// Add modern per-request metadata when the negotiated era requires it.
    fn with_meta(&self, params: Option<Value>) -> Option<Value> {
        let protocol = self
            .protocol
            .read()
            .ok()
            .and_then(|protocol| protocol.clone())?;
        if !protocol.modern {
            return params;
        }
        let mut object = match params {
            Some(Value::Object(object)) => object,
            Some(other) => return Some(other),
            None => Map::new(),
        };
        object.insert("_meta".to_string(), modern_meta(&protocol.version));
        Some(Value::Object(object))
    }

    fn set_protocol(&self, protocol: Protocol) {
        if let Ok(mut current) = self.protocol.write() {
            *current = Some(protocol);
        }
    }

    fn store_server_info(&self, info: Option<String>) {
        if let Ok(mut current) = self.server_info.write() {
            *current = info;
        }
    }

    fn take_child(&self) -> Option<Child> {
        match self.child.lock() {
            Ok(mut child) => child.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        }
    }

    fn abort_tasks(&self) {
        if let Ok(mut tasks) = self.tasks.lock() {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock()
            && let Some(child) = child.as_mut()
        {
            _ = child.start_kill();
        }
        if let Ok(mut tasks) = self.tasks.lock() {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
    }
}

/// Result of the modern discovery probe.
enum Probe {
    Modern {
        protocol: Protocol,
        info: Option<String>,
    },
    Legacy,
    Failed(String),
}

/// Interpret a successful `server/discover` result.
fn probe_result(result: &Value) -> Probe {
    match choose_protocol(result.get("supportedVersions").and_then(Value::as_array)) {
        Ok(Some(version)) => Probe::Modern {
            protocol: Protocol {
                version,
                modern: true,
            },
            info: server_info_from_discover(result),
        },
        Ok(None) => Probe::Legacy,
        Err(error) => Probe::Failed(error),
    }
}

/// Interpret an `UnsupportedProtocolVersionError` response.
fn probe_unsupported(data: Option<&Value>) -> Probe {
    let versions = data
        .and_then(|data| data.get("supported"))
        .and_then(Value::as_array);
    if versions.is_none_or(Vec::is_empty) {
        return Probe::Failed(
            "MCP server rejected the requested protocol version without listing supported \
             versions"
                .to_string(),
        );
    }
    match choose_protocol(versions) {
        Ok(Some(version)) => Probe::Modern {
            protocol: Protocol {
                version,
                modern: true,
            },
            info: None,
        },
        Ok(None) => Probe::Legacy,
        Err(error) => Probe::Failed(error),
    }
}

/// Pick a modern version from the server's advertised list.
///
/// `Ok(Some(version))` means use per-request metadata. `Ok(None)` means the
/// server advertises only a legacy version, so the caller falls back to the
/// initialize handshake. An empty or absent list after a successful discovery
/// means the server accepted the modern probe version.
pub(super) fn choose_protocol(versions: Option<&Vec<Value>>) -> Result<Option<String>, String> {
    let Some(values) = versions else {
        return Ok(Some(MODERN_PROTOCOL_VERSION.to_string()));
    };
    let versions: Vec<&str> = values.iter().filter_map(Value::as_str).collect();
    if versions.is_empty() {
        return Ok(Some(MODERN_PROTOCOL_VERSION.to_string()));
    }
    if versions.contains(&MODERN_PROTOCOL_VERSION) {
        return Ok(Some(MODERN_PROTOCOL_VERSION.to_string()));
    }
    if versions.contains(&LEGACY_PROTOCOL_VERSION) {
        return Ok(None);
    }
    let newest = versions.iter().max().copied().unwrap_or_default();
    if newest > MODERN_PROTOCOL_VERSION {
        Err(format!(
            "MCP server requires protocol {newest}, which this cake build does not support; \
             upgrade cake"
        ))
    } else {
        Err(format!(
            "MCP server and cake have no mutually supported protocol version; server supports \
             {}",
            versions.join(", ")
        ))
    }
}

fn modern_meta(version: &str) -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": version,
        "io.modelcontextprotocol/clientInfo": client_info(),
        "io.modelcontextprotocol/clientCapabilities": {},
    })
}

fn client_info() -> Value {
    json!({
        "name": "cake",
        "version": env!("CARGO_PKG_VERSION"),
    })
}

fn server_info_from(result: &Value) -> Option<String> {
    result
        .get("serverInfo")
        .and_then(|info| info.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn server_info_from_discover(result: &Value) -> Option<String> {
    result
        .get("_meta")
        .and_then(|meta| meta.get("io.modelcontextprotocol/serverInfo"))
        .and_then(|info| info.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Docker-style writer loop: one line per frame, flushed immediately.
async fn writer_loop(
    mut receiver: mpsc::UnboundedReceiver<String>,
    mut stdin: tokio::process::ChildStdin,
) {
    while let Some(line) = receiver.recv().await {
        if stdin.write_all(line.as_bytes()).await.is_err() {
            break;
        }
        if stdin.write_all(b"\n").await.is_err() {
            break;
        }
        if stdin.flush().await.is_err() {
            break;
        }
    }
}

/// Read server frames until stdout closes, then fail every waiting request.
async fn reader_loop(transport: Arc<Transport>, stdout: tokio::process::ChildStdout) {
    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => handle_line(&transport, &line).await,
            Ok(None) => break,
            Err(error) => {
                debug!(
                    target: "cake",
                    "MCP server '{}' stdout read failed: {error}",
                    transport.server
                );
                break;
            },
        }
    }
    transport.close_pending().await;
}

/// Log a server's stderr without letting one line grow without bound.
async fn stderr_loop(server: String, stderr: tokio::process::ChildStderr) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if !line.trim().is_empty() {
            let bounded: String = line.chars().take(1_024).collect();
            warn!(target: "cake", "MCP server '{server}' stderr: {bounded}");
        }
    }
}

/// Dispatch one parsed line from the server.
async fn handle_line(transport: &Arc<Transport>, line: &str) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }
    let Ok(message) = serde_json::from_str::<Value>(trimmed) else {
        warn!(
            target: "cake",
            "MCP server '{}' wrote non-JSON output to stdout; ignoring it",
            transport.server
        );
        return;
    };
    if message.get("method").is_some() {
        if message.get("id").is_some() {
            handle_server_request(transport, &message);
        } else {
            handle_notification(transport, &message);
        }
    } else if message.get("id").is_some() {
        handle_response(transport, &message).await;
    }
}

/// Answer a server-initiated request. Cake supports only the capability-free
/// requests every client can answer.
fn handle_server_request(transport: &Arc<Transport>, message: &Value) {
    let Some(id) = message.get("id").cloned() else {
        return;
    };
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let response = match method {
        "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
        "roots/list" => json!({ "jsonrpc": "2.0", "id": id, "result": { "roots": [] } }),
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": "Method not found" },
        }),
    };
    _ = transport.send_value(&response);
}

/// Log a server notification and count tool-list changes.
fn handle_notification(transport: &Arc<Transport>, message: &Value) {
    match message.get("method").and_then(Value::as_str) {
        Some("notifications/tools/list_changed") => {
            // Modern notifications name the subscription they arrived on. A
            // notification for a subscription this client did not open cannot
            // be trusted as belonging to this connection's tool stream.
            if let Some(id) = subscription_id(message)
                && !transport.has_subscription(id)
            {
                debug!(
                    target: "cake",
                    "MCP server '{}' sent a tool-list change for unknown subscription {id}; ignoring it",
                    transport.server
                );
                return;
            }
            transport.tools_changed.fetch_add(1, Ordering::SeqCst);
            debug!(
                target: "cake",
                "MCP server '{}' reported a tool-list change; the agent will re-list it before the next request",
                transport.server
            );
        },
        Some("notifications/subscriptions/acknowledged") => {
            let accepted = message
                .pointer("/params/notifications/toolsListChanged")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            debug!(
                target: "cake",
                "MCP server '{}' {} the tool-list-change subscription",
                transport.server,
                if accepted { "acknowledged" } else { "did not accept" }
            );
        },
        Some(method) => {
            debug!(
                target: "cake",
                "MCP server '{}' sent notification {method}",
                transport.server
            );
        },
        None => {},
    }
}

/// The modern subscription id carried by a notification.
fn subscription_id(message: &Value) -> Option<u64> {
    message
        .pointer("/params/_meta/io.modelcontextprotocol/subscriptionId")
        .and_then(Value::as_u64)
}

/// Interpret one response frame as a result or an error.
fn response_outcome(transport: &Transport, message: &Value) -> Result<Value, RequestError> {
    message.get("result").map_or_else(
        || {
            message.get("error").map_or_else(
                || {
                    Err(RequestError::Protocol(format!(
                        "MCP server '{}' returned a response with neither result nor error",
                        transport.server
                    )))
                },
                |error| {
                    Err(RequestError::Rpc(RpcError {
                        code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                        message: error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error")
                            .to_string(),
                        data: error.get("data").cloned(),
                    }))
                },
            )
        },
        |result| Ok(result.clone()),
    )
}

/// Resolve one response frame against the pending request table.
async fn handle_response(transport: &Arc<Transport>, message: &Value) {
    let Some(id) = message.get("id").and_then(Value::as_u64) else {
        debug!(
            target: "cake",
            "MCP server '{}' returned an unrecognized response id",
            transport.server
        );
        return;
    };
    // A `subscriptions/listen` response reports graceful closure, not a
    // request result. It never has a pending sender.
    if transport.remove_subscription(id) {
        match response_outcome(transport, message) {
            Ok(_) => debug!(
                target: "cake",
                "MCP server '{}' closed subscription {id}",
                transport.server
            ),
            Err(error) => warn!(
                target: "cake",
                "MCP server '{}' subscription {id} failed: {error}",
                transport.server
            ),
        }
        return;
    }
    let Some(sender) = transport.pending.lock().await.remove(&id) else {
        // A response that arrives after its request timed out is harmless.
        return;
    };
    _ = sender.send(response_outcome(transport, message));
}
