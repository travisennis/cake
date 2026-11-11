//! `cake doctor`: configured settings health and configured filesystem grants.
//!
//! The command validates the global and project settings files and the
//! selected profile through the ordinary settings loader, so it reports the
//! same unknown-key round-trip findings, parse failures, and semantic failures
//! a run would see. It then resolves the configured `directories`,
//! `[sandbox].writable`, and `[sandbox].read_only` grants through the same
//! [`crate::clients::GrantReport`] value `cake debug grants` renders.
//!
//! It is read-only: it starts no session, resolves no credential value, calls
//! no provider or gate, executes no hook or toolbox `describe`, connects no MCP
//! server, and creates no configured directory. The configured grants it
//! prints are the settings values for this project, not the complete OS-level
//! sandbox policy.

use crate::cli::debug::grants;
use crate::cli::debug::print_project_trust_warning;
use crate::cli::{
    CHECK_GRANTS_PATH, CHECK_SETTINGS_LOAD_FAILED, CmdRunner, CommandRunOptions, DiagnosticCheck,
    DiagnosticDocument, settings_warnings,
};
use crate::clients::GrantReport;
use crate::config::DataDir;
use crate::config::SettingsLoader;
use crate::config::settings::{SettingsLoad, SettingsOrigin};
use crate::config::trust::{ProjectConfigMode, ProjectTrustReport};
use clap::Parser;
use serde::Serialize;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::path::Path;

/// Validate configured settings and report the configured filesystem grants.
#[derive(Clone, Debug, Parser)]
#[command(after_help = "\
Examples:
  cake doctor
  cake doctor --json | jq '.checks'
  cake --profile review doctor --json | jq '.data.grants'

Exit codes: 0 healthy or warnings only; 3 unreadable, unparsable, or
self-contradictory settings.")]
pub struct DoctorCommand {
    /// Output the health report as one diagnostic JSON document
    #[arg(long, default_value_t = false)]
    json: bool,
}

impl CmdRunner for DoctorCommand {
    async fn run(&self, data_dir: &DataDir, options: &CommandRunOptions<'_>) -> anyhow::Result<()> {
        // CmdRunner is asynchronous, although this command has no async work.
        std::future::ready(()).await;
        run_doctor(self.json, data_dir, options)
    }
}

/// One settings file this invocation reads.
#[derive(Clone, Debug, Serialize)]
struct SettingsSourceReport {
    /// `global` or `project`.
    origin: &'static str,
    path: String,
    present: bool,
}

/// One configured grant entry with its resolved path and filesystem status.
#[derive(Clone, Debug, Serialize)]
struct ConfiguredGrant {
    /// The setting that produced the entry, such as `sandbox_writable`.
    source: &'static str,
    path: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

/// The merged configured writable and read-only grants.
#[derive(Clone, Debug, Serialize)]
struct ConfiguredGrants {
    policy: &'static str,
    policy_source: &'static str,
    writable: Vec<ConfiguredGrant>,
    read_only: Vec<ConfiguredGrant>,
}

impl ConfiguredGrants {
    /// Read the configured grant sources from the shared grant report.
    fn from_report(report: &GrantReport) -> Self {
        Self {
            policy: report.policy.label(),
            policy_source: report.policy_source.label(),
            writable: entries_for(report, &["directories", "sandbox_writable"]),
            read_only: entries_for(report, &["sandbox_read_only"]),
        }
    }

