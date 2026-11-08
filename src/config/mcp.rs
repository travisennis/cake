//! MCP server configuration discovery and naming.
//!
//! Cake reads the ecosystem `mcpServers` JSON shape from three sources, in
//! increasing precedence:
//!
//! 1. the user-owned global file `<config>/cake/mcp.json`;
//! 2. the trusted project file `.cake/mcp.json`;
//! 3. each explicit `--mcp-config <path>` file, in command-line order.
//!
//! Later sources replace earlier servers with the same name. A server entry
//! names the executable and its arguments directly, matching the configuration
//! other MCP clients document:
//!
//! ```json
//! {
//!   "mcpServers": {
//!     "safari-mcp": { "command": "/usr/bin/safaridriver", "args": ["--mcp"] }
//!   }
//! }
//! ```
//!
//! An entry may also carry a one-line `description` and an `exposure` of
//! `direct`, `deferred` (the default), or `hidden`. `deferred` keeps tool
//! schemas out of the request until `ToolSearch` selects them; `hidden`
//! prevents the server from starting at all.
//!
//! Discovery only reads and validates configuration. Starting servers and
//! speaking the protocol live in `clients::mcp`. `cake mcp add` and
//! `cake mcp remove` edit the user-owned global file through this module.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::trust::{ProjectConfigMode, ProjectTrustReport};

/// File name under `<config>/cake/` for the user-owned server list.
pub const MCP_CONFIG_FILE: &str = "mcp.json";

/// Project-relative path for a trusted project's server list.
pub const PROJECT_MCP_CONFIG: &str = ".cake/mcp.json";

/// Prefix applied to every registered MCP tool.
pub const MCP_TOOL_PREFIX: &str = "mcp__";

/// Default per-request timeout for a server.
pub const DEFAULT_MCP_TIMEOUT_SECS: u64 = 60;

/// Maximum timeout accepted from configuration.
pub const MAX_MCP_TIMEOUT_SECS: u64 = 3_600;

/// How a configured server's tools are exposed to the model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum McpExposure {
    /// Register every tool eagerly, as Cake did before deferred loading.
    Direct,
    /// Keep tool schemas out of the request until the model searches for one.
    #[default]
    Deferred,
    /// Never advertise or start the server.
    Hidden,
}

impl McpExposure {
    /// Parse the lowercase value accepted by `mcp.json` and `cake mcp add`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "direct" => Some(Self::Direct),
            "deferred" => Some(Self::Deferred),
            "hidden" => Some(Self::Hidden),
            _ => None,
        }
    }

    /// Lowercase value written to `mcp.json` and human-readable output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Deferred => "deferred",
            Self::Hidden => "hidden",
        }
    }
}

/// Maximum length of a server name. Leaves room for the prefix, separators,
/// one tool-name character, and a truncation hash within the 64-character
/// provider function-name limit.
const MAX_SERVER_NAME_LEN: usize = 32;

/// Maximum registered tool name length, matching the provider function-name
/// limit used by `config::toolbox`.
const MAX_REGISTERED_NAME_LEN: usize = 64;

/// Bytes appended to a truncated name: two separators plus eight hex digits
/// of a SHA-256 digest.
const TRUNCATION_SUFFIX_LEN: usize = 10;

/// One resolved MCP server definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServerConfig {
    /// Local instance name from the JSON object key.
    pub name: String,
    /// Executable to spawn.
    pub command: String,
    /// Arguments passed to the executable.
    pub args: Vec<String>,
    /// Additional environment variables. Values are never logged.
    pub env: BTreeMap<String, String>,
    /// Per-request timeout in seconds, clamped to 1..=3600.
    pub timeout_secs: u64,
    /// Optional one-line description shown in the MCP server summary.
    pub description: Option<String>,
    /// How this server's tools reach the model.
    pub exposure: McpExposure,
    /// File the definition came from, for diagnostics.
    pub source: PathBuf,
}

/// Resolved server list plus non-fatal discovery warnings.
#[derive(Debug, Default)]
pub struct McpConfigLoad {
    /// Servers in deterministic name order after precedence resolution.
    pub servers: Vec<McpServerConfig>,
    /// Human-readable problems that skipped a file or server.
    pub warnings: Vec<String>,
}

