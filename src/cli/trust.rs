//! `cake trust` management for authority-bearing project configuration.
//!
//! The command reads and hashes project `.cake/` files; it never parses hook
//! JSON into executable form, resolves a credential, or starts a process.

use std::fmt::Write as _;
use std::path::Path;

use clap::{ArgGroup, Parser};
use serde_json::json;

use crate::cli::{CmdRunner, CommandRunOptions, DiagnosticDocument, settings_warnings};
use crate::config::DataDir;
use crate::config::trust::{
    ProjectConfigMode, ProjectConfigSnapshot, ProjectTrustReport, TRUST_RECORD_VERSION,
    TrustRecord, TrustState, canonical_project_root, revoke_trust_record, trust_record_path,
};

/// Review, inspect, and revoke the trust record for a project.
#[derive(Clone, Debug, Parser)]
#[command(after_help = "\
Examples:
  cake trust
  cake trust --status
  cake trust --status --json | jq -r '.data.state'
  cake trust --check
  cake trust --revoke")]
#[command(group(
    ArgGroup::new("trust_action")
        .args(["status", "revoke", "check"])
        .multiple(false)
))]
#[expect(
    clippy::struct_excessive_bools,
    reason = "command-line action flags are inherently boolean"
)]
pub struct TrustCommand {
    /// Report the current trust state without writing
    #[arg(long)]
    status: bool,
    /// Remove the record for the current canonical project root
    #[arg(long)]
    revoke: bool,
    /// Exit nonzero unless the current snapshot is trusted
    #[arg(long)]
    check: bool,
    /// Emit one diagnostic JSON document (requires --status)
    #[arg(long, requires = "status", conflicts_with_all = ["revoke", "check"])]
    json: bool,
}

impl CmdRunner for TrustCommand {
    async fn run(
        &self,
        _data_dir: &DataDir,
        _options: &CommandRunOptions<'_>,
    ) -> anyhow::Result<()> {
        // CmdRunner is asynchronous, although this command has no async work.
        std::future::ready(()).await;
        let current_dir = std::env::current_dir()
            .map_err(|error| anyhow::anyhow!("Failed to get current directory: {error}"))?;
        let config_dir = crate::config::config_dir();
        if self.revoke {
            return run_revoke(&current_dir, &config_dir);
        }
        let snapshot =
            ProjectConfigSnapshot::load(&current_dir, &config_dir, ProjectConfigMode::Inspect)?;
        let report = snapshot.evaluate()?;
        if self.status {
            return run_status(&snapshot, &report, self.json);
        }
        if self.check {
            return report.require_trusted().map_err(anyhow::Error::new);
        }
        run_review(&snapshot, &report)
    }
}

fn run_status(
    snapshot: &ProjectConfigSnapshot,
    report: &ProjectTrustReport,
    json_output: bool,
) -> anyhow::Result<()> {
    if json_output {
        let mut data = json!({
            "state": report.state.label(),
            "project_root": report.project_root.to_string_lossy(),
            "project_key": report.project_key,
            "record_path": report.record_path.to_string_lossy(),
            "authority": report.authority,
            "changes": report.changes,
        });
        if let Some(reason) = &report.invalid_reason {
            data["invalid_reason"] = json!(reason);
        }
        let warnings = snapshot
            .settings
            .as_ref()
            .map_or_else(Vec::new, |settings| settings.warnings.clone());
        print!(
            "{}",
            DiagnosticDocument::new(
                "trust status",
                json!({
                    "state": report.state.label(),
                    "authority": report.authority_file_count(),
                    "changes": report.changes.len(),
                }),
                settings_warnings(&warnings),
                data,
            )
            .render()?
        );
        return Ok(());
    }
    print!("{}", render_status_text(report));
    Ok(())
}

