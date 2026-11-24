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
use crate::clients::tools::{Tool, ToolEntry, ToolError, ToolResult, repair_json_args};

/// Registered name of the search tool.
pub const TOOL_SEARCH_NAME: &str = "ToolSearch";

/// Default number of matches returned to the model.
const DEFAULT_LIMIT: usize = 5;

/// Maximum number of matches returned to the model.
const MAX_LIMIT: usize = 20;

/// Bound on one result description.
const MAX_RESULT_DESCRIPTION_CHARS: usize = 160;

/// BM25 term-frequency saturation.
const BM25_K1: f64 = 1.2;

/// BM25 length-normalization factor.
const BM25_B: f64 = 0.75;

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
                concise_description(&entry.description)
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

fn concise_description(description: &str) -> String {
    let one_line = description.split_whitespace().collect::<Vec<_>>().join(" ");
    let first_sentence = one_line.split('.').next().unwrap_or(&one_line).trim();
    let mut bounded: String = first_sentence
        .chars()
        .take(MAX_RESULT_DESCRIPTION_CHARS)
        .collect();
    if first_sentence.chars().count() > MAX_RESULT_DESCRIPTION_CHARS {
        bounded.push_str("...");
    }
    bounded
}

/// Rank entries for a query, optionally restricted to one server.
#[expect(
    clippy::cast_precision_loss,
    reason = "the deferred MCP catalog is small enough that f64 represents its sizes exactly"
)]
fn search<'a>(
    entries: &'a [ToolSearchEntry],
    query: &str,
    server: Option<&str>,
    limit: usize,
) -> Vec<&'a ToolSearchEntry> {
    let query_terms = tokens(query);
    if query_terms.is_empty() {
        return Vec::new();
    }
    let candidates: Vec<(&ToolSearchEntry, Vec<String>)> = entries
        .iter()
        .filter(|entry| {
            server.is_none_or(|server| entry.server.eq_ignore_ascii_case(server.trim()))
        })
        .map(|entry| (entry, document_tokens(entry)))
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    let average_length = candidates
        .iter()
        .map(|(_, tokens)| tokens.len())
        .sum::<usize>() as f64
        / candidates.len() as f64;
    let document_count = candidates.len() as f64;
    let document_frequency = term_document_frequency(&candidates);
    let mut scored: Vec<(f64, &ToolSearchEntry)> = candidates
        .into_iter()
        .filter_map(|(entry, document)| {
            let score = bm25_score(
                &query_terms,
                &document,
                &document_frequency,
                average_length,
                document_count,
            ) + exact_name_bonus(entry, query);
            (score > 0.0).then_some((score, entry))
        })
        .collect();
    scored.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.1.registered_name.cmp(&right.1.registered_name))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, entry)| entry)
        .collect()
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

fn term_document_frequency(
    documents: &[(&ToolSearchEntry, Vec<String>)],
) -> std::collections::HashMap<String, usize> {
    let mut frequencies = std::collections::HashMap::new();
    for (_, document) in documents {
        let unique: HashSet<&String> = document.iter().collect();
        for term in unique {
            *frequencies.entry(term.clone()).or_insert(0) += 1;
        }
    }
    frequencies
}

#[expect(
    clippy::cast_precision_loss,
    clippy::suboptimal_flops,
    reason = "BM25 is a ranking heuristic over a small catalog; clarity wins over fused operations"
)]
fn bm25_score(
    query_terms: &[String],
    document: &[String],
    document_frequency: &std::collections::HashMap<String, usize>,
    average_length: f64,
    document_count: f64,
) -> f64 {
    if document.is_empty() || average_length <= 0.0 {
        return 0.0;
    }
    let mut score = 0.0;
    for term in query_terms {
        let frequency = document.iter().filter(|token| *token == term).count() as f64;
        if frequency == 0.0 {
            continue;
        }
        let frequency_in_documents = document_frequency.get(term).copied().unwrap_or(0) as f64;
        if frequency_in_documents == 0.0 {
            continue;
        }
        let idf = ((document_count - frequency_in_documents + 0.5)
            / (frequency_in_documents + 0.5))
            .ln_1p();
        let length_ratio = document.len() as f64 / average_length;
        let denominator = frequency + BM25_K1 * (1.0 - BM25_B + BM25_B * length_ratio);
        score += idf * (frequency * (BM25_K1 + 1.0)) / denominator;
    }
    score
}

fn document_tokens(entry: &ToolSearchEntry) -> Vec<String> {
    let mut tokens = Vec::new();
    for _ in 0..2 {
        tokens.extend(tokens_of(&entry.registered_name));
    }
    tokens.extend(tokens_of(&entry.server));
    tokens.extend(tokens_of(&entry.description));
    tokens.extend(tokens_of(&entry.input_schema.to_string()));
    tokens
}

fn tokens(value: &str) -> Vec<String> {
    tokens_of(value)
        .into_iter()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect()
}

fn tokens_of(value: &str) -> Vec<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}
