//! `cake doctor`: grouped local settings, model, sandbox, and trust health.
//!
//! The command validates the global and project settings files and the
//! selected profile through the ordinary settings loader, so it reports the
//! same unknown-key round-trip findings, parse failures, and semantic failures
//! a run would see. It then resolves the configured `directories`,
//! `[sandbox].writable`, and `[sandbox].read_only` grants through the same
//! [`crate::clients::GrantReport`] value `cake debug grants` renders.
//!
//! The report has fixed groups with visible passing checks, and every non-ok
//! check carries a structured fix hint. Credential reporting inspects only
//! whether the selected model's configured `api_key_env` variable is present;
//! the value is never resolved, stored, or emitted.
//!
//! It is read-only: it starts no session, calls no provider or gate, executes
//! no hook or toolbox `describe`, connects no MCP server, and creates no
//! configured directory. The configured grants it prints are the settings
//! values for this project, not the complete OS-level sandbox policy.

use crate::cli::debug::grants;
use crate::cli::report::{SelectedModel, project_trust_payload};
use crate::cli::{CmdRunner, CommandRunOptions};
use crate::clients::GrantReport;
use crate::config::DataDir;
use crate::config::SettingsLoader;
use crate::config::settings::{LoadedSettings, ModelDefinition, SettingsLoad, SettingsOrigin};
use crate::config::trust::ProjectConfigMode;
use clap::Parser;
use serde::Serialize;
use serde_json::json;
use std::path::Path;

mod checks;
mod render;
mod report;

/// Inspect grouped local settings, model, sandbox, and project health.
#[derive(Clone, Debug, Parser)]
#[command(after_help = "\
Examples:
  cake doctor
  cake doctor --json | jq '.data.groups'
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
}

/// The selected model and, when it is defined, its credential presence.
struct ModelFact {
    name: Option<String>,
    source: Option<&'static str>,
    configured: bool,
    credential: Option<CredentialFact>,
}

impl ModelFact {
    /// Select `--model` first, then `default_model`, and inspect presence.
    fn build(loaded: &LoadedSettings, selected: Option<&str>) -> Self {
        let SelectedModel {
            name,
            source,
            definition,
        } = SelectedModel::resolve(loaded, selected);
        let credential = definition.map(credential_fact);
        Self {
            name,
            source,
            configured: definition.is_some(),
            credential,
        }
    }
}

/// Credential presence for one configured model.
#[derive(Debug, Serialize)]
struct CredentialFact {
    /// Credential source label; always `environment`.
    source: &'static str,
    /// Environment variable name; never the value.
    name: String,
    present: bool,
}

/// Collected inputs for the success report.
struct DoctorFacts<'a> {
    workspace: &'a Path,
    sources: &'a [SettingsSourceReport],
    load: &'a SettingsLoad,
    profile: Option<&'a str>,
    project_config_mode: ProjectConfigMode,
    model: ModelFact,
    grants: &'a GrantReport,
}

/// Collected inputs for the settings-load failure report.
struct DoctorFailureFacts<'a> {
    workspace: &'a Path,
    sources: &'a [SettingsSourceReport],
    profile: Option<&'a str>,
    failed_source: Option<&'a str>,
    message: String,
}

/// A settings load failure plus the report material that names it.
struct DoctorLoadFailure {
    error: anyhow::Error,
    failed_source: Option<String>,
    message: String,
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
    match load_settings_for_doctor(&sources, &current_dir, options) {
        Ok(load) => run_success(
            json_output,
            data_dir,
            options,
            &current_dir,
            &sources,
            &load,
        ),
        Err(failure) => run_failure(json_output, options, &current_dir, &sources, failure),
    }
}

fn run_success(
    json_output: bool,
    data_dir: &DataDir,
    options: &CommandRunOptions<'_>,
    current_dir: &Path,
    sources: &[SettingsSourceReport],
    load: &SettingsLoad,
) -> anyhow::Result<()> {
    let grant_report =
        grants::build_report(None, &[], &load.settings, Vec::new(), current_dir, data_dir);
    let configured = ConfiguredGrants::from_report(&grant_report);
    let facts = DoctorFacts {
        workspace: current_dir,
        sources,
        load,
        profile: options.profile,
        project_config_mode: options.project_config_mode,
        model: ModelFact::build(&load.settings, options.model),
        grants: &grant_report,
    };
    let report = checks::build_success(&facts);
    let data = json!({
        "settings_sources": sources,
        "profile": options.profile,
        "model": facts.model.name.as_deref(),
        "grants": configured,
        "project_trust": project_trust_payload(&load.trust),
        "default_model": load.settings.default_model.as_deref(),
        "model_source": facts.model.source,
        "model_configured": facts.model.configured,
        "credential": facts.model.credential.as_ref(),
    });
    let output = if json_output {
        render::render_json(&report, data)?
    } else {
        render::render_text(&report)
    };
    print!("{output}");
    Ok(())
}

fn run_failure(
    json_output: bool,
    options: &CommandRunOptions<'_>,
    current_dir: &Path,
    sources: &[SettingsSourceReport],
    failure: DoctorLoadFailure,
) -> anyhow::Result<()> {
    let facts = DoctorFailureFacts {
        workspace: current_dir,
        sources,
        profile: options.profile,
        failed_source: failure.failed_source.as_deref(),
        message: failure.message,
    };
    let report = checks::build_failure(&facts);
    let data = json!({
        "settings_sources": sources,
        "failed_source": facts.failed_source,
    });
    let output = if json_output {
        render::render_json(&report, data)?
    } else {
        render::render_text(&report)
    };
    print!("{output}");
    Err(failure.error)
}

fn credential_fact(definition: &ModelDefinition) -> CredentialFact {
    let env_name = definition.api_key_env.clone();
    let present = std::env::var(&env_name).is_ok_and(|value| !value.is_empty());
    CredentialFact {
        source: "environment",
        name: env_name,
        present,
    }
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

/// Load settings without printing; the caller owns both report streams.
fn load_settings_for_doctor(
    sources: &[SettingsSourceReport],
    current_dir: &Path,
    options: &CommandRunOptions<'_>,
) -> Result<SettingsLoad, DoctorLoadFailure> {
    SettingsLoader::load_with_project_config(
        current_dir,
        options.profile,
        options.no_gate,
        &crate::config::config_dir(),
        options.project_config_mode,
    )
    .map_err(|error| {
        let error: anyhow::Error = error.into();
        let failed_source = failed_source(sources);
        let message = failure_message(&error, sources, failed_source.as_deref());
        DoctorLoadFailure {
            error,
            failed_source,
            message,
        }
    })
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