fn render_status_text(report: &ProjectTrustReport) -> String {
    let mut output = String::new();
    writeln!(output, "Project trust: {}", report.state.label()).unwrap_or_default();
    writeln!(output, "Root: {}", report.project_root.display()).unwrap_or_default();
    writeln!(output, "Record: {}", report.record_path.display()).unwrap_or_default();
    if !report.authority.is_empty() {
        output.push_str("Authority-bearing configuration:\n");
        output.push_str(&render_authority(report));
    }
    if !report.changes.is_empty() {
        output.push_str("Changes since the trusted snapshot:\n");
        for change in &report.changes {
            writeln!(output, "  {}: {}", change.change.label(), change.path).unwrap_or_default();
        }
    }
    if let Some(reason) = &report.invalid_reason {
        writeln!(output, "Reason: {reason}").unwrap_or_default();
    }
    if matches!(report.state, TrustState::Untrusted | TrustState::Changed) {
        output.push_str(
            "\nRun `cake trust` to review and trust this snapshot.\n\
             Or re-run with --trust-project (CAKE_TRUST_PROJECT=1) for this invocation only.\n\
             Or re-run with --no-project-config (CAKE_PROJECT_CONFIG=off) to ignore project .cake configuration.\n",
        );
    }
    output
}

fn run_review(snapshot: &ProjectConfigSnapshot, report: &ProjectTrustReport) -> anyhow::Result<()> {
    if snapshot.authority.is_empty() {
        println!("No authority-bearing project configuration; no trust record is required.");
        return Ok(());
    }
    let record = TrustRecord {
        version: TRUST_RECORD_VERSION,
        project_root: report.project_root.to_string_lossy().into_owned(),
        files: snapshot.manifest.files.clone(),
    };
    TrustRecord::write_atomic(&report.record_path, &record)?;
    println!("Trusted project root: {}", report.project_root.display());
    println!("Authority-bearing configuration:");
    print_authority(report);
    print_review_details(snapshot);
    println!("Files in trust manifest: {}", record.files.len());
    println!("Trust record: {}", report.record_path.display());
    Ok(())
}

fn run_revoke(project_dir: &Path, config_dir: &Path) -> anyhow::Result<()> {
    let root = canonical_project_root(project_dir)?;
    let record_path = trust_record_path(config_dir, &root);
    if revoke_trust_record(config_dir, &root)? {
        println!("Revoked trust record: {}", record_path.display());
    } else {
        println!("No trust record for {}", root.display());
    }
    Ok(())
}

fn render_authority(report: &ProjectTrustReport) -> String {
    let mut output = String::new();
    for source in &report.authority {
        if source.keys.is_empty() {
            let executable = if source.executable {
                " (executable)"
            } else {
                ""
            };
            writeln!(output, "  {}{executable}", source.path).unwrap_or_default();
        } else {
            writeln!(output, "  {}: {}", source.path, source.keys.join(", ")).unwrap_or_default();
        }
    }
    output
}

fn print_authority(report: &ProjectTrustReport) {
    print!("{}", render_authority(report));
}

/// Show the commands a reviewer is accepting, without running any of them.
fn print_review_details(snapshot: &ProjectConfigSnapshot) {
    if let Some(source) = &snapshot.settings {
        let settings = &source.settings;
        if let Some(gates) = settings
            .tools
            .as_ref()
            .and_then(|tools| tools.bash.as_ref())
            .and_then(|bash| bash.gates.as_ref())
        {
            println!("  selected gates: {}", gates.join(", "));
        }
        let mut gates: Vec<(&String, &crate::config::gates::GateSettings)> =
            settings.gates.iter().collect();
        gates.sort_by(|left, right| left.0.cmp(right.0));
        for (name, gate) in gates {
            if let Some(command) = &gate.command {
                let args = gate
                    .args
                    .as_ref()
                    .map(|args| format!(" {}", args.join(" ")))
                    .unwrap_or_default();
                println!("  gate {name}: {command}{args}");
            }
        }
    }
    for hook in &snapshot.hook_files {
        let relative = hook.path.strip_prefix(&snapshot.project_root).map_or_else(
            |_| hook.path.display().to_string(),
            |path| path.display().to_string(),
        );
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&hook.bytes) else {
            println!("  {relative}: unreadable hook JSON (review the file)");
            continue;
        };
        let Some(events) = value.get("hooks").and_then(serde_json::Value::as_object) else {
            continue;
        };
        for (event, entries) in events {
            for entry in entries.as_array().into_iter().flatten() {
                for command in entry
                    .get("hooks")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(command) =
                        command.get("command").and_then(serde_json::Value::as_str)
                    {
                        println!("  {relative} [{event}]: {command}");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "trust_tests.rs"]
mod tests;