/// A configuration file the user explicitly named could not be used.
#[derive(Debug, thiserror::Error)]
pub enum McpConfigError {
    /// The explicit file does not exist.
    #[error("MCP config file {path} does not exist")]
    Missing {
        /// The path named on the command line.
        path: PathBuf,
    },
    /// The explicit file could not be read.
    #[error("MCP config file {path} could not be read: {source}")]
    Read {
        /// The path named on the command line.
        path: PathBuf,
        /// The underlying read failure.
        #[source]
        source: std::io::Error,
    },
    /// The explicit file is not valid JSON or not the documented shape.
    #[error("MCP config file {path} is invalid: {message}")]
    Invalid {
        /// The path named on the command line.
        path: PathBuf,
        /// The parser or validation diagnostic.
        message: String,
    },
}

/// JSON shape of one configuration file.
#[derive(Debug, Deserialize)]
struct RawMcpFile {
    #[serde(rename = "mcpServers", default)]
    mcp_servers: BTreeMap<String, RawMcpServer>,
}

/// JSON shape of one server entry.
#[derive(Debug, Deserialize)]
struct RawMcpServer {
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    env: Option<BTreeMap<String, String>>,
    /// Present for remote servers; recognized so the diagnostic can be specific.
    #[serde(default)]
    url: Option<String>,
    /// Transport label used by some clients. Only `stdio` is supported.
    #[serde(default, rename = "type")]
    transport: Option<String>,
    #[serde(default)]
    disabled: bool,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    description: Option<String>,
    /// Parsed after shape validation so one bad entry does not skip the file.
    #[serde(default)]
    exposure: Option<String>,
}

/// Discover and merge MCP server definitions.
///
/// Auto-discovered files that are unreadable or malformed produce warnings and
/// are skipped. A file named with `--mcp-config` is an explicit input, so a
/// missing, unreadable, or malformed file returns [`McpConfigError`].
///
/// `project_bytes` is the exact `.cake/mcp.json` content read by the trust
/// snapshot, so the bytes that were hashed are the bytes that are parsed.
///
/// # Errors
///
/// Returns [`McpConfigError`] when an explicit file cannot be used.
pub fn discover_mcp_servers(
    explicit: &[PathBuf],
    base_dir: &Path,
    config_dir: &Path,
    project_dir: &Path,
    trust: &ProjectTrustReport,
    mode: ProjectConfigMode,
    project_bytes: Option<&[u8]>,
) -> Result<McpConfigLoad, McpConfigError> {
    let mut merger = McpMerger::new();
    merger.read(&global_config_path(config_dir), false)?;
    load_project_source(
        &mut merger,
        &project_dir.join(PROJECT_MCP_CONFIG),
        trust,
        mode,
        project_bytes,
    );
    for path in explicit {
        merger.read(&resolve_explicit(path, base_dir), true)?;
    }
    Ok(merger.finish())
}

/// The user-owned global MCP configuration file beneath the config root.
#[must_use]
pub fn global_config_path(config_dir: &Path) -> PathBuf {
    config_dir.join("cake").join(MCP_CONFIG_FILE)
}

