//! `cake bash` inspection commands backed by the configured gate pipeline.

use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use crate::cli::{
    CmdRunner, CommandRunOptions, DiagnosticCheck, DiagnosticDocument, settings_warnings,
};
use crate::config::settings::LoadedSettings;
use crate::config::{DataDir, ModelConfig, SettingsLoader};
use crate::gates::composer::{
    GateMode, GatePipeline, GatePipelineError, GateRequest, PipelineAuthorization, PipelineOutcome,
    StageOutcome,
};
use crate::gates::protocol::{BashInput, GateDecision};

/// Inspect Bash command decisions without executing commands.
#[derive(Clone, Debug, Parser)]
#[command(after_help = "\
Examples:
  cake bash check -- 'git push --force origin master'
  cake bash check --json -- 'rm -rf build' | jq -r '.data.verdict'")]
pub struct BashCommand {
    #[command(subcommand)]
    command: BashSubcommand,
}

#[derive(Clone, Debug, Subcommand)]
enum BashSubcommand {
    /// Ask the configured gate pipeline about a command without executing it
    Check(BashCheckCommand),
}

#[derive(Clone, Debug, Parser)]
#[command(after_help = "\
Examples:
  cake bash check -- 'git push --force origin master'
  cake bash check --json -- 'rm -rf build' | jq '.data'
  cake bash check --diagnostic -- 'printf %s secret' ")]
struct BashCheckCommand {
    /// Request raw judge prompts and provider response (no longer supported)
    #[arg(long, conflicts_with = "json")]
    diagnostic: bool,
    /// Report the gate decision as one diagnostic JSON document
    #[arg(long)]
    json: bool,
    /// The raw command text to evaluate
    #[arg(value_name = "COMMAND")]
    command: String,
}

impl CmdRunner for BashCommand {
    async fn run(
        &self,
        _data_dir: &DataDir,
        options: &CommandRunOptions<'_>,
    ) -> anyhow::Result<()> {
        match &self.command {
            BashSubcommand::Check(check) => run_bash_check_command(check, options).await,
        }
    }
}

async fn run_bash_check_command(
    check: &BashCheckCommand,
    options: &CommandRunOptions<'_>,
) -> anyhow::Result<()> {
    if check.diagnostic {
        anyhow::bail!(
            "`cake bash check --diagnostic` was removed with the in-process judge; use `cake-safety-judge check --diagnostic` for standalone diagnostics"
        );
    }

    let current_dir = std::env::current_dir()
        .map_err(|error| anyhow::anyhow!("Failed to get current directory: {error}"))?;
    let loaded = SettingsLoader::load_with_profile(Some(&current_dir), options.profile)?;

    let outcome = run_bash_check(
        &loaded,
        &current_dir,
        &check.command,
        options.model,
        options.no_gate,
    )
    .await
    .inspect_err(|_| {
        if check.json {
            loaded.print_warnings();
        }
    })?;

    if check.json {
        print!(
            "{}",
            outcome.render_json(&settings_warnings(&loaded.warnings))?
        );
    } else {
        loaded.print_warnings();
        print!("{}", outcome.render_text());
    }
    Ok(())
}

async fn run_bash_check(
    loaded: &LoadedSettings,
    cwd: &Path,
    command: &str,
    cli_model: Option<&str>,
    no_gate: bool,
) -> anyhow::Result<CheckOutcome> {
    let model = selected_model(loaded, cli_model);
    let pipeline = crate::gates::runtime::start(
        &loaded.gates,
        &loaded.judge,
        &loaded.models,
        model.as_ref(),
        cwd,
        no_gate,
    )
    .await?;
    evaluate_pipeline(&pipeline, cwd, command).await
}

/// Resolve only a model descriptor. The gate runner receives the environment
/// variable name and resolves the credential in its own process.
pub(super) fn selected_model(
    loaded: &LoadedSettings,
    cli_model: Option<&str>,
) -> Option<ModelConfig> {
    let name = cli_model.or(loaded.default_model.as_deref())?;
    loaded
        .models
        .get(name)
        .map(crate::config::settings::ModelDefinition::to_model_config)
}

async fn evaluate_pipeline(
    pipeline: &GatePipeline,
    cwd: &Path,
    command: &str,
) -> anyhow::Result<CheckOutcome> {
    let input = BashInput::new(command.to_string(), cwd.to_string_lossy().into_owned());
    let result = pipeline
        .check(GateRequest::new(input))
        .await
        .map_err(anyhow::Error::new);
    pipeline.shutdown().await;
    Ok(CheckOutcome { outcome: result? })
}

struct CheckOutcome {
    outcome: PipelineOutcome,
}

