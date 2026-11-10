//! `cake debug tools`: the effective tool surface a run would assemble.
//!
//! The report is local and read-only. It resolves settings, the sandbox
//! policy, the model's image capability, and the exact-name selection the same
//! way a run does, then lists configured toolbox and MCP sources without running
//! `describe`, connecting a server, or starting an agent, provider, gate, hook,
//! or session writer. External candidates are marked `unprobed` because their
//! tool names and availability are only knowable by executing external code.

use crate::cli::debug::{print_project_trust_warning, project_trust_warning_message};
use crate::cli::settings::load_settings;
use crate::cli::{
    CHECK_CONFIG_PROJECT_TRUST, CHECK_MCP_CONFIG, CHECK_TOOLS_SELECTION, CommandRunOptions,
    DiagnosticCheck, DiagnosticDocument, settings_warnings,
};
use crate::clients::{
    BuiltinToolFilterReason, BuiltinToolInventoryEntry, SandboxPolicy, TOOL_SEARCH_NAME,
    builtin_tool_inventory, resolve_sandbox_policy_with_source, tool_search_enabled,
};
use crate::config::mcp::{MCP_TOOL_PREFIX, McpExposure, discover_mcp_servers};
use crate::config::settings::LoadedSettings;
use crate::config::toolbox::{
    TOOLBOX_ENV_VAR, TOOLBOX_PREFIX, discover_toolbox_entries, filter_project_toolbox_dirs,
    toolbox_directories,
};
use crate::config::trust::ProjectTrustReport;
use crate::config::{SettingsLoader, config_dir};
use serde::Serialize;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The run-time fact the local report cannot establish.
const GATE_READINESS_NOTE: &str = "Gate readiness is not probed. A run-time gate startup failure \
                                    can additionally remove Bash and BashSession.";

/// Inspect the effective tool surface without starting external code.
pub(super) fn run_tools(
    sandbox: Option<SandboxPolicy>,
    no_tools: bool,
    tools: Option<&str>,
    json_output: bool,
    options: &CommandRunOptions<'_>,
) -> anyhow::Result<()> {
    let load = load_settings("debug tools", json_output, options)?;
    let current_dir = std::env::current_dir()
        .map_err(|error| anyhow::anyhow!("Failed to get current directory: {error}"))?;
    let report = build_report(
        sandbox,
        no_tools,
        tools,
        &load.settings,
        &load.trust,
        options,
        &current_dir,
        load.mcp_config_bytes.as_deref(),
    )?;

    if json_output {
        print!(
            "{}",
            render_json(&report, &load.settings.warnings, &load.trust)?
        );
    } else {
        load.settings.print_warnings();
        print_project_trust_warning(&load.trust);
        for warning in &report.mcp_warnings {
            eprintln!("warning: {warning}");
        }
        print!("{}", format_text(&report));
    }
    Ok(())
}

/// One resolved model capability row.
#[derive(Debug, Serialize)]
struct ModelReport {
    name: Option<String>,
    /// `--model`, `default_model`, or absent.
    source: Option<&'static str>,
    configured: bool,
    supports_images: bool,
}

impl ModelReport {
    fn resolve(loaded: &LoadedSettings, options: &CommandRunOptions<'_>) -> Self {
        let (name, source) = options.model.map_or_else(
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
            configured: definition.is_some(),
            supports_images: definition.is_some_and(|definition| definition.supports_images),
        }
    }
}

/// One built-in tool row with its effective status.
#[derive(Debug, Serialize)]
struct BuiltinToolRow {
    name: &'static str,
    status: &'static str,
    reason: Option<&'static str>,
}

impl From<&BuiltinToolInventoryEntry> for BuiltinToolRow {
    fn from(entry: &BuiltinToolInventoryEntry) -> Self {
        Self {
            name: entry.name,
            status: if entry.enabled { "enabled" } else { "filtered" },
            reason: entry.reason.map(BuiltinToolFilterReason::label),
        }
    }
}

/// The exact-name selection and the names that resolve to no known source.
#[derive(Debug, Serialize)]
struct SelectionReport {
    /// `--no-tools`, `--tools`, `settings`, or `default`.
    source: &'static str,
    /// Exact-name selection, `None` for all tools.
    selected: Option<Vec<String>>,
    /// Selected names that match no built-in, toolbox, or MCP source.
    unknown: Vec<String>,
}