/// Resolve an explicit config path against the invocation directory.
fn resolve_explicit(path: &Path, base_dir: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

/// Add the trusted project file, or report why it was skipped.
fn load_project_source(
    merger: &mut McpMerger,
    project: &Path,
    trust: &ProjectTrustReport,
    mode: ProjectConfigMode,
    bytes: Option<&[u8]>,
) {
    if !mode.loads_project_config() {
        return;
    }
    if !trust.allows_authority() {
        if project.exists() {
            merger.warnings.push(format!(
                "ignoring {}: project configuration is not trusted; run `cake trust`",
                project.display()
            ));
        }
        return;
    }
    let Some(bytes) = bytes else {
        return;
    };
    match std::str::from_utf8(bytes) {
        Ok(text) => {
            if let Err(error) = merger.parse(project, text, false) {
                merger.warnings.push(error.to_string());
            }
        },
        Err(error) => merger.warnings.push(format!(
            "ignoring {}: file is not valid UTF-8: {error}",
            project.display()
        )),
    }
}

/// Accumulates servers from every source with later names replacing earlier.
#[derive(Default)]
struct McpMerger {
    servers: Vec<McpServerConfig>,
    positions: HashMap<String, usize>,
    warnings: Vec<String>,
}

impl McpMerger {
    fn new() -> Self {
        Self::default()
    }

    /// Read one file and merge its servers.
    fn read(&mut self, path: &Path, explicit: bool) -> Result<(), McpConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !explicit => {
                return Ok(());
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(McpConfigError::Missing {
                    path: path.to_path_buf(),
                });
            },
            Err(error) if explicit => {
                return Err(McpConfigError::Read {
                    path: path.to_path_buf(),
                    source: error,
                });
            },
            Err(error) => {
                self.warnings
                    .push(format!("ignoring MCP config {}: {error}", path.display()));
                return Ok(());
            },
        };
        self.parse(path, &text, explicit)
    }

    /// Parse one file's text and merge its servers.
    fn parse(&mut self, path: &Path, text: &str, explicit: bool) -> Result<(), McpConfigError> {
        let raw = match parse_mcp_file(text) {
            Ok(raw) => raw,
            Err(message) => {
                if explicit {
                    return Err(McpConfigError::Invalid {
                        path: path.to_path_buf(),
                        message,
                    });
                }
                self.warnings
                    .push(format!("ignoring MCP config {}: {message}", path.display()));
                return Ok(());
            },
        };
        for (name, server) in raw.mcp_servers {
            let Some(config) = resolve_server(&name, server, path, &mut self.warnings) else {
                continue;
            };
            self.merge(config);
        }
        Ok(())
    }

    fn merge(&mut self, config: McpServerConfig) {
        if let Some(index) = self.positions.get(&config.name) {
            self.servers[*index] = config;
        } else {
            self.positions
                .insert(config.name.clone(), self.servers.len());
            self.servers.push(config);
        }
    }

    fn finish(mut self) -> McpConfigLoad {
        self.servers
            .sort_by(|left, right| left.name.cmp(&right.name));
        McpConfigLoad {
            servers: self.servers,
            warnings: self.warnings,
        }
    }
}

/// Parse and shape-check one config file.
fn parse_mcp_file(text: &str) -> Result<RawMcpFile, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|error| format!("invalid JSON: {error}"))?;
    if !value.is_object() {
        return Err("expected a JSON object with an 'mcpServers' member".to_string());
    }
    serde_json::from_value(value).map_err(|error| format!("invalid mcpServers shape: {error}"))
}

/// Remove explicit config paths that live inside any checkout's `.cake/` tree.
///
/// Used only for `--no-project-config`: an explicit path must not re-enable a
/// project's MCP server definitions. Both the lexical and canonical paths are
/// checked so a symlinked `.cake` path is still filtered.
#[must_use]
pub fn filter_project_mcp_paths(paths: Vec<PathBuf>, project_roots: &[&Path]) -> Vec<PathBuf> {
    let cake_roots: Vec<PathBuf> = project_roots
        .iter()
        .map(|root| root.join(".cake"))
        .collect();
    let canonical_roots: Vec<PathBuf> = cake_roots
        .iter()
        .map(|root| std::fs::canonicalize(root).unwrap_or_else(|_| root.clone()))
        .collect();
    paths
        .into_iter()
        .filter(|path| {
            let lexical_hit = cake_roots.iter().any(|root| path.starts_with(root));
            let identity = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
            let canonical_hit = canonical_roots
                .iter()
                .any(|root| identity.starts_with(root));
            !lexical_hit && !canonical_hit
        })
        .collect()
}

/// Validate one raw entry and turn it into a resolved server definition.
fn resolve_server(
    name: &str,
    raw: RawMcpServer,
    source: &Path,
    warnings: &mut Vec<String>,
) -> Option<McpServerConfig> {
    if raw.disabled {
        return None;
    }
    if !valid_server_name(name) {
        warnings.push(format!(
            "ignoring MCP server '{name}' in {}: names must be 1-{MAX_SERVER_NAME_LEN} \
             characters of ASCII letters, digits, '_' or '-'",
            source.display()
        ));
        return None;
    }
    let exposure = parse_exposure(name, raw.exposure.as_deref(), source, warnings)?;
    let description = parse_description(name, raw.description.as_deref(), source, warnings).ok()?;
    if raw.url.is_some() {
        warnings.push(format!(
            "ignoring MCP server '{name}' in {}: remote HTTP servers are not supported; \
             only stdio 'command' servers are available",
            source.display()
        ));
        return None;
    }
    if let Some(transport) = raw.transport.as_deref()
        && transport != "stdio"
    {
        warnings.push(format!(
            "ignoring MCP server '{name}' in {}: transport '{transport}' is not supported; \
             only stdio is available",
            source.display()
        ));
        return None;
    }
    let (command, args) = parse_command(name, raw.command.as_deref(), raw.args, source, warnings)?;
    let env = parse_env(name, raw.env, source, warnings)?;
    Some(build_server(
        name,
        &command,
        args,
        env,
        description,
        exposure,
        raw.timeout_secs,
        source,
    ))
}

