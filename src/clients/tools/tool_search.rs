//! On-demand search for MCP tools that are not registered yet.
//!
//! The executor ranks a small in-memory catalog, returns the matching tool
//! names and descriptions to the model, and records the selected registered
//! names for the agent to add to its registry before the next turn.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde_json::Value;

use crate::clients::mcp::LazyMcpRuntime;
use crate::clients::tools::search_rank::{CaseFold, rank, tokens_of};
use crate::clients::tools::{Tool, ToolEntry, ToolError, ToolResult, repair_json_args};
use crate::text::bounded_description;

/// Registered name of the search tool.
pub const TOOL_SEARCH_NAME: &str = "ToolSearch";

/// Default number of matches returned to the model.
const DEFAULT_LIMIT: usize = 5;

/// Maximum number of matches returned to the model.
const MAX_LIMIT: usize = 20;

/// Bound on one result description.
const MAX_RESULT_DESCRIPTION_CHARS: usize = 160;

/// One deferred tool visible to search.
#[derive(Clone, Debug)]
pub struct ToolSearchEntry {
    /// Local server name.
    pub(crate) server: String,
    /// Provider-safe registered name.
    pub(crate) registered_name: String,
    /// Description advertised by the server.
    pub(crate) description: String,
    /// Argument schema, searched for names and descriptions.
    pub(crate) input_schema: Value,
}

/// Static deferred tools plus the servers that connect on demand.
#[derive(Clone)]
pub struct ToolSearchCatalog {
    entries: Vec<ToolSearchEntry>,
    lazy: Option<Arc<LazyMcpRuntime>>,
}

impl ToolSearchCatalog {
    /// Build a catalog from already-connected tools and lazy servers.
    #[must_use]
    pub const fn new(entries: Vec<ToolSearchEntry>, lazy: Option<Arc<LazyMcpRuntime>>) -> Self {
        Self { entries, lazy }
    }

    /// Every server a search can reach, sorted for a stable message.
    fn server_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .entries
            .iter()
            .map(|entry| entry.server.clone())
            .collect();
        if let Some(lazy) = &self.lazy {
            names.extend(lazy.server_names().into_iter().map(str::to_string));
        }
        names.sort_unstable();
        names.dedup();
        names
    }

    async fn entries_for_search(
        &self,
        server: Option<&str>,
    ) -> (Vec<ToolSearchEntry>, Vec<String>) {
        let mut entries = self.entries.clone();
        let mut seen: HashSet<String> = entries
            .iter()
            .map(|entry| entry.registered_name.clone())
            .collect();
        let mut failed = Vec::new();
        if let Some(lazy) = &self.lazy {
            let outcome = lazy.connect(server).await;
            failed = outcome.failed;
            for connected in &outcome.servers {
                for tool in &connected.tools {
                    if !lazy.tool_enabled(&tool.registered_name)
                        || !seen.insert(tool.registered_name.clone())
                    {
                        continue;
                    }
                    entries.push(ToolSearchEntry {
                        server: connected.name.clone(),
                        registered_name: tool.registered_name.clone(),
                        description: tool.description.clone(),
                        input_schema: tool.input_schema.clone(),
                    });
                }
            }
        }
        (entries, failed)
    }
}

/// Whether the exact-name allowlist permits `ToolSearch`.
///
/// An absent allowlist permits it. An explicit allowlist must name it, which
/// lets the agent fall back to direct registration when the user selected only
/// specific MCP tools.
#[must_use]
pub fn tool_search_enabled(enabled_tools: Option<&[String]>) -> bool {
    enabled_tools.is_none_or(|names| names.iter().any(|name| name == TOOL_SEARCH_NAME))
}

/// Build the `ToolSearch` registry entry.
///
/// `active` is shared with the agent so a later search can report tools that
/// are already registered instead of claiming they will become available.
pub fn tool_search_entry(
    catalog: Arc<ToolSearchCatalog>,
    pending: Arc<Mutex<Vec<String>>>,
    active: Arc<Mutex<HashSet<String>>>,
) -> ToolEntry {
    let definition = Tool {
        type_: "function".to_string(),
        name: TOOL_SEARCH_NAME.to_string(),
        description: "Search MCP tools that are not loaded yet. Call this when a deferred MCP \
                      server in the prompt might provide the capability you need, or when a \
                      needed tool is not in the current tool list. Matching tools become \
                      available on the next turn."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "query": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Words describing the capability you need, such as 'list browser tabs' or 'create issue'."
                },
                "server": {
                    "type": "string",
                    "description": "Optional MCP server name to restrict the search."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_LIMIT,
                    "description": "Maximum number of matches to return. Defaults to 5."
                }
            },
            "required": ["query"]
        }),
    };
    ToolEntry::new(definition, move |_context, _call_id, arguments| {
        let catalog = Arc::clone(&catalog);
        let pending = Arc::clone(&pending);
        let active = Arc::clone(&active);
        Box::pin(async move { execute(&catalog, &pending, &active, &arguments).await })
    })
    .tolerates_intent()
    .read_safe()
    .repairs_arguments()
}