impl SelectionReport {
    fn resolve(
        loaded: &LoadedSettings,
        no_tools: bool,
        tools: Option<&str>,
        known: &KnownSources<'_>,
    ) -> Self {
        let selected =
            SettingsLoader::resolve_tools_config(no_tools, tools, loaded.tools_enabled.as_deref());
        let source = selection_source(loaded, no_tools, tools);
        let unknown = selected
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|name| !known.matches(name))
            .cloned()
            .collect();
        Self {
            source,
            selected,
            unknown,
        }
    }
}

const fn selection_source(
    loaded: &LoadedSettings,
    no_tools: bool,
    tools: Option<&str>,
) -> &'static str {
    if no_tools {
        "--no-tools"
    } else if tools.is_some() {
        "--tools"
    } else if loaded.tools_enabled.is_some() {
        "settings"
    } else {
        "default"
    }
}

/// The names a diagnostic can attribute to a known built-in or configured
/// external source without probing it.
struct KnownSources<'a> {
    builtins: &'a [BuiltinToolInventoryEntry],
    has_toolbox_candidates: bool,
    mcp_servers: &'a [McpSourceReport],
}

impl KnownSources<'_> {
    fn matches(&self, name: &str) -> bool {
        self.builtins.iter().any(|entry| entry.name == name)
            || name == TOOL_SEARCH_NAME
            || (self.has_toolbox_candidates && name.starts_with(TOOLBOX_PREFIX))
            || self
                .mcp_servers
                .iter()
                .any(|server| name.starts_with(&format!("{MCP_TOOL_PREFIX}{}__", server.name)))
    }
}

/// `ToolSearch` availability for the resolved configuration.
#[derive(Debug, Serialize)]
struct ToolSearchReport {
    name: &'static str,
    /// `conditional` when a deferred MCP source could install it, otherwise
    /// `absent`.
    state: &'static str,
    reason: String,
    deferred_server_count: usize,
    /// `direct` when permitted deferred tools are registered directly because
    /// the exact-name selection excludes `ToolSearch`.
    fallback: Option<&'static str>,
}

impl ToolSearchReport {
    fn resolve(
        selected: Option<&[String]>,
        policy: SandboxPolicy,
        mcp_servers: &[McpSourceReport],
    ) -> Self {
        let deferred_server_count = mcp_servers
            .iter()
            .filter(|server| server.exposure == McpExposure::Deferred)
            .count();
        let allowed = tool_search_enabled(selected);

        if deferred_server_count == 0 {
            return Self {
                name: TOOL_SEARCH_NAME,
                state: "absent",
                reason: "no deferred MCP servers are configured".to_string(),
                deferred_server_count,
                fallback: None,
            };
        }
        if policy == SandboxPolicy::ReadOnly {
            return Self {
                name: TOOL_SEARCH_NAME,
                state: "absent",
                reason: "read-only policy; MCP servers are not started".to_string(),
                deferred_server_count,
                fallback: None,
            };
        }
        if selected.is_some_and(<[String]>::is_empty) {
            return Self {
                name: TOOL_SEARCH_NAME,
                state: "absent",
                reason: "--no-tools selects no tools".to_string(),
                deferred_server_count,
                fallback: None,
            };
        }
        if !allowed {
            return Self {
                name: TOOL_SEARCH_NAME,
                state: "absent",
                reason: "the exact-name selection does not include ToolSearch".to_string(),
                deferred_server_count,
                fallback: Some("direct"),
            };
        }
        Self {
            name: TOOL_SEARCH_NAME,
            state: "conditional",
            reason: "installed when a configured deferred MCP server connects and exposes an \
                     allowed tool"
                .to_string(),
            deferred_server_count,
            fallback: None,
        }
    }
}

/// One configured toolbox executable, before `describe` runs.
#[derive(Debug, Serialize)]
struct ToolboxCandidateReport {
    filename: String,
    path: String,
    status: &'static str,
    available: bool,
}

/// One toolbox directory and its unprobed candidates.
#[derive(Debug, Serialize)]
struct ToolboxDirectoryReport {
    path: String,
    /// `project`, `global`, or `configured`.
    source: &'static str,
    project_local: bool,
    trusted: bool,
    /// `loadable`, `untrusted`, or `read-only`.
    status: &'static str,
    exists: bool,
    candidates: Vec<ToolboxCandidateReport>,
}

#[derive(Debug, Serialize)]
struct ToolboxReport {
    describe_probed: bool,
    directories: Vec<ToolboxDirectoryReport>,
}