    /// Path problems and notices from the configured grant sources.
    fn warnings(report: &GrantReport) -> Vec<String> {
        let configured = ["directories", "sandbox_writable", "sandbox_read_only"];
        let mut warnings = Vec::new();
        for source in report
            .sources
            .iter()
            .filter(|source| configured.contains(&source.id))
        {
            warnings.extend(source.notices.iter().cloned());
            warnings.extend(
                source
                    .entries
                    .iter()
                    .filter(|entry| entry.status.label() != "granted")
                    .filter_map(|entry| entry.detail.clone()),
            );
        }
        warnings
    }
}

fn entries_for(report: &GrantReport, ids: &[&str]) -> Vec<ConfiguredGrant> {
    report
        .sources
        .iter()
        .filter(|source| ids.contains(&source.id))
        .flat_map(|source| {
            source.entries.iter().map(|entry| ConfiguredGrant {
                source: source.id,
                path: entry.path.display().to_string(),
                status: entry.status.label(),
                detail: entry.detail.clone(),
            })
        })
        .collect()
}

fn run_doctor(
    json_output: bool,
    data_dir: &DataDir,
    options: &CommandRunOptions<'_>,
) -> anyhow::Result<()> {
    let current_dir = std::env::current_dir()
        .map_err(|error| anyhow::anyhow!("Failed to get current directory: {error}"))?;
    let sources = settings_sources(&current_dir, options.project_config_mode);
    let load = load_settings_for_doctor(json_output, &sources, &current_dir, options)?;
    // The configured grants do not depend on the skill catalog, so this
    // resolves them without reading a skill file.
    let report = grants::build_report(
        None,
        &[],
        &load.settings,
        Vec::new(),
        &current_dir,
        data_dir,
    );
    let configured = ConfiguredGrants::from_report(&report);

    if json_output {
        print!(
            "{}",
            render_json(&load, &sources, &configured, &report, options.profile)?
        );
    } else {
        load.settings.print_warnings();
        for warning in ConfiguredGrants::warnings(&report) {
            eprintln!("warning: {warning}");
        }
        print_project_trust_warning(&load.trust);
        print!(
            "{}",
            render_text(&load, &sources, &configured, &report, options.profile)
        );
    }
    Ok(())
}

/// The settings files this invocation reads, in loader order.
fn settings_sources(current_dir: &Path, mode: ProjectConfigMode) -> Vec<SettingsSourceReport> {
    let mut sources = vec![SettingsSourceReport {
        origin: "global",
        path: crate::config::config_dir()
            .join("cake")
            .join("settings.toml")
            .display()
            .to_string(),
        present: false,
    }];
    if mode.loads_project_config() {
        sources.push(SettingsSourceReport {
            origin: "project",
            path: current_dir
                .join(".cake")
                .join("settings.toml")
                .display()
                .to_string(),
            present: false,
        });
    }
    for source in &mut sources {
        source.present = Path::new(&source.path).exists();
    }
    sources
}

/// Load settings, keeping the failure report actionable.
///
/// The loader reports one failure at a time, so the document carries the
/// settings files that were read beside the failure. The original error is
/// still returned, which keeps its exit classification.
fn load_settings_for_doctor(
    json_output: bool,
    sources: &[SettingsSourceReport],
    current_dir: &Path,
    options: &CommandRunOptions<'_>,
) -> anyhow::Result<SettingsLoad> {
    match SettingsLoader::load_with_project_config(
        current_dir,
        options.profile,
        options.no_gate,
        &crate::config::config_dir(),
        options.project_config_mode,
    ) {
        Ok(load) => Ok(load),
        Err(error) => {
            let error: anyhow::Error = error.into();
            let failed = failed_source(sources);
            let message = failure_message(&error, sources, failed.as_deref());
            if json_output {
                DiagnosticDocument::new(
                    "doctor",
                    json!({
                        "settings_source_count": sources.len(),
                        "warning_count": 0,
                    }),
                    vec![DiagnosticCheck::error(CHECK_SETTINGS_LOAD_FAILED, message)],
                    json!({
                        "settings_sources": sources,
                        "failed_source": failed,
                    }),
                )
                .print();
            } else {
                eprint!("{}", format_settings_sources(sources, None));
            }
            Err(error)
        },
    }
}

/// The settings file whose own parse or read fails, when one does.
///
/// A semantic failure merges first and fails later, so no single file owns it;
/// the caller then names every source it checked instead.
fn failed_source(sources: &[SettingsSourceReport]) -> Option<String> {
    sources
        .iter()
        .find(|source| {
            let origin = if source.origin == "global" {
                SettingsOrigin::Global
            } else {
                SettingsOrigin::Project
            };
            SettingsLoader::read_source(Path::new(&source.path), origin).is_err()
        })
        .map(|source| source.path.clone())
}

fn failure_message(
    error: &anyhow::Error,
    sources: &[SettingsSourceReport],
    failed: Option<&str>,
) -> String {
    if let Some(path) = failed {
        return format!("{path}: {error}");
    }
    let paths: Vec<&str> = sources.iter().map(|source| source.path.as_str()).collect();
    format!("{error} (settings sources: {})", paths.join(", "))
}

fn render_json(
    load: &SettingsLoad,
    sources: &[SettingsSourceReport],
    configured: &ConfiguredGrants,
    report: &GrantReport,
    profile: Option<&str>,
) -> anyhow::Result<String> {
    let loaded = &load.settings;
    let mut checks = settings_warnings(&loaded.warnings);
    checks.extend(
        ConfiguredGrants::warnings(report)
            .into_iter()
            .map(|warning| DiagnosticCheck::warning(CHECK_GRANTS_PATH, warning)),
    );

    DiagnosticDocument::new(
        "doctor",
        json!({
            "settings_source_count": sources.len(),
            "warning_count": checks.len(),
            "profile": profile,
            "model": loaded.default_model,
            "policy": configured.policy,
            "policy_source": configured.policy_source,
            "configured_writable_count": configured.writable.len(),
            "configured_read_only_count": configured.read_only.len(),
        }),
        checks,
        json!({
            "settings_sources": sources,
            "profile": profile,
            "model": loaded.default_model,
            "grants": configured,
            "project_trust": trust_payload(&load.trust),
        }),
    )
    .render()
}

fn trust_payload(trust: &ProjectTrustReport) -> Value {
    json!({
        "state": trust.state.label(),
        "authority": trust.authority,
        "changes": trust.changes,
    })
}

fn render_text(
    load: &SettingsLoad,
    sources: &[SettingsSourceReport],
    configured: &ConfiguredGrants,
    report: &GrantReport,
    profile: Option<&str>,
) -> String {
    let loaded = &load.settings;
    let warning_count = loaded.warnings.len() + ConfiguredGrants::warnings(report).len();
    let mut output = format_settings_sources(sources, profile);
    _ = writeln!(
        output,
        "  Model: {}",
        loaded.default_model.as_deref().unwrap_or("(none)")
    );
    if warning_count == 0 {
        output.push_str("  Status: ok\n");
    } else {
        _ = writeln!(
            output,
            "  Status: warning ({warning_count} finding(s); see stderr)"
        );
    }

    output.push_str("\nConfigured grants\n");
    _ = writeln!(
        output,
        "  Policy: {} (from {})",
        configured.policy, configured.policy_source
    );
    append_grants(&mut output, "Writable", &configured.writable);
    append_grants(&mut output, "Read-only", &configured.read_only);
    if configured.writable.is_empty() && configured.read_only.is_empty() {
        output.push_str("  (no configured grant settings)\n");
    }
    output.push_str(
        "  These are the configured grant settings resolved for this project; they are not the complete OS-level sandbox policy.\n",
    );
    output
}

fn format_settings_sources(sources: &[SettingsSourceReport], profile: Option<&str>) -> String {
    let mut output = String::from("Settings health\n");
    for source in sources {
        let label = if source.origin == "global" {
            "Global settings"
        } else {
            "Project settings"
        };
        _ = writeln!(
            output,
            "  {label}: {} ({})",
            source.path,
            if source.present { "present" } else { "missing" }
        );
    }
    if let Some(profile) = profile {
        _ = writeln!(output, "  Profile: {profile}");
    } else {
        output.push_str("  Profile: (none)\n");
    }
    output
}

fn append_grants(output: &mut String, title: &str, entries: &[ConfiguredGrant]) {
    _ = writeln!(output, "  {title}");
    if entries.is_empty() {
        output.push_str("    (none)\n");
        return;
    }
    for entry in entries {
        _ = write!(
            output,
            "    [{}] {} ({})",
            entry.source, entry.path, entry.status
        );
        if let Some(detail) = &entry.detail {
            _ = write!(output, ": {detail}");
        }
        output.push('\n');
    }
}