/// Parse the optional exposure label.
fn parse_exposure(
    name: &str,
    value: Option<&str>,
    source: &Path,
    warnings: &mut Vec<String>,
) -> Option<McpExposure> {
    let Some(value) = value else {
        return Some(McpExposure::default());
    };
    if let Some(exposure) = McpExposure::parse(value) {
        return Some(exposure);
    }
    warnings.push(format!(
        "ignoring MCP server '{name}' in {}: exposure must be 'direct', 'deferred', or 'hidden'",
        source.display()
    ));
    None
}

/// Parse the optional one-line description.
fn parse_description(
    name: &str,
    value: Option<&str>,
    source: &Path,
    warnings: &mut Vec<String>,
) -> Result<Option<String>, ()> {
    match value.map(str::trim) {
        Some(description) if description.contains('\0') => {
            warnings.push(format!(
                "ignoring MCP server '{name}' in {}: description may not contain a NUL byte",
                source.display()
            ));
            Err(())
        },
        Some(description) if !description.is_empty() => Ok(Some(description.to_string())),
        _ => Ok(None),
    }
}

/// Validate the executable and its arguments.
fn parse_command(
    name: &str,
    command: Option<&str>,
    args: Option<Vec<String>>,
    source: &Path,
    warnings: &mut Vec<String>,
) -> Option<(String, Vec<String>)> {
    let Some(command) = command.map(str::trim) else {
        warnings.push(format!(
            "ignoring MCP server '{name}' in {}: 'command' is required",
            source.display()
        ));
        return None;
    };
    if command.is_empty() || command.contains('\0') {
        warnings.push(format!(
            "ignoring MCP server '{name}' in {}: 'command' must be a non-empty executable",
            source.display()
        ));
        return None;
    }
    let args = args.unwrap_or_default();
    if args.iter().any(|arg| arg.contains('\0')) {
        warnings.push(format!(
            "ignoring MCP server '{name}' in {}: arguments may not contain NUL bytes",
            source.display()
        ));
        return None;
    }
    Some((command.to_string(), args))
}

/// Validate the optional environment map.
fn parse_env(
    name: &str,
    env: Option<BTreeMap<String, String>>,
    source: &Path,
    warnings: &mut Vec<String>,
) -> Option<BTreeMap<String, String>> {
    let Some(env) = env else {
        return Some(BTreeMap::new());
    };
    if let Some((key, _)) = env
        .iter()
        .find(|(key, value)| !valid_env_name(key) || value.contains('\0'))
    {
        warnings.push(format!(
            "ignoring MCP server '{name}' in {}: environment entry '{key}' is not a valid \
             name or contains a NUL byte",
            source.display()
        ));
        return None;
    }
    Some(env)
}

/// Apply timeout defaults and clamping.
#[expect(
    clippy::too_many_arguments,
    reason = "one constructor for the resolved server shape"
)]
fn build_server(
    name: &str,
    command: &str,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    description: Option<String>,
    exposure: McpExposure,
    timeout_secs: Option<u64>,
    source: &Path,
) -> McpServerConfig {
    McpServerConfig {
        name: name.to_string(),
        command: command.to_string(),
        args,
        env,
        timeout_secs: timeout_secs
            .unwrap_or(DEFAULT_MCP_TIMEOUT_SECS)
            .clamp(1, MAX_MCP_TIMEOUT_SECS),
        description,
        exposure,
        source: source.to_path_buf(),
    }
}