/// One configured MCP server, without connecting or reading environment values.
#[derive(Debug, Serialize)]
struct McpSourceReport {
    name: String,
    source: String,
    command: String,
    args: Vec<String>,
    exposure: McpExposure,
    status: &'static str,
    available: bool,
    reason: Option<&'static str>,
}

#[derive(Debug, Serialize)]
struct McpSourcesReport {
    connected: bool,
    servers: Vec<McpSourceReport>,
}

/// The complete read-only tool inventory.
struct ToolInventoryReport {
    working_dir: PathBuf,
    model: ModelReport,
    policy: SandboxPolicy,
    policy_source: &'static str,
    builtins: Vec<BuiltinToolRow>,
    tool_search: ToolSearchReport,
    selection: SelectionReport,
    toolbox: ToolboxReport,
    mcp: McpSourcesReport,
    mcp_warnings: Vec<String>,
}

impl ToolInventoryReport {
    fn external_tools_available(&self) -> bool {
        self.policy != SandboxPolicy::ReadOnly
    }

    fn checks(&self, settings: &[String], trust: &ProjectTrustReport) -> Vec<DiagnosticCheck> {
        let mut checks = settings_warnings(settings);
        if let Some(message) = project_trust_warning_message(trust) {
            checks.push(DiagnosticCheck::warning(
                CHECK_CONFIG_PROJECT_TRUST,
                message,
            ));
        }
        checks.extend(
            self.mcp_warnings
                .iter()
                .map(|warning| DiagnosticCheck::warning(CHECK_MCP_CONFIG, warning.clone())),
        );
        checks.extend(self.selection.unknown.iter().map(|name| {
            DiagnosticCheck::warning(
                CHECK_TOOLS_SELECTION,
                format!("tool selection '{name}' matches no built-in, toolbox, or MCP source"),
            )
        }));
        checks
    }

    fn summary(&self, warning_count: usize) -> Value {
        let enabled = self
            .builtins
            .iter()
            .filter(|row| row.status == "enabled")
            .count();
        let toolbox_candidates: usize = self
            .toolbox
            .directories
            .iter()
            .map(|directory| directory.candidates.len())
            .sum();
        let deferred_mcp_servers = self
            .mcp
            .servers
            .iter()
            .filter(|server| server.exposure == McpExposure::Deferred)
            .count();
        json!({
            "model": self.model.name,
            "supports_images": self.model.supports_images,
            "policy": self.policy.label(),
            "policy_source": self.policy_source,
            "builtin_tool_count": self.builtins.len(),
            "enabled_builtin_count": enabled,
            "filtered_builtin_count": self.builtins.len() - enabled,
            "toolbox_directory_count": self.toolbox.directories.len(),
            "toolbox_candidate_count": toolbox_candidates,
            "mcp_server_count": self.mcp.servers.len(),
            "deferred_mcp_server_count": deferred_mcp_servers,
            "selected_count": self.selection.selected.as_ref().map(Vec::len),
            "unknown_selection_count": self.selection.unknown.len(),
            "warning_count": warning_count,
        })
    }
}

/// Resolve every read-only tool input for `debug tools`.
///
/// `project_bytes` is the trusted `.cake/mcp.json` content read by the settings
/// snapshot, so the bytes reported are the bytes a run would parse.
#[expect(
    clippy::too_many_arguments,
    reason = "the report mirrors independently resolved CLI, settings, trust, and discovery inputs"
)]
fn build_report(
    sandbox: Option<SandboxPolicy>,
    no_tools: bool,
    tools: Option<&str>,
    loaded: &LoadedSettings,
    trust: &ProjectTrustReport,
    options: &CommandRunOptions<'_>,
    current_dir: &Path,
    project_mcp_bytes: Option<&[u8]>,
) -> anyhow::Result<ToolInventoryReport> {
    let (policy, policy_source) = resolve_sandbox_policy_with_source(sandbox);
    let model = ModelReport::resolve(loaded, options);
    let selected =
        SettingsLoader::resolve_tools_config(no_tools, tools, loaded.tools_enabled.as_deref());
    let inventory = builtin_tool_inventory(policy, model.supports_images, selected.as_deref());
    let builtins: Vec<BuiltinToolRow> = inventory.iter().map(BuiltinToolRow::from).collect();
    let toolbox = ToolboxReport::build(current_dir, trust, options, policy);
    let (mcp, mcp_warnings) =
        discover_sources(current_dir, trust, options, policy, project_mcp_bytes)?;
    let known = KnownSources {
        builtins: &inventory,
        has_toolbox_candidates: toolbox
            .directories
            .iter()
            .any(|directory| !directory.candidates.is_empty()),
        mcp_servers: &mcp,
    };
    let selection = SelectionReport::resolve(loaded, no_tools, tools, &known);
    let tool_search = ToolSearchReport::resolve(selection.selected.as_deref(), policy, &mcp);

    Ok(ToolInventoryReport {
        working_dir: current_dir.to_path_buf(),
        model,
        policy,
        policy_source: policy_source.label(),
        builtins,
        tool_search,
        selection,
        toolbox,
        mcp: McpSourcesReport {
            connected: false,
            servers: mcp,
        },
        mcp_warnings,
    })
}

