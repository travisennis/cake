//! Lexical search over discovered skill metadata.
//!
//! The prompt advertises a bounded skill catalog. Search keeps every discovered
//! skill reachable without sending every routing description on every request:
//! the model searches names and descriptions, then reads the returned
//! `SKILL.md` location through the normal reader.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::PathBuf;

use serde::Deserialize;

use crate::clients::tools::{Tool, ToolEntry, ToolError, ToolResult, repair_json_args};

/// Registered name of the skill search tool.
pub const SKILL_SEARCH_NAME: &str = "SkillSearch";

/// Default number of matches returned to the model.
const DEFAULT_LIMIT: usize = 5;

/// Maximum number of matches returned to the model.
const MAX_LIMIT: usize = 20;

/// Bound on one returned description.
const MAX_RESULT_DESCRIPTION_CHARS: usize = 200;

/// Bound on the complete tool result.
const MAX_RESULT_CHARS: usize = 8_000;

/// BM25 term-frequency saturation.
const BM25_K1: f64 = 1.2;

/// BM25 length-normalization factor.
const BM25_B: f64 = 0.75;

/// One discovered skill visible to search.
#[derive(Clone, Debug)]
pub struct SkillSearchEntry {
    /// Skill name from the frontmatter.
    pub name: String,
    /// Routing description from the frontmatter.
    pub description: String,
    /// Absolute path to the skill's `SKILL.md`.
    pub location: PathBuf,
}

/// Whether the exact-name allowlist permits `SkillSearch`.
#[must_use]
pub fn skill_search_enabled(enabled_tools: Option<&[String]>) -> bool {
    enabled_tools.is_none_or(|names| names.iter().any(|name| name == SKILL_SEARCH_NAME))
}

/// Build the `SkillSearch` registry entry.
pub fn skill_search_entry(entries: Vec<SkillSearchEntry>) -> ToolEntry {
    let definition = Tool {
        type_: "function".to_string(),
        name: SKILL_SEARCH_NAME.to_string(),
        description: "Search the full skill catalog by name and description. Use this when the \
                      skill catalog reports omitted skills, or when you need to find a skill that \
                      is not listed. Returns the skill name, a short description, and the \
                      SKILL.md location to read."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "query": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Words describing the capability or skill you need, such as 'browser tabs' or 'release checklist'."
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
        Box::pin(async move { execute(&entries, &arguments) })
    })
    .tolerates_intent()
    .read_safe()
    .repairs_arguments()
}

#[derive(Deserialize)]
struct SearchArguments {
    query: String,
    #[serde(default)]
    limit: Option<usize>,
}

/// Execute one search over the discovered catalog.
fn execute(entries: &[SkillSearchEntry], arguments: &str) -> Result<ToolResult, ToolError> {
    let args: SearchArguments = serde_json::from_str(arguments)
        .or_else(|_| serde_json::from_str(&repair_json_args(arguments)))
        .map_err(|error| ToolError::new(format!("Invalid SkillSearch arguments: {error}")))?;
    if args.query.trim().is_empty() {
        return Err(ToolError::new(
            "Invalid SkillSearch arguments: query must not be empty",
        ));
    }
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let matches = search(entries, &args.query, limit);
    Ok(text_result(render_matches(&matches, &args.query)))
}

const fn text_result(output: String) -> ToolResult {
    ToolResult {
        output,
        images: Vec::new(),
        compensation_events: Vec::new(),
        permission_denials: Vec::new(),
    }
}

fn render_matches(matches: &[&SkillSearchEntry], query: &str) -> String {
    if matches.is_empty() {
        return format!("No skills matched '{query}'.");
    }

    let mut output = String::new();
    if matches.len() == 1 {
        _ = writeln!(output, "Found 1 skill matching '{query}':");
    } else {
        _ = writeln!(output, "Found {} skills matching '{query}':", matches.len());
    }
    for entry in matches {
        let line = format!(
            "- {}: {}\n  location: {}\n",
            entry.name,
            concise_description(&entry.description),
            entry.location.display()
        );
        if !append_bounded(&mut output, &line) {
            break;
        }
    }
    output
}

/// Append as much of `line` as fits in the tool result budget.
fn append_bounded(output: &mut String, line: &str) -> bool {
    let remaining = MAX_RESULT_CHARS.saturating_sub(output.chars().count());
    if remaining == 0 {
        return false;
    }
    let line_characters = line.chars().count();
    if line_characters <= remaining {
        output.push_str(line);
        return true;
    }
    if remaining > 3 {
        output.extend(line.chars().take(remaining - 3));
        output.push_str("...");
    } else {
        output.extend(line.chars().take(remaining));
    }
    false
}

fn concise_description(description: &str) -> String {
    let one_line = description.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut bounded: String = one_line
        .chars()
        .take(MAX_RESULT_DESCRIPTION_CHARS)
        .collect();
    if one_line.chars().count() > MAX_RESULT_DESCRIPTION_CHARS {
        bounded.push_str("...");
    }
    bounded
}

/// Rank entries for a query.
#[expect(
    clippy::cast_precision_loss,
    reason = "the skill catalog is small enough that f64 represents its sizes exactly"
)]
fn search<'a>(
    entries: &'a [SkillSearchEntry],
    query: &str,
    limit: usize,
) -> Vec<&'a SkillSearchEntry> {
    let query_terms = tokens(query);
    if query_terms.is_empty() {
        return Vec::new();
    }
    let candidates: Vec<(&SkillSearchEntry, Vec<String>)> = entries
        .iter()
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
    let mut scored: Vec<(f64, &SkillSearchEntry)> = candidates
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
            .then_with(|| left.1.name.cmp(&right.1.name))
            .then_with(|| left.1.location.cmp(&right.1.location))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, entry)| entry)
        .collect()
}

fn exact_name_bonus(entry: &SkillSearchEntry, query: &str) -> f64 {
    if query.trim().eq_ignore_ascii_case(&entry.name) {
        1_000.0
    } else {
        0.0
    }
}

fn term_document_frequency(
    documents: &[(&SkillSearchEntry, Vec<String>)],
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

fn document_tokens(entry: &SkillSearchEntry) -> Vec<String> {
    let mut tokens = Vec::new();
    for _ in 0..2 {
        tokens.extend(tokens_of(&entry.name));
    }
    tokens.extend(tokens_of(&entry.description));
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
        .map(str::to_lowercase)
        .collect()
}