/// Server names become part of a provider function name, so keep them to the
/// characters every provider accepts.
pub fn valid_server_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_SERVER_NAME_LEN
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// Environment variable names follow the portable POSIX convention.
pub fn valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Build the registered tool name for one server/tool pair.
///
/// The result fits the 64-character provider limit. Characters outside
/// `[A-Za-z0-9_-]` become `_`; a name that would exceed the limit is truncated
/// and given a deterministic eight-hex-character digest suffix so two long
/// names cannot collapse into one.
#[must_use]
pub fn registered_tool_name(server: &str, tool: &str) -> Option<String> {
    let server = sanitize_component(server);
    let tool = sanitize_component(tool);
    if !has_alphanumeric(&server) || !has_alphanumeric(&tool) {
        return None;
    }
    let candidate = format!("{MCP_TOOL_PREFIX}{server}__{tool}");
    if candidate.len() <= MAX_REGISTERED_NAME_LEN {
        return Some(candidate);
    }

    let digest = sha256_hex(candidate.as_bytes());
    let digest = digest.get(..8).unwrap_or(digest.as_str());
    let fixed_len = MCP_TOOL_PREFIX.len() + server.len() + 2 + TRUNCATION_SUFFIX_LEN;
    let keep = MAX_REGISTERED_NAME_LEN.saturating_sub(fixed_len);
    let truncated: String = tool.chars().take(keep.max(1)).collect();
    Some(format!("{MCP_TOOL_PREFIX}{server}__{truncated}__{digest}"))
}

/// Replace every character that a provider function name rejects.
fn sanitize_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// A name made only of punctuation is not a usable tool or server name.
fn has_alphanumeric(value: &str) -> bool {
    value.chars().any(|ch| ch.is_ascii_alphanumeric())
}

/// Lowercase hex SHA-256 digest.
fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// One stdio server definition written by `cake mcp add`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServerDraft {
    /// Local instance name.
    pub name: String,
    /// Executable to spawn.
    pub command: String,
    /// Arguments passed to the executable.
    pub args: Vec<String>,
    /// Additional environment variables, by name.
    pub env: BTreeMap<String, String>,
    /// Per-request timeout in seconds.
    pub timeout_secs: u64,
    /// Optional one-line description shown in the MCP server summary.
    pub description: Option<String>,
    /// How the server's tools reach the model.
    pub exposure: McpExposure,
}

/// A failure while reading or editing MCP server definitions.
#[derive(Debug, thiserror::Error)]
pub enum McpManageError {
    /// The server name is not a usable provider tool prefix.
    #[error(
        "invalid MCP server name '{name}': names must be 1-{MAX_SERVER_NAME_LEN} characters \
         of ASCII letters, digits, '_' or '-'"
    )]
    InvalidName {
        /// The rejected name.
        name: String,
    },
    /// The command is missing or contains a NUL byte.
    #[error("invalid MCP command for '{name}': provide a non-empty executable after `--`")]
    InvalidCommand {
        /// The server the command belongs to.
        name: String,
    },
    /// The description contains a NUL byte.
    #[error("invalid MCP description for '{name}': description may not contain a NUL byte")]
    InvalidDescription {
        /// The server the description belongs to.
        name: String,
    },
    /// An environment variable name is not a portable name.
    #[error("invalid environment variable name '{key}' for MCP server '{name}'")]
    InvalidEnv {
        /// The server the environment entry belongs to.
        name: String,
        /// The rejected variable name.
        key: String,
    },
    /// The requested timeout is outside the supported range.
    #[error("invalid MCP timeout {seconds}s: use a value from 1 to {MAX_MCP_TIMEOUT_SECS}")]
    InvalidTimeout {
        /// The rejected timeout.
        seconds: u64,
    },
    /// The requested transport is a remote HTTP server, which Cake does not
    /// implement.
    #[error(
        "remote MCP server '{name}' is not supported: Cake implements stdio servers only; \
         omit --url and provide the executable after `--`"
    )]
    UnsupportedUrl {
        /// The server the URL belongs to.
        name: String,
    },
    /// The file exists but is not the documented `mcpServers` shape.
    #[error("MCP config file {path} is invalid: {message}")]
    Invalid {
        /// The file that could not be used.
        path: PathBuf,
        /// The parser or shape diagnostic.
        message: String,
    },
    /// The file could not be read.
    #[error("MCP config file {path} could not be read: {source}")]
    Read {
        /// The file that could not be read.
        path: PathBuf,
        /// The underlying read failure.
        #[source]
        source: std::io::Error,
    },
    /// The file could not be written.
    #[error("MCP config file {path} could not be written: {source}")]
    Write {
        /// The file that could not be written.
        path: PathBuf,
        /// The underlying write failure.
        #[source]
        source: std::io::Error,
    },
    /// The named server is not defined by the resolved sources.
    #[error("MCP server '{name}' is not configured for this directory")]
    NotFound {
        /// The name the command looked for.
        name: String,
    },
}

