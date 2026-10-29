//! Inspect configured external gates.

use std::fmt::Write as _;

use clap::{Parser, Subcommand};
use serde::Serialize;
use serde_json::json;

use crate::cli::{
    CHECK_SETTINGS_LOAD_FAILED, CmdRunner, CommandRunOptions, DiagnosticDocument, report_failure,
    settings_warnings,
};
use crate::config::gates::GateSettings;
use crate::config::settings::LoadedSettings;
use crate::config::{DataDir, SettingsLoader};
use crate::gates::composer::GatePipelineError;

/// List and validate the configured external gates.
#[derive(Clone, Debug, Parser)]
#[command(after_help = "\
Examples:
  cake gates list
  cake gates list --json | jq '.data.gates'
  cake gates doctor")]
pub struct GatesCommand {
    #[command(subcommand)]
    command: GatesSubcommand,
}

#[derive(Clone, Debug, Subcommand)]
enum GatesSubcommand {
    /// Show the selected gate configuration without starting a gate
    List {
        /// Output one diagnostic JSON document
        #[arg(long)]
        json: bool,
    },
    /// Start configured gates, validate their protocol and coverage, then stop
    Doctor {
        /// Output one diagnostic JSON document
        #[arg(long)]
        json: bool,
    },
}

impl CmdRunner for GatesCommand {
    async fn run(
        &self,
        _data_dir: &DataDir,
        options: &CommandRunOptions<'_>,
    ) -> anyhow::Result<()> {
        match &self.command {
            GatesSubcommand::List { json } => run_list(*json, options),
            GatesSubcommand::Doctor { json } => run_doctor(*json, options).await,
        }
    }
}

fn load_settings(
    command: &'static str,
    json_output: bool,
    options: &CommandRunOptions<'_>,
) -> anyhow::Result<(LoadedSettings, std::path::PathBuf)> {
    let cwd = std::env::current_dir()
        .map_err(|error| anyhow::anyhow!("Failed to get current directory: {error}"))?;
    let loaded =
        SettingsLoader::load_with_profile(Some(&cwd), options.profile).map_err(|error| {
            report_failure(
                json_output,
                command,
                CHECK_SETTINGS_LOAD_FAILED,
                error.into(),
            )
        })?;
    Ok((loaded, cwd))
}

fn run_list(json_output: bool, options: &CommandRunOptions<'_>) -> anyhow::Result<()> {
    let (loaded, _) = load_settings("gates list", json_output, options)?;
    let (bypassed, gates) = list_gates(&loaded, options.no_gate);
    if json_output {
        print!("{}", render_list_json(&loaded, bypassed, &gates)?);
    } else {
        loaded.print_warnings();
        print!("{}", render_list_text(bypassed, &gates));
    }
    Ok(())
}

fn list_gates(loaded: &LoadedSettings, no_gate: bool) -> (bool, Vec<GateListing>) {
    let bypassed = loaded.gates.bypassed(&loaded.judge, no_gate);
    let gates = if bypassed {
        Vec::new()
    } else {
        loaded
            .gates
            .selected(&loaded.judge)
            .into_iter()
            .map(|(name, settings)| GateListing::new(name, settings))
            .collect()
    };
    (bypassed, gates)
}

fn render_list_json(
    loaded: &LoadedSettings,
    bypassed: bool,
    gates: &[GateListing],
) -> anyhow::Result<String> {
    DiagnosticDocument::new(
        "gates list",
        json!({ "count": gates.len(), "bypassed": bypassed }),
        settings_warnings(&loaded.warnings),
        json!({ "bypassed": bypassed, "gates": gates }),
    )
    .render()
}

fn render_list_text(bypassed: bool, gates: &[GateListing]) -> String {
    let mut output = String::new();
    if bypassed {
        output.push_str("Gate pipeline: bypassed\n");
        return output;
    }
    output.push_str("Gate pipeline:\n");
    for gate in gates {
        writeln!(
            output,
            "  {}: command={} mode={} transport={} phases=pre_tool actions=bash.exec",
            gate.name,
            gate.command.as_deref().unwrap_or("<missing>"),
            gate.mode,
            gate.transport
        )
        .unwrap_or_default();
    }
    output
}

async fn run_doctor(json_output: bool, options: &CommandRunOptions<'_>) -> anyhow::Result<()> {
    let (loaded, cwd) = load_settings("gates doctor", json_output, options)?;
    let model = super::bash::selected_model(&loaded, options.model);
    let pipeline = crate::gates::runtime::start(
        &loaded.gates,
        &loaded.judge,
        &loaded.models,
        model.as_ref(),
        &cwd,
        options.no_gate,
    )
    .await
    .map_err(|error| report_gate_failure(json_output, error))?;
    let bypassed = pipeline.is_bypass();
    let records = pipeline.startup_records();
    pipeline.shutdown().await;

    if json_output {
        print!("{}", render_doctor_json(&loaded, bypassed, &records)?);
    } else {
        loaded.print_warnings();
        print!("{}", render_doctor_text(bypassed, &records));
    }
    Ok(())
}

fn render_doctor_json(
    loaded: &LoadedSettings,
    bypassed: bool,
    records: &[crate::session_telemetry::GateStartupTelemetry],
) -> anyhow::Result<String> {
    DiagnosticDocument::new(
        "gates doctor",
        json!({ "count": records.len(), "bypassed": bypassed }),
        settings_warnings(&loaded.warnings),
        json!({ "bypassed": bypassed, "gates": records }),
    )
    .render()
}

fn render_doctor_text(
    bypassed: bool,
    records: &[crate::session_telemetry::GateStartupTelemetry],
) -> String {
    if bypassed {
        return "Gate checks are bypassed; Bash runs under its OS sandbox and every call records a bypass.\n".to_string();
    }
    if records.is_empty() {
        return "No configured gates.\n".to_string();
    }
    let mut output = String::new();
    for gate in records {
        writeln!(
            output,
            "{}: {} mode={} phases={} actions={} latency={}ms",
            gate.gate,
            gate.outcome,
            gate.mode,
            display_list(&gate.phases),
            display_list(&gate.actions),
            gate.latency_ms
        )
        .unwrap_or_default();
    }
    output
}

fn report_gate_failure(json_output: bool, error: GatePipelineError) -> anyhow::Error {
    let check_id = super::bash::gate_error_check_id(&error);
    report_failure(
        json_output,
        "gates doctor",
        check_id,
        anyhow::Error::new(error),
    )
}

fn display_list(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_string()
    } else {
        values.join(",")
    }
}

#[derive(Debug, Serialize)]
struct GateListing {
    name: String,
    command: Option<String>,
    args: Vec<String>,
    mode: String,
    transport: String,
    timeout_secs: Option<u64>,
    models: Vec<String>,
    env: Vec<String>,
}

impl GateListing {
    fn new(name: String, settings: GateSettings) -> Self {
        Self {
            name,
            command: settings.command,
            args: settings.args.unwrap_or_default(),
            mode: settings.mode.unwrap_or_else(|| "enforce".to_string()),
            transport: settings.transport.unwrap_or_else(|| "stdio".to_string()),
            timeout_secs: settings.timeout_secs,
            models: settings.models.unwrap_or_default(),
            env: settings.env.unwrap_or_default(),
        }
    }
}

#[cfg(test)]
#[path = "gates_tests.rs"]
mod tests;
