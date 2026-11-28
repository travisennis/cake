use crate::cli::report::{McpDefinition, Table, project_trust_payload};
use crate::cli::settings::load_settings;
use crate::cli::{
    CHECK_BLOBS_DIRECTORY, CHECK_BLOBS_ORPHANED_STORE, CHECK_BLOBS_REFERENCE_CORRUPT,
    CHECK_BLOBS_REFERENCE_MISSING, CHECK_BLOBS_SESSION, CHECK_BLOBS_TEMPORARY,
    CHECK_BLOBS_UNREFERENCED, CHECK_MCP_CONFIG, CHECK_MCP_SERVER, CmdRunner, CommandRunOptions,
    DiagnosticCheck, DiagnosticDocument, report_failure, settings_warnings,
};
use crate::clients::SandboxPolicy;
use crate::clients::mcp::connect_server;
use crate::config::blob_store::{BlobDiagnostics, SessionBlobDiagnostics};
use crate::config::mcp::discover_mcp_servers;
use crate::config::model::ApiType;
use crate::config::settings::LoadedSettings;
use crate::config::skills::{
    SkillCatalogBudget, discover_skills_with_paths, parse_skill_path_list,
};
use crate::config::trust::{ProjectTrustReport, TrustState};
use crate::config::{DataDir, ModelDefinition, SettingsLoader};
use clap::{Parser, Subcommand};
use serde::Serialize;
use serde_json::json;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

mod config_report;
pub mod grants;
mod session_report;
mod skills_report;
mod tools_report;

pub use session_report::SessionDiagnosticError;

/// Debug and introspection commands.
#[derive(Clone, Debug, Parser)]
#[command(after_help = "\
Examples:
  cake debug config
  cake debug config --json | jq '.data.model'
  cake debug session 550e8400-e29b-41d4-a716-446655440000 --json | jq '.data.tasks'
  cake debug blobs --json | jq '.data.blobs.totals'
  cake debug skills --catalog-budget 4000
  cake debug skills --json | jq '.data.skills[].name'
  cake debug grants --json | jq '.data.read_write[]'
  cake debug models --json | jq '.data.models[].name'
  cake debug tools --json | jq '.data.builtins[] | select(.status == \"filtered\")'")]
pub struct DebugCommand {
    #[command(subcommand)]
    command: DebugSubcommand,
}