impl McpManageError {
    /// The process exit code that accompanies this failure.
    pub const fn exit_code(&self) -> u8 {
        use crate::exit_code::code;
        match self {
            Self::InvalidName { .. }
            | Self::InvalidCommand { .. }
            | Self::InvalidDescription { .. }
            | Self::InvalidEnv { .. }
            | Self::InvalidTimeout { .. }
            | Self::UnsupportedUrl { .. }
            | Self::Invalid { .. }
            | Self::NotFound { .. } => code::INPUT_ERROR,
            Self::Read { .. } | Self::Write { .. } => code::AGENT_ERROR,
        }
    }
}

/// Add one stdio server to the global file, replacing an entry with the same
/// name and preserving every other top-level key and server entry.
///
/// # Errors
///
/// Returns [`McpManageError`] for an invalid definition, an unreadable or
/// malformed existing file, or a failed write.
pub fn upsert_server(path: &Path, draft: &McpServerDraft) -> Result<(), McpManageError> {
    validate_draft(draft)?;
    let mut document = read_document(path)?;
    let servers = servers_mut(&mut document, path)?;
    servers.insert(draft.name.clone(), server_entry(draft));
    write_document(path, &document)
}

/// Remove one server from the global file. Returns whether it was present.
///
/// A file that does not exist and a name that is absent are both `Ok(false)`,
/// and an absent name does not rewrite the file.
///
/// # Errors
///
/// Returns [`McpManageError`] for an invalid name, an unreadable or malformed
/// existing file, or a failed write.
pub fn remove_server(path: &Path, name: &str) -> Result<bool, McpManageError> {
    if !valid_server_name(name) {
        return Err(McpManageError::InvalidName {
            name: name.to_string(),
        });
    }
    let mut document = read_document(path)?;
    let servers = servers_mut(&mut document, path)?;
    let removed = servers.remove(name).is_some();
    if removed {
        write_document(path, &document)?;
    }
    Ok(removed)
}

/// Validate every field that becomes a provider-visible server definition.
fn validate_draft(draft: &McpServerDraft) -> Result<(), McpManageError> {
    if !valid_server_name(&draft.name) {
        return Err(McpManageError::InvalidName {
            name: draft.name.clone(),
        });
    }
    if draft.command.trim().is_empty() || draft.command.contains('\0') {
        return Err(McpManageError::InvalidCommand {
            name: draft.name.clone(),
        });
    }
    if draft
        .description
        .as_deref()
        .is_some_and(|description| description.contains('\0'))
    {
        return Err(McpManageError::InvalidDescription {
            name: draft.name.clone(),
        });
    }
    if let Some(key) = draft.env.keys().find(|key| !valid_env_name(key)) {
        return Err(McpManageError::InvalidEnv {
            name: draft.name.clone(),
            key: key.clone(),
        });
    }
    if draft.timeout_secs == 0 || draft.timeout_secs > MAX_MCP_TIMEOUT_SECS {
        return Err(McpManageError::InvalidTimeout {
            seconds: draft.timeout_secs,
        });
    }
    Ok(())
}

