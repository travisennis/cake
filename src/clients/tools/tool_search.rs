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
pub fn tool_search_entry(
    entries: Vec<ToolSearchEntry>,
    pending: Arc<Mutex<Vec<String>>>,
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
        let entries = entries.clone();
        let pending = Arc::clone(&pending);
        Box::pin(async move { execute(&entries, &pending, &arguments) })
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
fn execute(
    entries: &[ToolSearchEntry],
    pending: &Mutex<Vec<String>>,
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
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let matches = search(entries, &args.query, args.server.as_deref(), limit);
    if matches.is_empty() {
        return Ok(text_result(no_match_message(entries, &args.query)));
    }
    pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .extend(matches.iter().map(|entry| entry.registered_name.clone()));
    Ok(text_result(render_matches(&matches)))
}

const fn text_result(output: String) -> ToolResult {
    ToolResult {
        output,
        images: Vec::new(),
        compensation_events: Vec::new(),
        permission_denials: Vec::new(),
    }
}

fn render_matches(matches: &[&ToolSearchEntry]) -> String {
    let (noun, availability) = if matches.len() == 1 {
        ("tool", "It is available on the next turn.")
    } else {
        ("tools", "They are available on the next turn.")
    };
    let mut output = format!("Found {} MCP {noun}. {availability}\n", matches.len());
    for entry in matches {
        _ = writeln!(
            output,
            "- {} ({}): {}",
            entry.registered_name,
            entry.server,
            concise_description(&entry.description)
        );
    }
    output
}

fn no_match_message(entries: &[ToolSearchEntry], query: &str) -> String {
    let servers: Vec<&str> = entries
        .iter()
        .map(|entry| entry.server.as_str())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if servers.is_empty() {
        return format!("No MCP tools matched '{query}'. No deferred MCP tools are available.");
    }
    let mut servers = servers;
    servers.sort_unstable();
    format!(
        "No MCP tools matched '{query}'. Deferred servers: {}.",
        servers.join(", ")
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::tools::ToolContext;

    fn entry(name: &str, description: &str, schema: Value) -> ToolSearchEntry {
        ToolSearchEntry {
            server: "fixture".to_string(),
            registered_name: format!("mcp__fixture__{name}"),
            description: description.to_string(),
            input_schema: schema,
        }
    }

    fn fixture_entries() -> Vec<ToolSearchEntry> {
        vec![
            entry(
                "list_tabs",
                "List open browser tabs.",
                serde_json::json!({"type": "object", "properties": {}}),
            ),
            entry(
                "create_page",
                "Create a new page in the workspace.",
                serde_json::json!({"type": "object", "properties": {}}),
            ),
            entry(
                "search_history",
                "Search browsing history.",
                serde_json::json!({"type": "object", "properties": {}}),
            ),
        ]
    }

    #[test]
    fn ranks_a_name_match_first() {
        let entries = fixture_entries();
        let matches = search(&entries, "list tabs", None, 5);
        assert_eq!(matches[0].registered_name, "mcp__fixture__list_tabs");
    }

    #[test]
    fn matches_description_and_schema_text() {
        let entries = fixture_entries();
        assert_eq!(
            search(&entries, "browser", None, 5)[0].registered_name,
            "mcp__fixture__list_tabs"
        );
        let schema_entry = entry(
            "create_issue",
            "Create an issue.",
            serde_json::json!({"type": "object", "properties": {"title": {"type": "string"}}}),
        );
        let entries = vec![schema_entry];
        assert_eq!(
            search(&entries, "title", None, 5)[0].registered_name,
            "mcp__fixture__create_issue"
        );
    }

    #[test]
    fn filters_by_server() {
        let mut entries = fixture_entries();
        entries.push(ToolSearchEntry {
            server: "other".to_string(),
            registered_name: "mcp__other__list_tabs".to_string(),
            description: "List tabs on another server.".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        });
        let matches = search(&entries, "list tabs", Some("other"), 5);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].registered_name, "mcp__other__list_tabs");
    }

    #[test]
    fn honors_the_result_limit() {
        let entries = fixture_entries();
        assert_eq!(search(&entries, "search", None, 1).len(), 1);
    }

    #[test]
    fn exact_name_outranks_a_description_match() {
        let entries = vec![
            entry("read_file", "Read a file.", serde_json::json!({})),
            entry("other", "Use read_file when needed.", serde_json::json!({})),
        ];
        assert_eq!(
            search(&entries, "mcp__fixture__read_file", None, 5)[0].registered_name,
            "mcp__fixture__read_file"
        );
    }

    #[test]
    fn punctuation_only_query_returns_no_matches() {
        let entries = fixture_entries();
        assert!(search(&entries, "!!!", None, 5).is_empty());
    }

    #[test]
    fn allowlist_controls_search_availability() {
        assert!(tool_search_enabled(None));
        assert!(tool_search_enabled(Some(&["ToolSearch".to_string()])));
        assert!(!tool_search_enabled(Some(&["Read".to_string()])));
    }

    #[test]
    fn executor_returns_matches_and_records_activation() {
        let entries = fixture_entries();
        let pending = Arc::new(Mutex::new(Vec::new()));
        let entry = tool_search_entry(entries, Arc::clone(&pending));
        let context = Arc::new(ToolContext::with_temp_dirs(
            std::env::temp_dir(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ));
        let result = futures::executor::block_on((entry.execute)(
            context,
            "call-1".to_string(),
            r#"{"query":"list tabs"}"#.to_string(),
        ))
        .expect("search should succeed");
        assert!(result.output.contains("mcp__fixture__list_tabs"));
        assert_eq!(
            *pending.lock().unwrap(),
            vec!["mcp__fixture__list_tabs".to_string()]
        );
    }
}