#[derive(Deserialize)]
struct SearchArguments {
    query: String,
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

/// Execute one search and record the selected names for activation.
async fn execute(
    catalog: &ToolSearchCatalog,
    pending: &Mutex<Vec<String>>,
    active: &Mutex<HashSet<String>>,
    arguments: &str,
) -> Result<ToolResult, ToolError> {
    let args: SearchArguments = serde_json::from_str(arguments)
        .or_else(|_| serde_json::from_str(&repair_json_args(arguments)))
        .map_err(|error| ToolError::new(format!("Invalid ToolSearch arguments: {error}")))?;
    if args.query.trim().is_empty() {
        return Err(ToolError::new(
            "Invalid ToolSearch arguments: query must not be empty",
        ));
    }
    let (entries, failed) = catalog.entries_for_search(args.server.as_deref()).await;
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let matches = search(&entries, &args.query, args.server.as_deref(), usize::MAX);
    if matches.is_empty() {
        return Ok(text_result(no_match_message(
            &catalog.server_names(),
            &failed,
            &args.query,
        )));
    }
    let active_names = active
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let (active_matches, new_matches): (Vec<_>, Vec<_>) = matches
        .into_iter()
        .partition(|entry| active_names.contains(&entry.registered_name));
    let new_matches: Vec<_> = new_matches.into_iter().take(limit).collect();
    pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .extend(
            new_matches
                .iter()
                .map(|entry| entry.registered_name.clone()),
        );
    let mut output = render_matches(&new_matches, &active_matches, &args.query);
    append_failures(&mut output, &failed);
    Ok(text_result(output))
}

const fn text_result(output: String) -> ToolResult {
    ToolResult {
        output,
        images: Vec::new(),
        compensation_events: Vec::new(),
        permission_denials: Vec::new(),
    }
}

fn render_matches(
    new_matches: &[&ToolSearchEntry],
    active_matches: &[&ToolSearchEntry],
    query: &str,
) -> String {
    let mut output = String::new();
    if new_matches.is_empty() {
        _ = write!(output, "No new MCP tools matched '{query}'.");
    } else {
        let (noun, availability) = if new_matches.len() == 1 {
            ("tool", "It is available on the next turn.")
        } else {
            ("tools", "They are available on the next turn.")
        };
        _ = writeln!(
            output,
            "Found {} new MCP {noun}. {availability}",
            new_matches.len()
        );
        for entry in new_matches {
            _ = writeln!(
                output,
                "- {} ({}): {}",
                entry.registered_name,
                entry.server,
                bounded_description(&entry.description, MAX_RESULT_DESCRIPTION_CHARS, true,)
                    .unwrap_or_default()
            );
        }
    }
    if !active_matches.is_empty() {
        output.push('\n');
        let (noun, verb) = if active_matches.len() == 1 {
            ("tool", "is")
        } else {
            ("tools", "are")
        };
        _ = writeln!(
            output,
            "{} matching MCP {noun} {verb} already active: {}.",
            active_matches.len(),
            active_names(active_matches)
        );
    }
    output
}

fn active_names(active_matches: &[&ToolSearchEntry]) -> String {
    let mut names = active_matches
        .iter()
        .take(MAX_LIMIT)
        .map(|entry| entry.registered_name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let omitted = active_matches.len().saturating_sub(MAX_LIMIT);
    if omitted > 0 {
        _ = write!(names, " (+{omitted} more)");
    }
    names
}

fn no_match_message(servers: &[String], failed: &[String], query: &str) -> String {
    if servers.is_empty() {
        return format!("No MCP tools matched '{query}'. No deferred MCP tools are available.");
    }
    let mut output = format!(
        "No MCP tools matched '{query}'. Deferred servers: {}.",
        servers.join(", ")
    );
    append_failures(&mut output, failed);
    output
}

fn append_failures(output: &mut String, failed: &[String]) {
    if failed.is_empty() {
        return;
    }
    let mut failed = failed.to_vec();
    failed.sort_unstable();
    failed.dedup();
    _ = write!(
        output,
        "\nDeferred MCP servers unavailable: {}.",
        failed.join(", ")
    );
}

fn search<'a>(
    entries: &'a [ToolSearchEntry],
    query: &str,
    server: Option<&str>,
    limit: usize,
) -> Vec<&'a ToolSearchEntry> {
    let candidates = entries
        .iter()
        .filter(|entry| {
            server.is_none_or(|server| entry.server.eq_ignore_ascii_case(server.trim()))
        })
        .map(|entry| (entry, document_tokens(entry)))
        .collect();
    rank(
        candidates,
        query,
        CaseFold::Ascii,
        limit,
        |entry| exact_name_bonus(entry, query),
        |left, right| left.registered_name.cmp(&right.registered_name),
    )
}

fn exact_name_bonus(entry: &ToolSearchEntry, query: &str) -> f64 {
    let query = query.trim().to_ascii_lowercase();
    if query == entry.registered_name.to_ascii_lowercase() {
        return 1_000.0;
    }
    let local_name = entry
        .registered_name
        .rsplit_once("__")
        .map_or(entry.registered_name.as_str(), |(_, local)| local);
    if query == local_name.to_ascii_lowercase() {
        500.0
    } else {
        0.0
    }
}

fn document_tokens(entry: &ToolSearchEntry) -> Vec<String> {
    let mut tokens = Vec::new();
    for _ in 0..2 {
        tokens.extend(tokens_of(&entry.registered_name, CaseFold::Ascii));
    }
    tokens.extend(tokens_of(&entry.server, CaseFold::Ascii));
    tokens.extend(tokens_of(&entry.description, CaseFold::Ascii));
    tokens.extend(tokens_of(&entry.input_schema.to_string(), CaseFold::Ascii));
    tokens
}