/// Read the file as a JSON object with an object-valued `mcpServers` member.
fn read_document(path: &Path) -> Result<serde_json::Value, McpManageError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(serde_json::json!({ "mcpServers": {} }));
        },
        Err(source) => {
            return Err(McpManageError::Read {
                path: path.to_path_buf(),
                source,
            });
        },
    };
    let mut document: serde_json::Value =
        serde_json::from_str(&text).map_err(|error| McpManageError::Invalid {
            path: path.to_path_buf(),
            message: format!("invalid JSON: {error}"),
        })?;
    let Some(root) = document.as_object_mut() else {
        return Err(McpManageError::Invalid {
            path: path.to_path_buf(),
            message: "expected a JSON object with an 'mcpServers' member".to_string(),
        });
    };
    match root.get_mut("mcpServers") {
        Some(serde_json::Value::Object(_)) => {},
        Some(_) => {
            return Err(McpManageError::Invalid {
                path: path.to_path_buf(),
                message: "'mcpServers' must be a JSON object".to_string(),
            });
        },
        None => {
            root.insert(
                "mcpServers".to_string(),
                serde_json::Value::Object(serde_json::Map::new()),
            );
        },
    }
    Ok(document)
}

/// Borrow the `mcpServers` object after [`read_document`] guaranteed it.
fn servers_mut<'a>(
    document: &'a mut serde_json::Value,
    path: &Path,
) -> Result<&'a mut serde_json::Map<String, serde_json::Value>, McpManageError> {
    document
        .get_mut("mcpServers")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| McpManageError::Invalid {
            path: path.to_path_buf(),
            message: "'mcpServers' must be a JSON object".to_string(),
        })
}

/// Build the JSON entry for one draft, omitting defaults.
fn server_entry(draft: &McpServerDraft) -> serde_json::Value {
    let mut entry = serde_json::Map::new();
    entry.insert(
        "command".to_string(),
        serde_json::Value::String(draft.command.clone()),
    );
    if !draft.args.is_empty() {
        entry.insert("args".to_string(), serde_json::json!(draft.args));
    }
    if !draft.env.is_empty() {
        entry.insert("env".to_string(), serde_json::json!(draft.env));
    }
    if draft.timeout_secs != DEFAULT_MCP_TIMEOUT_SECS {
        entry.insert(
            "timeout_secs".to_string(),
            serde_json::json!(draft.timeout_secs),
        );
    }
    if let Some(description) = draft
        .description
        .as_deref()
        .map(str::trim)
        .filter(|description| !description.is_empty())
    {
        entry.insert(
            "description".to_string(),
            serde_json::Value::String(description.to_string()),
        );
    }
    if draft.exposure != McpExposure::default() {
        entry.insert(
            "exposure".to_string(),
            serde_json::Value::String(draft.exposure.as_str().to_string()),
        );
    }
    serde_json::Value::Object(entry)
}

/// Serialize the document and replace the file atomically with owner-only
/// permissions. A symlinked destination is written through its target so a
/// dotfiles setup keeps the link.
fn write_document(path: &Path, document: &serde_json::Value) -> Result<(), McpManageError> {
    let target = write_target(path)?;
    let Some(parent) = target.parent() else {
        return Err(McpManageError::Write {
            path: target,
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the config path has no parent directory",
            ),
        });
    };
    fs::create_dir_all(parent).map_err(|source| McpManageError::Write {
        path: target.clone(),
        source,
    })?;
    let mut bytes = serde_json::to_vec_pretty(document).map_err(|error| McpManageError::Write {
        path: target.clone(),
        source: std::io::Error::other(error),
    })?;
    bytes.push(b'\n');
    let temp = temp_path(&target, parent);
    let write_result = write_temp_file(&temp, &bytes).and_then(|()| {
        fs::rename(&temp, &target).map_err(|source| McpManageError::Write {
            path: target.clone(),
            source,
        })
    });
    if write_result.is_err() {
        drop(fs::remove_file(&temp));
    }
    write_result
}

/// Resolve a symlinked destination so the write keeps the link in place.
fn write_target(path: &Path) -> Result<PathBuf, McpManageError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            fs::canonicalize(path).map_err(|source| McpManageError::Write {
                path: path.to_path_buf(),
                source,
            })
        },
        _ => Ok(path.to_path_buf()),
    }
}

/// Build a unique sibling path for the temporary file.
fn temp_path(path: &Path, parent: &Path) -> PathBuf {
    parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .map_or("mcp", |name| name.to_str().unwrap_or("mcp")),
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    ))
}

/// Create the temporary file with owner-only permissions and flush it.
fn write_temp_file(path: &Path, bytes: &[u8]) -> Result<(), McpManageError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).map_err(|source| McpManageError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| McpManageError::Write {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod mcp_tests;
