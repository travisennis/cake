//! Shared facts and fixed-width tables for read-only CLI reports.
//!
//! `debug` and `doctor` describe the same resolved inputs in different
//! documents. This module owns the parts they can share without changing a
//! document shape: selecting the effective model, serializing project trust,
//! classifying toolbox directories, and rendering text tables. Each command
//! keeps its own payload structs and section order.

use crate::clients::SandboxPolicy;
use crate::config::ModelDefinition;
use crate::config::mcp::{McpExposure, McpServerConfig};
use crate::config::settings::LoadedSettings;
use crate::config::toolbox::{
    TOOLBOX_ENV_VAR, discover_toolbox_entries, filter_project_toolbox_dirs, toolbox_directories,
};
use crate::config::trust::{ProjectConfigMode, ProjectTrustReport};
use serde::Serialize;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The model a run would select, with the definition it resolves to.
pub struct SelectedModel<'a> {
    pub name: Option<String>,
    pub source: Option<&'static str>,
    pub definition: Option<&'a ModelDefinition>,
}

impl<'a> SelectedModel<'a> {
    /// Resolve `--model` first, then `default_model`, then no selection.
    pub fn resolve(loaded: &'a LoadedSettings, selected: Option<&str>) -> Self {
        let (name, source) = selected.map_or_else(
            || {
                loaded
                    .default_model
                    .as_deref()
                    .map_or((None, None), |default| {
                        (Some(default.to_string()), Some("default_model"))
                    })
            },
            |model| (Some(model.to_string()), Some("--model")),
        );
        let definition = name.as_deref().and_then(|name| loaded.models.get(name));
        Self {
            name,
            source,
            definition,
        }
    }
}

/// The project-trust object embedded in diagnostic payloads.
pub fn project_trust_payload(trust: &ProjectTrustReport) -> Value {
    json!({
        "state": trust.state.label(),
        "authority": trust.authority,
        "changes": trust.changes,
    })
}

/// The project-trust object with the resolution mode used by `debug config`.
pub fn project_trust_payload_with_mode(trust: &ProjectTrustReport) -> Value {
    let mut payload = project_trust_payload(trust);
    if let Some(object) = payload.as_object_mut() {
        object.insert("mode".to_string(), json!(trust.mode.label()));
        object.insert("one_run".to_string(), json!(trust.mode.one_run()));
    }
    payload
}

/// One toolbox candidate discovered without running `describe`.
pub struct ToolboxCandidateFacts {
    pub filename: String,
    pub path: String,
}

/// One toolbox directory resolved and classified for a report.
pub struct ToolboxDirectoryFacts {
    pub path: PathBuf,
    pub project_local: bool,
    pub trusted: bool,
    pub status: &'static str,
    pub exists: bool,
    pub candidates: Vec<ToolboxCandidateFacts>,
}

/// Resolve toolbox directories exactly as the effective reports describe them.
pub fn toolbox_directory_facts(
    extra_dirs: &[PathBuf],
    default_dir: &Path,
    working_dir: &Path,
    trust: &ProjectTrustReport,
    mode: ProjectConfigMode,
    policy: SandboxPolicy,
) -> Vec<ToolboxDirectoryFacts> {
    let mut dirs = toolbox_directories(
        std::env::var(TOOLBOX_ENV_VAR).ok().as_deref(),
        extra_dirs,
        default_dir,
        working_dir,
        working_dir,
    );
    if !mode.loads_project_config() {
        dirs = filter_project_toolbox_dirs(dirs, &[working_dir]);
    }

    let project_cake_root = trust.project_root.join(".cake");
    dirs.into_iter()
        .map(|dir| {
            let project_local = dir.starts_with(&project_cake_root);
            let trusted = !project_local || trust.allows_authority();
            let status = if !trusted {
                "untrusted"
            } else if policy == SandboxPolicy::ReadOnly {
                "read-only"
            } else {
                "loadable"
            };
            let candidates = discover_toolbox_entries(std::slice::from_ref(&dir))
                .into_iter()
                .map(|entry| ToolboxCandidateFacts {
                    filename: entry.filename,
                    path: entry.path.display().to_string(),
                })
                .collect();
            ToolboxDirectoryFacts {
                exists: dir.is_dir(),
                path: dir,
                project_local,
                trusted,
                status,
                candidates,
            }
        })
        .collect()
}

/// The command fields shared by every MCP report row.
#[derive(Debug, Serialize)]
pub struct McpDefinition {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub source: String,
    pub exposure: McpExposure,
}

impl From<&McpServerConfig> for McpDefinition {
    fn from(config: &McpServerConfig) -> Self {
        Self {
            name: config.name.clone(),
            command: config.command.clone(),
            args: config.args.clone(),
            source: config.source.display().to_string(),
            exposure: config.exposure,
        }
    }
}

/// A fixed-width text table rendered from a header and string rows.
pub struct Table {
    headers: Vec<&'static str>,
    rows: Vec<Vec<String>>,
}

impl Table {
    pub fn new(headers: &[&'static str]) -> Self {
        Self {
            headers: headers.to_vec(),
            rows: Vec::new(),
        }
    }

    pub fn push(&mut self, cells: &[&str]) {
        debug_assert_eq!(cells.len(), self.headers.len());
        self.rows
            .push(cells.iter().map(|cell| (*cell).to_string()).collect());
    }

    pub fn header(&self, trim_end: bool) -> String {
        self.line(&self.headers, trim_end)
    }

    pub fn separator(&self) -> String {
        let widths = self.widths();
        widths
            .iter()
            .map(|width| "-".repeat(*width))
            .collect::<Vec<_>>()
            .join("  ")
    }

    pub fn row(&self, index: usize, trim_end: bool) -> String {
        let cells: Vec<&str> = self.rows[index].iter().map(String::as_str).collect();
        self.line(&cells, trim_end)
    }

    pub fn render(&self, trim_end: bool) -> String {
        let mut output = self.header(trim_end);
        output.push('\n');
        output.push_str(&self.separator());
        output.push('\n');
        for index in 0..self.rows.len() {
            output.push_str(&self.row(index, trim_end));
            output.push('\n');
        }
        output
    }

    fn widths(&self) -> Vec<usize> {
        let mut widths: Vec<usize> = self.headers.iter().map(|header| header.len()).collect();
        for row in &self.rows {
            for (index, cell) in row.iter().enumerate() {
                widths[index] = widths[index].max(cell.len());
            }
        }
        widths
    }

    fn line(&self, cells: &[&str], trim_end: bool) -> String {
        let widths = self.widths();
        let mut line = String::new();
        for (index, cell) in cells.iter().enumerate() {
            if index > 0 {
                line.push_str("  ");
            }
            _ = write!(line, "{cell:<width$}", width = widths[index]);
        }
        if trim_end {
            line.trim_end().to_string()
        } else {
            line
        }
    }
}
