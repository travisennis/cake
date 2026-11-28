//! Lexical search over discovered skill metadata.
//!
//! The prompt advertises a bounded skill catalog. Search keeps every discovered
//! skill reachable without sending every routing description on every request:
//! the model searches names and descriptions, then reads the returned
//! `SKILL.md` location through the normal reader.

use std::fmt::Write as _;
use std::path::PathBuf;

use serde::Deserialize;

use crate::clients::tools::search_rank::{CaseFold, rank, tokens_of};
use crate::clients::tools::{Tool, ToolEntry, ToolError, ToolResult, repair_json_args};
use crate::text::{append_bounded, bounded_description};

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
            bounded_description(&entry.description, MAX_RESULT_DESCRIPTION_CHARS, false)
                .unwrap_or_default(),
            entry.location.display()
        );
        if !append_bounded(&mut output, &line, MAX_RESULT_CHARS) {
            break;
        }
    }
    output
}

/// Rank entries for a query.
fn search<'a>(
    entries: &'a [SkillSearchEntry],
    query: &str,
    limit: usize,
) -> Vec<&'a SkillSearchEntry> {
    let candidates = entries
        .iter()
        .map(|entry| (entry, document_tokens(entry)))
        .collect();
    rank(
        candidates,
        query,
        CaseFold::Unicode,
        limit,
        |entry| exact_name_bonus(entry, query),
        |left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.location.cmp(&right.location))
        },
    )
}

fn exact_name_bonus(entry: &SkillSearchEntry, query: &str) -> f64 {
    if query.trim().eq_ignore_ascii_case(&entry.name) {
        1_000.0
    } else {
        0.0
    }
}

fn document_tokens(entry: &SkillSearchEntry) -> Vec<String> {
    let mut tokens = Vec::new();
    for _ in 0..2 {
        tokens.extend(tokens_of(&entry.name, CaseFold::Unicode));
    }
    tokens.extend(tokens_of(&entry.description, CaseFold::Unicode));
    tokens
}
