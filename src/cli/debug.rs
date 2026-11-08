use crate::cli::settings::load_settings;
use crate::cli::{
    CHECK_MCP_CONFIG, CHECK_MCP_SERVER, CmdRunner, CommandRunOptions, DiagnosticCheck,
    DiagnosticDocument, report_failure, settings_warnings,
};
use crate::clients::mcp::connect_server;
use crate::config::mcp::discover_mcp_servers;
use crate::config::model::ApiType;
use crate::config::settings::LoadedSettings;
use crate::config::skills::{discover_skills_with_paths, parse_skill_path_list};
use crate::config::trust::{ProjectTrustReport, TrustState};
use crate::config::{DataDir, ModelDefinition, SettingsLoader};
use clap::{Parser, Subcommand};
use serde::Serialize;
use serde_json::json;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Debug and introspection commands.
#[derive(Clone, Debug, Parser)]
#[command(after_help = "\
Examples:
  cake debug skills --catalog-budget 4000
  cake debug models --json | jq '.data.models[].name'")]
pub struct DebugCommand {
    #[command(subcommand)]
    command: DebugSubcommand,
}

#[derive(Clone, Debug, Subcommand)]
enum DebugSubcommand {
    /// Report selected skill catalog size without calling a model
    Skills {
        /// Advisory rendered XML budget in Unicode characters
        #[arg(long, default_value_t = 8000)]
        catalog_budget: usize,
        /// Advisory routing-description budget in Unicode characters
        #[arg(long, default_value_t = 400)]
        description_budget: usize,
        /// Report an empty catalog, as for a run with --no-skills
        #[arg(long)]
        no_skills: bool,
        /// Only include these comma-separated skill names
        #[arg(long, value_name = "NAMES")]
        skills: Option<String>,
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
    async fn run(
        &self,
        _data_dir: &DataDir,
        options: &CommandRunOptions<'_>,
    ) -> anyhow::Result<()> {
        // CmdRunner is asynchronous, although some commands have no async work.
        std::future::ready(()).await;
        match &self.command {
            DebugSubcommand::Skills {
                catalog_budget,
                description_budget,
                no_skills,
                skills,
            } => run_skills(
                *catalog_budget,
                *description_budget,
                *no_skills,
                skills.as_deref(),
                options,
            ),
            DebugSubcommand::Models { json } => run_models(*json, options),
            DebugSubcommand::Mcp { json, mcp_config } => run_mcp(*json, mcp_config, options).await,
        }
    }
}

/// Tell a human that authority-bearing project configuration was ignored.
fn print_project_trust_warning(trust: &ProjectTrustReport) {
    if matches!(
        trust.state,
        TrustState::Untrusted | TrustState::Changed | TrustState::Invalid
    ) {
        eprintln!(
            "warning: project configuration is {}; authority-bearing settings were ignored. Run `cake trust` to review.",
            trust.state.label()
        );
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
    catalog_budget: usize,
    description_budget: usize,
    no_skills: bool,
    skills: Option<&str>,
    options: &CommandRunOptions<'_>,
) -> anyhow::Result<()> {
    let load = load_settings("debug skills", false, options)?;
    let loaded = load.settings;
    loaded.print_warnings();
    print_project_trust_warning(&load.trust);
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
    for diagnostic in &catalog.diagnostics {
        eprintln!(
            "Skill diagnostic ({}): {}",
            diagnostic.file.display(),
            diagnostic.message
        );
    }
    print!(
        "{}",
        catalog.size_report(catalog_budget, description_budget)
    );
    Ok(())
}

/// One resolved server row in the `debug mcp` report.
#[derive(Debug, Serialize)]
struct McpServerReport {
    name: String,
    command: String,
    args: Vec<String>,
    env: Vec<String>,
    source: String,
    timeout_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    exposure: crate::config::mcp::McpExposure,
    status: &'static str,
    protocol_version: Option<String>,
    server_info: Option<String>,
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
                format!("MCP server '{}' failed: {error}", report.name),
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
        name: config.name.clone(),
        command: config.command.clone(),
        args: config.args.clone(),
        env: config.env.keys().cloned().collect(),
        source: config.source.display().to_string(),
        timeout_secs: config.timeout_secs,
        description: config.description.clone(),
        exposure: config.exposure,
        status: "failed",
        protocol_version: None,
        server_info: None,
        error: None,
        tools: Vec::new(),
    };
    if config.exposure == crate::config::mcp::McpExposure::Hidden {
        report.status = "hidden";
        return report;
    }
    match connect_server(config, current_dir).await {
        Ok(server) => {
            report.status = "ready";
            report.protocol_version = Some(server.protocol_version.clone());
            report.server_info = server.server_info.clone();
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
        output.push('\n');
        _ = writeln!(output, "{} ({})", report.name, report.status);
        _ = writeln!(output, "  source: {}", report.source);
        let command = std::iter::once(report.command.as_str())
            .chain(report.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        _ = writeln!(output, "  command: {command}");
        _ = writeln!(output, "  timeout: {}s", report.timeout_secs);
        _ = writeln!(output, "  exposure: {}", report.exposure.as_str());
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
    output
}

fn format_models(models: &std::collections::HashMap<String, ModelDefinition>) -> String {
    if models.is_empty() {
        return "No models configured.\n".to_string();
    }

    let mut rows: Vec<ModelRow<'_>> = models.values().map(ModelRow::from).collect();
    rows.sort_by(|left, right| left.name.cmp(right.name));

    let headers = ["Name", "Model", "API Type", "Base URL", "API Key Env"];
    let widths = column_widths(&headers, &rows);

    let mut output = String::new();
    output.push_str("Configured Models\n");
    output.push_str(&separator(&widths));
    output.push('\n');
    output.push_str(&format_row(&headers, &widths));
    output.push('\n');
    output.push_str(&separator(&widths));
    output.push('\n');

    for row in rows {
        output.push_str(&format_row(
            &[
                row.name,
                row.model,
                row.api_type,
                row.base_url,
                row.api_key_env,
            ],
            &widths,
        ));
        output.push('\n');
    }

    output.push_str(&separator(&widths));
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

fn column_widths(headers: &[&str; 5], rows: &[ModelRow<'_>]) -> [usize; 5] {
    let mut widths = headers.map(str::len);
    for row in rows {
        let values = [
            row.name,
            row.model,
            row.api_type,
            row.base_url,
            row.api_key_env,
        ];
        for (index, value) in values.iter().enumerate() {
            widths[index] = widths[index].max(value.len());
        }
    }
    widths
}

fn format_row(values: &[&str; 5], widths: &[usize; 5]) -> String {
    format!(
        "{:<name_width$}  {:<model_width$}  {:<api_width$}  {:<url_width$}  {:<key_width$}",
        values[0],
        values[1],
        values[2],
        values[3],
        values[4],
        name_width = widths[0],
        model_width = widths[1],
        api_width = widths[2],
        url_width = widths[3],
        key_width = widths[4],
    )
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
            "project_trust": {
                "state": trust.state.label(),
                "authority": trust.authority,
                "changes": trust.changes,
            },
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

fn separator(widths: &[usize; 5]) -> String {
    format!(
        "{}  {}  {}  {}  {}",
        "-".repeat(widths[0]),
        "-".repeat(widths[1]),
        "-".repeat(widths[2]),
        "-".repeat(widths[3]),
        "-".repeat(widths[4]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(name: &str, api_type: ApiType) -> ModelDefinition {
        ModelDefinition {
            supports_images: false,
            name: name.to_string(),
            model: format!("provider/{name}"),
            base_url: format!("https://{name}.example.com/v1"),
            api_key_env: format!("{}_API_KEY", name.to_ascii_uppercase()),
            provider: None,
            provider_headers: None,
            api_type,
            temperature: None,
            top_p: None,
            top_k: None,
            max_output_tokens: None,
            context_window: None,
            reasoning_effort: None,
            reasoning_summary: None,
            reasoning_max_tokens: None,
            providers: vec![],
        }
    }

    #[test]
    fn format_models_prints_sorted_model_rows() {
        let mut models = std::collections::HashMap::new();
        models.insert("zen".to_string(), model("zen", ApiType::ChatCompletions));
        models.insert("alpha".to_string(), model("alpha", ApiType::Responses));

        let output = format_models(&models);

        assert!(output.contains("Configured Models"));
        assert!(output.contains("Name"));
        assert!(output.contains("API Key Env"));
        assert!(output.contains("alpha  provider/alpha"));
        assert!(output.contains("responses"));
        assert!(output.contains("zen    provider/zen"));
        assert!(output.contains("chat_completions"));
        assert!(output.contains("ZEN_API_KEY"));
        assert!(!output.contains("actual-secret-value"));
        assert!(output.find("alpha").expect("alpha row") < output.find("zen").expect("zen row"));
    }

    #[test]
    fn format_models_handles_empty_settings() {
        let output = format_models(&std::collections::HashMap::new());
        assert_eq!(output, "No models configured.\n");
    }

    #[test]
    fn render_models_json_returns_a_diagnostic_document() {
        let mut models = std::collections::HashMap::new();
        models.insert("zen".to_string(), model("zen", ApiType::ChatCompletions));
        let loaded = loaded_settings(models, None, Vec::new());

        let output = render_models_json(&loaded, &trust_report()).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["schema_version"], 1);
        assert_eq!(parsed["command"], "debug models");
        assert_eq!(parsed["status"], "ok");
        assert_eq!(parsed["summary"]["count"], 1);
        assert_eq!(parsed["checks"], serde_json::json!([]));
        assert_eq!(parsed["data"]["models"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["data"]["models"][0]["name"], "zen");
    }

    #[test]
    fn render_models_json_reports_the_resolved_default_model() {
        let mut models = std::collections::HashMap::new();
        models.insert("zen".to_string(), model("zen", ApiType::ChatCompletions));

        let output = render_models_json(
            &loaded_settings(models, Some("zen"), Vec::new()),
            &trust_report(),
        )
        .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["data"]["default_model"], "zen");
        assert_eq!(parsed["data"]["models"][0]["name"], "zen");
    }

    #[test]
    fn render_models_json_empty() {
        let output = render_models_json(
            &loaded_settings(std::collections::HashMap::default(), None, Vec::new()),
            &trust_report(),
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["summary"]["count"], 0);
        assert_eq!(parsed["data"]["models"], serde_json::json!([]));
        assert_eq!(parsed["data"]["default_model"], serde_json::json!(null));
    }

    #[test]
    fn render_models_json_non_empty() {
        let mut models = std::collections::HashMap::new();
        models.insert("zen".to_string(), model("zen", ApiType::ChatCompletions));
        models.insert("alpha".to_string(), model("alpha", ApiType::Responses));

        let output =
            render_models_json(&loaded_settings(models, None, Vec::new()), &trust_report())
                .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let arr = parsed["data"]["models"]
            .as_array()
            .expect("models must be a JSON array");
        assert_eq!(arr.len(), 2);

        // Verify sorted order (alpha before zen)
        assert_eq!(arr[0]["name"], "alpha");
        assert_eq!(arr[0]["model"], "provider/alpha");
        assert_eq!(arr[0]["api_type"], "responses");
        assert_eq!(arr[1]["name"], "zen");
        assert_eq!(arr[1]["model"], "provider/zen");
        assert_eq!(arr[1]["api_type"], "chat_completions");

        // Verify full fields are present (not just the table subset)
        assert!(arr[0].get("provider").is_some());
        assert!(arr[0].get("temperature").is_some());
        assert!(arr[0].get("top_p").is_some());
        assert!(arr[0].get("top_k").is_some());

        // Verify no leaked secrets — only env var names
        assert_eq!(arr[0]["api_key_env"], "ALPHA_API_KEY");
        assert_eq!(arr[1]["api_key_env"], "ZEN_API_KEY");
    }

    #[test]
    fn render_models_json_reports_settings_findings_instead_of_stderr_text() {
        let warnings = vec!["unknown key 'temparature' in .cake/settings.toml".to_string()];
        let output = render_models_json(
            &loaded_settings(std::collections::HashMap::default(), None, warnings),
            &trust_report(),
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(parsed["status"], "warning");
        let checks = parsed["checks"].as_array().unwrap();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0]["id"], "settings.unknown_key");
        assert_eq!(checks[0]["status"], "warning");
        assert_eq!(
            checks[0]["message"],
            "unknown key 'temparature' in .cake/settings.toml"
        );
    }

    /// A report for a project with no authority-bearing configuration.
    fn trust_report() -> ProjectTrustReport {
        ProjectTrustReport {
            state: TrustState::NotRequired,
            mode: crate::config::trust::ProjectConfigMode::Inspect,
            project_root: std::path::PathBuf::from("/tmp/project"),
            project_key: "key".to_string(),
            record_path: std::path::PathBuf::from("/tmp/trust/key.json"),
            authority: Vec::new(),
            changes: Vec::new(),
            invalid_reason: None,
        }
    }

    /// A loaded settings value for `--json` rendering tests.
    fn loaded_settings(
        models: std::collections::HashMap<String, ModelDefinition>,
        default_model: Option<&str>,
        warnings: Vec<String>,
    ) -> LoadedSettings {
        LoadedSettings {
            gates: crate::config::gates::GateConfiguration::default(),
            models,
            default_model: default_model.map(str::to_string),
            directories: Vec::new(),
            sandbox: crate::config::settings::SandboxSettings::default(),
            skills: crate::config::settings::SkillSettings::default(),
            tools_enabled: None,
            system_prompt: None,
            judge: crate::config::settings::JudgeSettings::default(),
            limits: crate::config::settings::ResolvedLimits::default(),
            warnings,
        }
    }

    fn fixture_mcp_report(status: &'static str) -> McpServerReport {
        McpServerReport {
            name: "safari-mcp".to_string(),
            command: "/usr/bin/safaridriver".to_string(),
            args: vec!["--mcp".to_string()],
            env: vec!["SAFARI_TOKEN".to_string()],
            source: "/home/user/.config/cake/mcp.json".to_string(),
            timeout_secs: 60,
            description: Some("Browser automation".to_string()),
            exposure: if status == "hidden" {
                crate::config::mcp::McpExposure::Hidden
            } else {
                crate::config::mcp::McpExposure::Direct
            },
            status,
            protocol_version: (status == "ready").then(|| "2026-07-28".to_string()),
            server_info: (status == "ready").then(|| "Safari".to_string()),
            error: (status == "failed").then(|| "connection closed".to_string()),
            tools: if status == "ready" {
                vec![McpToolReport {
                    name: "list_tabs".to_string(),
                    registered_name: "mcp__safari-mcp__list_tabs".to_string(),
                    description: "List tabs".to_string(),
                    has_output_schema: false,
                }]
            } else {
                Vec::new()
            },
        }
    }

    #[test]
    fn mcp_text_report_lists_ready_and_failed_servers() {
        let ready = format_mcp_report(&[fixture_mcp_report("ready")]);
        assert!(ready.contains("safari-mcp (ready)"));
        assert!(ready.contains("/usr/bin/safaridriver --mcp"));
        assert!(ready.contains("env: SAFARI_TOKEN"));
        assert!(ready.contains("exposure: direct"));
        assert!(ready.contains("description: Browser automation"));
        assert!(ready.contains("list_tabs -> mcp__safari-mcp__list_tabs"));

        let failed = format_mcp_report(&[fixture_mcp_report("failed")]);
        assert!(failed.contains("safari-mcp (failed)"));
        assert!(failed.contains("error: connection closed"));
    }

    #[test]
    fn mcp_text_report_marks_hidden_servers() {
        let hidden = format_mcp_report(&[fixture_mcp_report("hidden")]);
        assert!(hidden.contains("safari-mcp (hidden)"));
        assert!(hidden.contains("exposure: hidden"));
        assert!(!hidden.contains("protocol:"));
    }

    #[test]
    fn empty_mcp_report_is_explicit() {
        assert_eq!(format_mcp_report(&[]), "No MCP servers configured.\n");
    }

    #[test]
    fn mcp_json_document_reports_warning_status_for_a_failed_server() {
        let document = DiagnosticDocument::new(
            "debug mcp",
            json!({"count": 1, "ready": 0, "tool_count": 0}),
            vec![DiagnosticCheck::warning(
                CHECK_MCP_SERVER,
                "MCP server 'safari-mcp' failed: connection closed",
            )],
            json!({"servers": [fixture_mcp_report("failed")]}),
        );
        let rendered = document.render().unwrap();
        let value: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(value["status"], "warning");
        assert_eq!(value["data"]["servers"][0]["status"], "failed");
        assert_eq!(value["data"]["servers"][0]["exposure"], "direct");
        assert_eq!(
            value["data"]["servers"][0]["description"],
            "Browser automation"
        );
        assert_eq!(value["checks"][0]["id"], CHECK_MCP_SERVER);
    }
}