impl CheckOutcome {
    fn render_text(&self) -> String {
        let outcome = &self.outcome;
        let verdict = summary_verdict(outcome);
        let stage = deciding_stage(outcome);
        let mut text = format!("Verdict: {verdict}\n");
        if let Some(stage) = stage {
            writeln!(text, "Gate: {}", stage.gate).unwrap_or_default();
            if let Some(code) = stage.code.as_deref().filter(|code| !code.is_empty()) {
                writeln!(text, "Code: {code}").unwrap_or_default();
            }
            if stage.overridden {
                text.push_str("Overridden: allowlist\n");
            }
            if let Some(confidence) = stage.confidence {
                writeln!(text, "Confidence: {confidence}").unwrap_or_default();
            }
            if !stage.message.is_empty() {
                writeln!(text, "Message: {}", stage.message).unwrap_or_default();
            }
        } else if outcome.bypassed {
            text.push_str("Message: gate checks were explicitly bypassed.\n");
        } else {
            text.push_str("Message: no enforce gate expressed a decision.\n");
        }
        writeln!(text, "Latency: {:.2}s", outcome.latency.as_secs_f32()).unwrap_or_default();
        text
    }

    fn render_json(&self, checks: &[DiagnosticCheck]) -> anyhow::Result<String> {
        let summary = json!({"verdict": summary_verdict(&self.outcome)});
        let data = outcome_data(&self.outcome);
        DiagnosticDocument::new("bash check", summary, checks.to_vec(), data).render()
    }
}

fn summary_verdict(outcome: &PipelineOutcome) -> &'static str {
    if outcome.bypassed {
        return "bypassed";
    }
    if let Some(decision) = outcome.verdict {
        return decision.as_str();
    }
    if overridden_block(outcome).is_some() {
        return "block";
    }
    authorization_label(&outcome.authorization)
}

const fn authorization_label(authorization: &PipelineAuthorization) -> &'static str {
    match authorization {
        PipelineAuthorization::Run => "allow",
        PipelineAuthorization::Block { .. } => "block",
    }
}

fn outcome_data(outcome: &PipelineOutcome) -> Value {
    let stage = deciding_stage(outcome);
    let (code, confidence, message, overridden) = if outcome.bypassed {
        (None, None, "gate checks were explicitly bypassed.", false)
    } else if let Some(stage) = stage {
        (
            stage.code.as_deref().filter(|code| !code.is_empty()),
            stage.confidence,
            stage.message.as_str(),
            stage.overridden,
        )
    } else {
        (None, None, "no enforce gate expressed a decision.", false)
    };
    let verdict = if outcome.bypassed {
        None
    } else if let Some(decision) = outcome.verdict {
        Some(decision.as_str())
    } else if overridden_block(outcome).is_some() {
        Some("block")
    } else {
        None
    };
    let gate = stage.map(|stage| stage.gate.as_str());
    let stages = outcome.stages.iter().map(stage_data).collect::<Vec<_>>();
    json!({
        "verdict": verdict,
        "code": code,
        "confidence": confidence,
        "message": message,
        "overridden": overridden,
        "bypassed": outcome.bypassed,
        "latency_ms": duration_millis(outcome.latency),
        "stage": gate.map(|_| "gate"),
        "probability": Value::Null,
        "observation": {
            "outcome": "absent",
            "probability": Value::Null,
            "failure_class": Value::Null,
            "elapsed_ms": Value::Null,
        },
        "gate": gate,
        "stages": stages,
    })
}

fn deciding_stage(outcome: &PipelineOutcome) -> Option<&StageOutcome> {
    if let Some(gate) = outcome.deciding_gate.as_ref()
        && let Some(stage) = outcome.stages.iter().find(|stage| &stage.gate == gate)
    {
        return Some(stage);
    }
    overridden_block(outcome)
}

fn overridden_block(outcome: &PipelineOutcome) -> Option<&StageOutcome> {
    outcome.stages.iter().find(|stage| {
        stage.mode == GateMode::Enforce
            && stage.overridden
            && stage.decision == Some(GateDecision::Block)
    })
}

fn stage_data(stage: &StageOutcome) -> Value {
    json!({
        "gate": stage.gate,
        "version": stage.version,
        "mode": stage.mode.as_str(),
        "decision": stage.decision.map(GateDecision::as_str),
        "code": stage.code,
        "latency_ms": duration_millis(stage.latency),
        "overridden": stage.overridden,
        "failure_class": stage.failure.as_ref().map(|failure| failure.class.as_str()),
        "failed_open": stage.failed_open,
        "disabled": stage.disabled,
    })
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

const GATE_CHECK_IDS: [(&str, &str); 11] = [
    ("config", "gates.configuration"),
    ("timeout", "gates.timeout"),
    ("network", "gates.network"),
    ("auth", "gates.authentication"),
    ("rate_limit", "gates.rate_limit"),
    ("transport", "gates.transport"),
    ("malformed", "gates.malformed"),
    ("refusal", "gates.refusal"),
    ("unsupported", "gates.unsupported"),
    ("internal", "gates.internal"),
    ("crash", "gates.crash"),
];

pub(super) fn gate_error_check_id(error: &GatePipelineError) -> &'static str {
    let class = error.class();
    GATE_CHECK_IDS
        .iter()
        .find(|(name, _)| *name == class.as_str())
        .map_or("gates.protocol", |(_, check_id)| check_id)
}

#[cfg(test)]
#[path = "bash_tests.rs"]
mod tests;