#[derive(Clone, Debug, Subcommand)]
enum DebugSubcommand {
    /// Report the effective runtime configuration and project trust
    #[command(after_help = "\
Examples:
  cake debug config
  cake debug config --json | jq '.data.model'
  cake debug config --sandbox read-only --add-dir ../shared")]
    Config {
        /// Select the sandbox policy to report
        #[arg(long, value_enum)]
        sandbox: Option<SandboxPolicy>,
        /// Include an existing directory as a read-only grant. Can be repeated.
        #[arg(long = "add-dir", value_name = "DIR")]
        add_dir: Vec<String>,
        /// Report an empty skill set, as for a run with --no-skills
        #[arg(long)]
        no_skills: bool,
        /// Only include these comma-separated skill names
        #[arg(long, value_name = "NAMES")]
        skills: Option<String>,
        /// Report no model-visible tools, as for a run with --no-tools
        #[arg(long)]
        no_tools: bool,
        /// Only report these comma-separated registered tool names
        #[arg(long, value_name = "NAMES")]
        tools: Option<String>,
        /// Add a toolbox directory to report without describing its tools. Can be repeated.
        #[arg(long, value_name = "DIR")]
        toolbox: Vec<PathBuf>,
        /// Output the configuration report as one diagnostic JSON document
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Diagnose one persisted session without resuming it
    #[command(after_help = "\
Examples:
  cake debug session 550e8400-e29b-41d4-a716-446655440000
  cake debug session 550e8400-e29b-41d4-a716-446655440000 --json | jq '.data.tasks'
  cake debug session 550e8400-e29b-41d4-a716-446655440000 --export /tmp/session-diagnostic.json")]
    Session {
        /// Session UUID to diagnose
        #[arg(value_name = "UUID")]
        uuid: String,
        /// Write the redacted diagnostic document to a new local file
        #[arg(long, value_name = "PATH")]
        export: Option<PathBuf>,
        /// Report operations at or above this duration as slow
        #[arg(long, value_name = "MS", default_value_t = 2000)]
        slow_ms: u64,
        /// Output the session diagnostic as one JSON document
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Report session payload blob health without deleting anything
    #[command(after_help = "\
Examples:
  cake debug blobs
  cake debug blobs --json | jq '.data.blobs.totals'

This command is read-only. Resume-time recovery removes unreferenced blobs.")]
    Blobs {
        /// Output the blob report as one diagnostic JSON document
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Inspect selected skills and report catalog size without calling a model
    #[command(after_help = "\
Examples:
  cake debug skills
  cake debug skills --json | jq '.data.skills[].name'")]
    Skills {
        /// Override the rendered XML budget in Unicode characters
        #[arg(long, value_name = "CHARS")]
        catalog_budget: Option<usize>,
        /// Override the routing-description budget in Unicode characters
        #[arg(long, value_name = "CHARS")]
        description_budget: Option<usize>,
        /// Report an empty catalog, as for a run with --no-skills
        #[arg(long)]
        no_skills: bool,
        /// Only include these comma-separated skill names
        #[arg(long, value_name = "NAMES")]
        skills: Option<String>,
        /// Output the selected catalog as one diagnostic JSON document
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Report the effective filesystem grants and their sources
    #[command(after_help = "\
Examples:
  cake debug grants
  cake debug grants --json | jq '.data.read_write[]'
  cake debug grants --sandbox read-only --add-dir ../shared")]
    Grants {
        /// Select the sandbox policy to report
        #[arg(long, value_enum)]
        sandbox: Option<SandboxPolicy>,
        /// Include an existing directory as a read-only grant. Can be repeated.
        #[arg(long = "add-dir", value_name = "DIR")]
        add_dir: Vec<String>,
        /// Report an empty skill grant set, as for a run with --no-skills
        #[arg(long)]
        no_skills: bool,
        /// Only include grants from these comma-separated skill names
        #[arg(long, value_name = "NAMES")]
        skills: Option<String>,
        /// Output the grant report as one diagnostic JSON document
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Report the effective tool surface without starting external processes
    #[command(after_help = "\
Examples:
  cake debug tools
  cake debug tools --json | jq '.data.builtins[] | select(.status == \"filtered\")'
  cake debug tools --tools Read
  cake debug tools --no-tools
  cake debug tools --sandbox read-only")]
    Tools {
        /// Select the sandbox policy to report
        #[arg(long, value_enum)]
        sandbox: Option<SandboxPolicy>,
        /// Report no model-visible tools, as for a run with --no-tools
        #[arg(long)]
        no_tools: bool,
        /// Only report these comma-separated tool names
        #[arg(long, value_name = "NAMES")]
        tools: Option<String>,
        /// Output the tool inventory as one diagnostic JSON document
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Show configured models from settings.toml
    #[command(after_help = "\
Examples:
  cake debug models
  cake debug models --json | jq '.data.models[].name'")]
    Models {
        /// Output model definitions as one diagnostic JSON document
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Connect to configured MCP servers and list their tools
    #[command(after_help = "\
Examples:
  cake debug mcp
  cake debug mcp --json | jq '.data.servers[].tools[].registered_name'")]
    Mcp {
        /// Output the server and tool inventory as one diagnostic JSON document
        #[arg(long, default_value_t = false)]
        json: bool,
        /// Load MCP server definitions from this file as well. Can be repeated.
        #[arg(long, value_name = "PATH")]
        mcp_config: Vec<PathBuf>,
    },
}

impl CmdRunner for DebugCommand {
    async fn run(&self, data_dir: &DataDir, options: &CommandRunOptions<'_>) -> anyhow::Result<()> {
        std::future::ready(()).await;
        match &self.command {
            DebugSubcommand::Config {
                sandbox,
                add_dir,
                no_skills,
                skills,
                no_tools,
                tools,
                toolbox,
                json,
            } => config_report::run_config(
                &config_report::ConfigInputs {
                    sandbox: *sandbox,
                    add_dir,
                    no_skills: *no_skills,
                    skills: skills.as_deref(),
                    no_tools: *no_tools,
                    tools: tools.as_deref(),
                    toolbox,
                },
                *json,
                data_dir,
                options,
            ),
            DebugSubcommand::Blobs { json } => run_blobs(*json, data_dir),
            DebugSubcommand::Session {
                uuid,
                export,
                slow_ms,
                json,
            } => session_report::run_session(
                &session_report::SessionInputs {
                    uuid,
                    export: export.as_deref(),
                    slow_ms: *slow_ms,
                },
                *json,
                data_dir,
                options,
            ),
            DebugSubcommand::Skills {
                catalog_budget,
                description_budget,
                no_skills,
                skills,
                json,
            } => run_skills(
                *catalog_budget,
                *description_budget,
                *no_skills,
                skills.as_deref(),
                *json,
                options,
            ),
            DebugSubcommand::Models { json } => run_models(*json, options),
            DebugSubcommand::Tools {
                sandbox,
                no_tools,
                tools,
                json,
            } => tools_report::run_tools(*sandbox, *no_tools, tools.as_deref(), *json, options),
            DebugSubcommand::Grants {
                sandbox,
                add_dir,
                no_skills,
                skills,
                json,
            } => grants::run_grants(
                *sandbox,
                add_dir,
                *no_skills,
                skills.as_deref(),
                *json,
                data_dir,
                options,
            ),
            DebugSubcommand::Mcp { json, mcp_config } => run_mcp(*json, mcp_config, options).await,
        }
    }
}

/// Report session payload blob health without deleting anything.
fn run_blobs(json: bool, data_dir: &DataDir) -> anyhow::Result<()> {
    let diagnostics = data_dir
        .blob_diagnostics()
        .map_err(|error| report_failure(json, "debug blobs", CHECK_BLOBS_DIRECTORY, error))?;
    if json {
        print_blob_document(&diagnostics);
    } else {
        print!("{}", format_blob_diagnostics(&diagnostics));
    }
    Ok(())
}

/// Turn report findings into stable diagnostic checks.
fn blob_checks(diagnostics: &BlobDiagnostics) -> Vec<DiagnosticCheck> {
    let mut checks = Vec::new();
    for session in &diagnostics.sessions {
        append_session_blob_checks(session, &mut checks);
    }
    for store in &diagnostics.orphaned_stores {
        if let Some(error) = &store.directory_error {
            checks.push(DiagnosticCheck::warning(
                CHECK_BLOBS_DIRECTORY,
                format!(
                    "orphaned blob directory {}: {error}",
                    store.blob_dir.display()
                ),
            ));
        }
        checks.push(DiagnosticCheck::warning(
            CHECK_BLOBS_ORPHANED_STORE,
            format!(
                "blob directory {} has no matching session file ({} blob(s), {} bytes)",
                store.blob_dir.display(),
                store.blob_count,
                store.stored_bytes
            ),
        ));
        if store.temporary_file_count > 0 {
            checks.push(DiagnosticCheck::warning(
                CHECK_BLOBS_TEMPORARY,
                format!(
                    "orphaned blob directory {}: {} stale temporary blob file(s)",
                    store.blob_dir.display(),
                    store.temporary_file_count
                ),
            ));
        }
    }
    checks
}

/// Append the findings for one session report.
fn append_session_blob_checks(session: &SessionBlobDiagnostics, checks: &mut Vec<DiagnosticCheck>) {
    if let Some(error) = &session.session_error {
        checks.push(DiagnosticCheck::warning(
            CHECK_BLOBS_SESSION,
            format!("session {}: {error}", session.session_id),
        ));
    } else if !session.references_complete {
        checks.push(DiagnosticCheck::warning(
            CHECK_BLOBS_SESSION,
            format!(
                "session {}: payload references could not be collected completely",
                session.session_id
            ),
        ));
    }
    if let Some(error) = &session.directory_error {
        checks.push(DiagnosticCheck::warning(
            CHECK_BLOBS_DIRECTORY,
            format!(
                "session {}: blob directory {}: {error}",
                session.session_id,
                session.blob_dir.display()
            ),
        ));
    }
    for missing in &session.missing_references {
        checks.push(DiagnosticCheck::warning(
            CHECK_BLOBS_REFERENCE_MISSING,
            format!(
                "session {}: referenced payload {} is missing at {}",
                session.session_id,
                missing.digest,
                missing.path.display()
            ),
        ));
    }
    for corrupt in &session.corrupt_references {
        checks.push(DiagnosticCheck::warning(
            CHECK_BLOBS_REFERENCE_CORRUPT,
            format!(
                "session {}: referenced payload {} is corrupt at {}: {}",
                session.session_id,
                corrupt.digest,
                corrupt.path.display(),
                corrupt.reason
            ),
        ));
    }
    if session.unreferenced_count > 0 {
        checks.push(DiagnosticCheck::warning(
            CHECK_BLOBS_UNREFERENCED,
            format!(
                "session {}: {} unreferenced blob(s) in {}",
                session.session_id,
                session.unreferenced_count,
                session.blob_dir.display()
            ),
        ));
    }
    if session.temporary_file_count > 0 {
        checks.push(DiagnosticCheck::warning(
            CHECK_BLOBS_TEMPORARY,
            format!(
                "session {}: {} stale temporary blob file(s) in {}",
                session.session_id,
                session.temporary_file_count,
                session.blob_dir.display()
            ),
        ));
    }
}

/// Emit the machine-readable report.
fn print_blob_document(diagnostics: &BlobDiagnostics) {
    let totals = &diagnostics.totals;
    let summary = json!({
        "sessions": totals.session_count,
        "blobs": totals.blob_count,
        "unique_blobs": totals.unique_blob_count,
        "stored_bytes": totals.stored_bytes,
        "referenced": totals.referenced_count,
        "referenced_bytes": totals.referenced_bytes,
        "unreferenced": totals.unreferenced_count,
        "missing_references": totals.missing_reference_count,
        "corrupt_references": totals.corrupt_reference_count,
        "orphaned_stores": totals.orphaned_store_count,
    });
    DiagnosticDocument::new(
        "debug blobs",
        summary,
        blob_checks(diagnostics),
        json!({ "blobs": diagnostics }),
    )
    .print();
}

/// Render the short human-readable blob report.
fn format_blob_diagnostics(diagnostics: &BlobDiagnostics) -> String {
    let totals = &diagnostics.totals;
    let mut output = String::from("Session payload blobs\n");
    _ = writeln!(output, "  sessions: {}", totals.session_count);
    _ = writeln!(
        output,
        "  blobs: {} ({} unique files)",
        totals.blob_count, totals.unique_blob_count
    );
    _ = writeln!(
        output,
        "  stored: {} bytes (deduplicated)",
        totals.stored_bytes
    );
    _ = writeln!(
        output,
        "  referenced: {} ids, {} bytes",
        totals.referenced_count, totals.referenced_bytes
    );
    _ = writeln!(
        output,
        "  unreferenced: {} blobs, {} bytes",
        totals.unreferenced_count, totals.unreferenced_bytes
    );
    _ = writeln!(output, "  temporary files: {}", totals.temporary_file_count);
    _ = writeln!(
        output,
        "  missing references: {}",
        totals.missing_reference_count
    );
    _ = writeln!(
        output,
        "  corrupt references: {}",
        totals.corrupt_reference_count
    );
    _ = writeln!(
        output,
        "  orphaned stores: {} ({} blobs, {} bytes)",
        totals.orphaned_store_count, totals.orphaned_store_blob_count, totals.orphaned_store_bytes
    );

    output.push_str("\nSessions\n");
    if diagnostics.sessions.is_empty() {
        output.push_str("  (none)\n");
    }
    for session in &diagnostics.sessions {
        format_session_blob_diagnostics(session, &mut output);
    }

    output.push_str("\nOrphaned stores\n");
    if diagnostics.orphaned_stores.is_empty() {
        output.push_str("  (none)\n");
    }
    for store in &diagnostics.orphaned_stores {
        _ = writeln!(
            output,
            "  {}: {} blobs, {} bytes",
            store.blob_dir.display(),
            store.blob_count,
            store.stored_bytes
        );
        if let Some(error) = &store.directory_error {
            _ = writeln!(output, "    directory error: {error}");
        }
        if store.temporary_file_count > 0 {
            _ = writeln!(
                output,
                "    temporary files: {}",
                store.temporary_file_count
            );
        }
    }
    output
}

/// Append one session row and its problems to the text report.
fn format_session_blob_diagnostics(session: &SessionBlobDiagnostics, output: &mut String) {
    _ = writeln!(
        output,
        "  {}: {} blobs, {} bytes; {} referenced ({} bytes), {} unreferenced ({} bytes)",
        session.session_id,
        session.blob_count,
        session.stored_bytes,
        session.referenced_count,
        session.referenced_bytes,
        session.unreferenced_count,
        session.unreferenced_bytes
    );
    if let Some(error) = &session.session_error {
        _ = writeln!(output, "    session error: {error}");
    }
    if let Some(error) = &session.directory_error {
        _ = writeln!(output, "    directory error: {error}");
    }
    if session.temporary_file_count > 0 {
        _ = writeln!(
            output,
            "    temporary files: {}",
            session.temporary_file_count
        );
    }
    for missing in &session.missing_references {
        _ = writeln!(
            output,
            "    missing: {} at {}",
            missing.digest,
            missing.path.display()
        );
    }
    for corrupt in &session.corrupt_references {
        _ = writeln!(
            output,
            "    corrupt: {}: {}",
            corrupt.digest, corrupt.reason
        );
    }
}

/// Build the warning for authority-bearing project configuration that was ignored.
pub(super) fn project_trust_warning_message(trust: &ProjectTrustReport) -> Option<String> {
    matches!(
        trust.state,
        TrustState::Untrusted | TrustState::Changed | TrustState::Invalid
    )
    .then(|| {
        format!(
            "project configuration is {}; authority-bearing settings were ignored. Run `cake trust` to review.",
            trust.state.label()
        )
    })
}

/// Tell a human that authority-bearing project configuration was ignored.
pub fn print_project_trust_warning(trust: &ProjectTrustReport) {
    if let Some(message) = project_trust_warning_message(trust) {
        eprintln!("warning: {message}");
    }
}

/// Inspect configured models without setting up an agent session.
///
/// In machine mode the document carries the configured models and the settings
/// findings, so stderr stays quiet; without `--json` the warnings keep their
/// existing stderr behavior.
fn run_models(json: bool, options: &CommandRunOptions<'_>) -> anyhow::Result<()> {
    let load = load_settings("debug models", json, options)?;
    print!("{}", render_models(&load.settings, &load.trust, json)?);
    Ok(())
}

/// Render the configured models as the formatted table, or as one diagnostic
/// document when the machine flag is set.
fn render_models(
    loaded: &LoadedSettings,
    trust: &ProjectTrustReport,
    json: bool,
) -> anyhow::Result<String> {
    if json {
        render_models_json(loaded, trust)
    } else {
        loaded.print_warnings();
        print_project_trust_warning(trust);
        Ok(format_models(&loaded.models))
    }
}

/// Inspect the selected skill catalog without setting up an agent session.
fn run_skills(
    catalog_budget: Option<usize>,
    description_budget: Option<usize>,
    no_skills: bool,
    skills: Option<&str>,
    json: bool,
    options: &CommandRunOptions<'_>,
) -> anyhow::Result<()> {
    if catalog_budget == Some(0) || description_budget == Some(0) {
        return Err(anyhow::anyhow!("skill budgets must be positive integers"));
    }
    let load = load_settings("debug skills", json, options)?;
    let loaded = load.settings;
    let roots = loaded
        .skills
        .path
        .as_deref()
        .map(parse_skill_path_list)
        .unwrap_or_default();
    let current_dir = std::env::current_dir()
        .map_err(|error| anyhow::anyhow!("Failed to get current directory: {error}"))?;
    let config = SettingsLoader::resolve_skill_config(no_skills, skills, &loaded.skills);
    let catalog = config.apply(discover_skills_with_paths(&current_dir, &roots));
    let budget = SkillCatalogBudget::resolve(
        None,
        catalog_budget.or_else(|| loaded.skills.catalog_budget()),
        description_budget.or_else(|| loaded.skills.description_budget()),
    );

    if json {
        print!(
            "{}",
            skills_report::render_json(&catalog, &loaded.warnings, &load.trust, budget)?
        );
    } else {
        loaded.print_warnings();
        print_project_trust_warning(&load.trust);
        print!("{}", skills_report::format_text(&catalog, budget));
    }
    Ok(())
}

/// One resolved server row in the `debug mcp` report.
#[derive(Debug, Serialize)]
struct McpServerReport {
    #[serde(flatten)]
    definition: McpDefinition,
    env: Vec<String>,
    timeout_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    status: &'static str,
    protocol_version: Option<String>,
    server_info: Option<String>,
    /// How the client watches for a tool-list change: `subscription` for a
    /// modern server, `notifications` for a legacy one. Absent when the
    /// server never connected.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_updates: Option<String>,
    error: Option<String>,
    tools: Vec<McpToolReport>,
}

/// One discovered tool in the `debug mcp` report.
#[derive(Debug, Serialize)]
struct McpToolReport {
    name: String,
    registered_name: String,
    description: String,
    has_output_schema: bool,
}

/// Connect to every configured MCP server and report the tool inventory.
///
/// Connecting is the point of the command: a server's tools are its own
/// runtime output, so a configuration-only listing could not tell a user
/// whether the server actually works.
async fn run_mcp(
    json_output: bool,
    mcp_config: &[PathBuf],
    options: &CommandRunOptions<'_>,
) -> anyhow::Result<()> {
    let load = load_settings("debug mcp", json_output, options)?;
    let current_dir = std::env::current_dir()
        .map_err(|error| anyhow::anyhow!("Failed to get current directory: {error}"))?;
    let mcp_config =
        resolve_mcp_config_paths(mcp_config, &current_dir, options.project_config_mode);
    let discovery = discover_mcp_servers(
        &mcp_config,
        &current_dir,
        &crate::config::config_dir(),
        &current_dir,
        &load.trust,
        options.project_config_mode,
        load.mcp_config_bytes.as_deref(),
    )
    .map_err(|error| report_failure(json_output, "debug mcp", CHECK_MCP_CONFIG, error.into()))?;

    let reports = collect_mcp_reports(&discovery.servers, &current_dir).await;
    let checks = mcp_checks(&discovery.warnings, &reports);

    if json_output {
        print_mcp_document(&reports, checks);
    } else {
        print_mcp_text(&discovery.warnings, &reports);
    }
    Ok(())
}

/// Resolve explicit config paths, filtering project-local ones when project
/// configuration is disabled.
fn resolve_mcp_config_paths(
    mcp_config: &[PathBuf],
    current_dir: &Path,
    mode: crate::config::trust::ProjectConfigMode,
) -> Vec<PathBuf> {
    if mode == crate::config::trust::ProjectConfigMode::Ignore {
        crate::config::mcp::filter_project_mcp_paths(mcp_config.to_vec(), &[current_dir])
    } else {
        mcp_config.to_vec()
    }
}

/// Connect to each configured server in order and collect its report.
async fn collect_mcp_reports(
    servers: &[crate::config::mcp::McpServerConfig],
    current_dir: &Path,
) -> Vec<McpServerReport> {
    let mut reports = Vec::new();
    for config in servers {
        reports.push(report_server(config, current_dir).await);
    }
    reports
}

/// Build the diagnostic findings from configuration warnings and failed
/// servers.
fn mcp_checks(warnings: &[String], reports: &[McpServerReport]) -> Vec<DiagnosticCheck> {
    let mut checks: Vec<DiagnosticCheck> = warnings
        .iter()
        .map(|warning| DiagnosticCheck::warning(CHECK_MCP_CONFIG, warning.clone()))
        .collect();
    checks.extend(reports.iter().filter_map(|report| {
        report.error.as_ref().map(|error| {
            DiagnosticCheck::warning(
                CHECK_MCP_SERVER,
                format!("MCP server '{}' failed: {error}", report.definition.name),
            )
        })
    }));
    checks
}

/// Emit the machine-readable inventory document.
fn print_mcp_document(reports: &[McpServerReport], checks: Vec<DiagnosticCheck>) {
    let ready = reports
        .iter()
        .filter(|report| report.status == "ready")
        .count();
    let tool_count: usize = reports.iter().map(|report| report.tools.len()).sum();
    let document = DiagnosticDocument::new(
        "debug mcp",
        json!({
            "count": reports.len(),
            "ready": ready,
            "tool_count": tool_count,
        }),
        checks,
        json!({ "servers": reports }),
    );
    document.print();
}

/// Emit warnings on stderr and the human-readable inventory on stdout.
fn print_mcp_text(warnings: &[String], reports: &[McpServerReport]) {
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
    print!("{}", format_mcp_report(reports));
}

/// Connect one server and collect its report. A failed server is data, not a
/// command failure: the other servers still tell the user something useful.
async fn report_server(
    config: &crate::config::mcp::McpServerConfig,
    current_dir: &Path,
) -> McpServerReport {
    let mut report = McpServerReport {
        definition: McpDefinition::from(config),
        env: config.env.keys().cloned().collect(),
        timeout_secs: config.timeout_secs,
        description: config.description.clone(),
        status: "failed",
        protocol_version: None,
        server_info: None,
        tool_updates: None,
        error: None,
        tools: Vec::new(),
    };
    if report.definition.exposure == crate::config::mcp::McpExposure::Hidden {
        report.status = "hidden";
        return report;
    }
    match connect_server(config, current_dir).await {
        Ok(server) => {
            report.status = "ready";
            report.protocol_version = Some(server.protocol_version.clone());
            report.server_info = server.server_info.clone();
            report.tool_updates = Some(server.tool_list_change_mode().as_str().to_string());
            report.tools = server
                .tools
                .iter()
                .map(|tool| McpToolReport {
                    name: tool.name.clone(),
                    registered_name: tool.registered_name.clone(),
                    description: tool.description.clone(),
                    has_output_schema: tool.output_schema.is_some(),
                })
                .collect();
            server.shutdown().await;
        },
        Err(error) => report.error = Some(error),
    }
    report
}

/// Render the human-readable `debug mcp` report.
fn format_mcp_report(reports: &[McpServerReport]) -> String {
    if reports.is_empty() {
        return "No MCP servers configured.\n".to_string();
    }
    let mut output = String::from("MCP servers\n");
    for report in reports {
        format_mcp_server(report, &mut output);
    }
    output
}

/// Append one server row to the `debug mcp` text report.
fn format_mcp_server(report: &McpServerReport, output: &mut String) {
    output.push('\n');
    _ = writeln!(output, "{} ({})", report.definition.name, report.status);
    _ = writeln!(output, "  source: {}", report.definition.source);
    let command = std::iter::once(report.definition.command.as_str())
        .chain(report.definition.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    _ = writeln!(output, "  command: {command}");
    _ = writeln!(output, "  timeout: {}s", report.timeout_secs);
    _ = writeln!(
        output,
        "  exposure: {}",
        report.definition.exposure.as_str()
    );
    if let Some(tool_updates) = &report.tool_updates {
        _ = writeln!(output, "  tool updates: {tool_updates}");
    }
    if let Some(description) = &report.description {
        _ = writeln!(output, "  description: {description}");
    }
    if !report.env.is_empty() {
        _ = writeln!(output, "  env: {}", report.env.join(", "));
    }
    if let Some(version) = &report.protocol_version {
        _ = writeln!(output, "  protocol: {version}");
    }
    if let Some(info) = &report.server_info {
        _ = writeln!(output, "  server: {info}");
    }
    if let Some(error) = &report.error {
        _ = writeln!(output, "  error: {error}");
    }
    if !report.tools.is_empty() {
        _ = writeln!(output, "  tools ({}):", report.tools.len());
        for tool in &report.tools {
            _ = writeln!(output, "    - {} -> {}", tool.name, tool.registered_name);
        }
    }
}

fn format_models(models: &std::collections::HashMap<String, ModelDefinition>) -> String {
    if models.is_empty() {
        return "No models configured.\n".to_string();
    }

    let mut rows: Vec<ModelRow<'_>> = models.values().map(ModelRow::from).collect();
    rows.sort_by(|left, right| left.name.cmp(right.name));
    let row_count = rows.len();

    let mut table = Table::new(&["Name", "Model", "API Type", "Base URL", "API Key Env"]);
    for row in &rows {
        table.push(&[
            row.name,
            row.model,
            row.api_type,
            row.base_url,
            row.api_key_env,
        ]);
    }

    let mut output = String::from("Configured Models\n");
    output.push_str(&table.separator());
    output.push('\n');
    output.push_str(&table.header(false));
    output.push('\n');
    output.push_str(&table.separator());
    output.push('\n');
    for index in 0..row_count {
        output.push_str(&table.row(index, false));
        output.push('\n');
    }
    output.push_str(&table.separator());
    output.push('\n');
    output
}

struct ModelRow<'a> {
    name: &'a str,
    model: &'a str,
    api_type: &'static str,
    base_url: &'a str,
    api_key_env: &'a str,
}

impl<'a> From<&'a ModelDefinition> for ModelRow<'a> {
    fn from(definition: &'a ModelDefinition) -> Self {
        Self {
            name: definition.name.as_str(),
            model: definition.model.as_str(),
            api_type: api_type_label(definition.api_type),
            base_url: definition.base_url.as_str(),
            api_key_env: definition.api_key_env.as_str(),
        }
    }
}

const fn api_type_label(api_type: ApiType) -> &'static str {
    match api_type {
        ApiType::ChatCompletions => "chat_completions",
        ApiType::Responses => "responses",
    }
}

/// Render the configured models and settings findings as one diagnostic
/// document.
fn render_models_json(
    loaded: &LoadedSettings,
    trust: &ProjectTrustReport,
) -> anyhow::Result<String> {
    let models = sorted_models(&loaded.models);
    DiagnosticDocument::new(
        "debug models",
        serde_json::json!({ "count": models.len() }),
        settings_warnings(&loaded.warnings),
        serde_json::json!({
            "default_model": loaded.default_model,
            "models": models,
            "project_trust": project_trust_payload(trust),
        }),
    )
    .render()
}

/// The configured models in name order, independent of map iteration order.
fn sorted_models(
    models: &std::collections::HashMap<String, ModelDefinition>,
) -> Vec<&ModelDefinition> {
    let mut defs: Vec<&ModelDefinition> = models.values().collect();
    defs.sort_by(|left, right| left.name.cmp(&right.name));
    defs
}