impl ToolboxReport {
    fn build(
        working_dir: &Path,
        trust: &ProjectTrustReport,
        options: &CommandRunOptions<'_>,
        policy: SandboxPolicy,
    ) -> Self {
        let default_dir = config_dir().join("cake").join("tools");
        let mut dirs = toolbox_directories(
            std::env::var(TOOLBOX_ENV_VAR).ok().as_deref(),
            &[],
            &default_dir,
            working_dir,
            working_dir,
        );
        if !options.project_config_mode.loads_project_config() {
            dirs = filter_project_toolbox_dirs(dirs, &[working_dir]);
        }

        let project_cake_root = trust.project_root.join(".cake");
        let directories = dirs
            .into_iter()
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
                    .map(|entry| ToolboxCandidateReport {
                        filename: entry.filename,
                        path: entry.path.display().to_string(),
                        status: "unprobed",
                        available: status == "loadable",
                    })
                    .collect();
                ToolboxDirectoryReport {
                    path: dir.display().to_string(),
                    source: toolbox_source(&dir, working_dir, &default_dir),
                    project_local,
                    trusted,
                    status,
                    exists: dir.is_dir(),
                    candidates,
                }
            })
            .collect();
        Self {
            describe_probed: false,
            directories,
        }
    }
}

/// Attribute a toolbox directory to the source that selected it.
fn toolbox_source(dir: &Path, working_dir: &Path, default_dir: &Path) -> &'static str {
    if dir.starts_with(working_dir.join(".cake")) {
        "project"
    } else if dir == default_dir {
        "global"
    } else {
        "configured"
    }
}

/// Discover configured MCP servers without connecting to any of them.
fn discover_sources(
    current_dir: &Path,
    trust: &ProjectTrustReport,
    options: &CommandRunOptions<'_>,
    policy: SandboxPolicy,
    project_mcp_bytes: Option<&[u8]>,
) -> anyhow::Result<(Vec<McpSourceReport>, Vec<String>)> {
    let discovery = discover_mcp_servers(
        &[],
        current_dir,
        &config_dir(),
        current_dir,
        trust,
        options.project_config_mode,
        project_mcp_bytes,
    )
    .map_err(|error| anyhow::anyhow!("MCP discovery failed: {error}"))?;
    Ok((
        discovery
            .servers
            .iter()
            .map(|config| mcp_source_report(config, policy))
            .collect(),
        discovery.warnings,
    ))
}

/// Convert one configured server without reading environment values.
fn mcp_source_report(
    config: &crate::config::mcp::McpServerConfig,
    policy: SandboxPolicy,
) -> McpSourceReport {
    let (available, reason) = match config.exposure {
        McpExposure::Hidden => (false, Some("hidden exposure")),
        McpExposure::Direct | McpExposure::Deferred if policy == SandboxPolicy::ReadOnly => {
            (false, Some("read-only policy"))
        },
        McpExposure::Direct | McpExposure::Deferred => (true, None),
    };
    McpSourceReport {
        name: config.name.clone(),
        source: config.source.display().to_string(),
        command: config.command.clone(),
        args: config.args.clone(),
        exposure: config.exposure,
        status: "unprobed",
        available,
        reason,
    }
}

fn render_json(
    report: &ToolInventoryReport,
    settings: &[String],
    trust: &ProjectTrustReport,
) -> anyhow::Result<String> {
    let checks = report.checks(settings, trust);
    let summary = report.summary(checks.len());
    DiagnosticDocument::new(
        "debug tools",
        summary,
        checks,
        json!({
            "working_directory": report.working_dir.display().to_string(),
            "model": report.model,
            "sandbox": {
                "policy": report.policy.label(),
                "policy_source": report.policy_source,
                "external_tools_available": report.external_tools_available(),
            },
            "builtins": report.builtins,
            "tool_search": report.tool_search,
            "selection": report.selection,
            "toolbox": report.toolbox,
            "mcp": report.mcp,
            "gate_readiness": {
                "probed": false,
                "note": GATE_READINESS_NOTE,
            },
        }),
    )
    .render()
}

fn format_text(report: &ToolInventoryReport) -> String {
    let mut output = String::from("Effective tools\n");
    _ = writeln!(
        output,
        "Working directory: {}",
        report.working_dir.display()
    );
    append_model(&mut output, report);
    append_builtins(&mut output, report);
    append_tool_search(&mut output, report);
    append_toolbox(&mut output, report);
    append_mcp(&mut output, report);
    append_selection(&mut output, report);
    _ = writeln!(output, "\nNote: {GATE_READINESS_NOTE}");
    output
}

fn append_model(output: &mut String, report: &ToolInventoryReport) {
    match (&report.model.name, report.model.source) {
        (Some(name), Some(source)) => _ = writeln!(output, "Model: {name} (from {source})"),
        (Some(name), None) => _ = writeln!(output, "Model: {name}"),
        (None, _) => output.push_str("Model: none\n"),
    }
    _ = writeln!(
        output,
        "Model image support: {}",
        if report.model.supports_images {
            "yes"
        } else {
            "no"
        }
    );
    _ = writeln!(
        output,
        "Policy: {} (from {})",
        report.policy.label(),
        report.policy_source
    );
}

fn append_builtins(output: &mut String, report: &ToolInventoryReport) {
    output.push_str("\nBuilt-in tools\n");
    for row in &report.builtins {
        match row.reason {
            Some(reason) => _ = writeln!(output, "  {:<12} {:<8} {reason}", row.name, row.status),
            None => _ = writeln!(output, "  {:<12} {}", row.name, row.status),
        }
    }
}

fn append_tool_search(output: &mut String, report: &ToolInventoryReport) {
    output.push_str("\nToolSearch\n");
    _ = writeln!(output, "  State: {}", report.tool_search.state);
    _ = writeln!(output, "  Reason: {}", report.tool_search.reason);
    if let Some(fallback) = report.tool_search.fallback {
        _ = writeln!(
            output,
            "  Fallback: deferred tools are registered {fallback}ly"
        );
    }
}

fn append_toolbox(output: &mut String, report: &ToolInventoryReport) {
    output.push_str("\nToolbox (describe probing skipped)\n");
    for directory in &report.toolbox.directories {
        _ = writeln!(
            output,
            "  {} ({}, {})",
            directory.path, directory.source, directory.status
        );
        if !directory.exists {
            output.push_str("    missing\n");
        }
        for candidate in &directory.candidates {
            let status = if candidate.available && report.external_tools_available() {
                candidate.status.to_string()
            } else {
                format!("{}, unavailable", candidate.status)
            };
            _ = writeln!(
                output,
                "    {} -> {} ({status})",
                candidate.filename, candidate.path
            );
        }
    }
}

fn append_mcp(output: &mut String, report: &ToolInventoryReport) {
    output.push_str("\nMCP servers (connection skipped)\n");
    if report.mcp.servers.is_empty() {
        output.push_str("  none configured\n");
    }
    for server in &report.mcp.servers {
        _ = writeln!(
            output,
            "  {} ({}, {}, {})",
            server.name,
            server.source,
            server.exposure.as_str(),
            server.status
        );
        let command = std::iter::once(server.command.as_str())
            .chain(server.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        _ = writeln!(output, "    command: {command}");
        if let Some(reason) = server.reason {
            _ = writeln!(output, "    unavailable: {reason}");
        } else if !report.external_tools_available() {
            output.push_str("    unavailable: read-only policy\n");
        }
    }
}

fn append_selection(output: &mut String, report: &ToolInventoryReport) {
    output.push_str("\nSelection\n");
    _ = writeln!(output, "  Source: {}", report.selection.source);
    match &report.selection.selected {
        Some(names) if names.is_empty() => output.push_str("  Selected: none\n"),
        Some(names) => _ = writeln!(output, "  Selected: {}", names.join(", ")),
        None => output.push_str("  Selected: all available\n"),
    }
    if !report.selection.unknown.is_empty() {
        _ = writeln!(
            output,
            "  Unknown: {} (matches no known source)",
            report.selection.unknown.join(", ")
        );
    }
}
