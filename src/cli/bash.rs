//! `cake bash` subcommands: judge introspection without executing anything.
//!
//! `cake bash check -- <command>` runs the same judge path and prompt the
//! Bash preflight will use (Milestone 5 of the LLM-judge `ExecPlan`), prints the
//! verdict, code, message, confidence, and latency, and never executes the
//! command. A judge error exits nonzero; a verdict is successful inspection
//! output. Follows the ADR-009 introspection pattern (load merged settings,
//! print to stdout, exit before agent/session setup).

use std::path::Path;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};

use crate::cli::{CmdRunner, CommandRunOptions};
use crate::clients::judge::{JudgeClient, JudgeRequest, JudgeVerdict, repo_state_digest};
use crate::config::settings::LoadedSettings;
use crate::config::{DataDir, ResolvedModelConfig, SettingsLoader};

/// Inspect and explain Bash command-safety decisions.
#[derive(Clone, Debug, Parser)]
pub struct BashCommand {
    #[command(subcommand)]
    command: BashSubcommand,
}

#[derive(Clone, Debug, Subcommand)]
enum BashSubcommand {
    /// Explain how the command-safety judge would decide a command, without
    /// executing it
    Check(BashCheckCommand),
}

#[derive(Clone, Debug, Parser)]
struct BashCheckCommand {
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
            BashSubcommand::Check(check) => {
                let current_dir = std::env::current_dir()
                    .map_err(|e| anyhow::anyhow!("Failed to get current directory: {e}"))?;
                let loaded =
                    SettingsLoader::load_with_profile(Some(&current_dir), options.profile)?;
                let output =
                    run_bash_check(&loaded, &current_dir, &check.command, options.model).await?;
                print!("{output}");
                Ok(())
            },
        }
    }
}

/// Run one `cake bash check` evaluation against the configured judge.
///
/// Resolves the judge model (the `[tools.bash.judge] model` override, falling
/// back to the `--model` flag, then the agent's `default_model`), appends any
/// configured user rubric file, and renders the verdict without executing
/// anything.
async fn run_bash_check(
    loaded: &LoadedSettings,
    cwd: &Path,
    command: &str,
    cli_model: Option<&str>,
) -> anyhow::Result<String> {
    let model = resolve_judge_model(loaded, cli_model)?;
    let client = JudgeClient::new(model, Duration::from_secs(loaded.judge.timeout_secs))
        .with_user_rubric(load_user_rubric(loaded)?);
    evaluate_with_client(client, cwd, command).await
}

/// Evaluate one command with an already-configured judge client and render the
/// verdict. Exposed to tests so they can drive the exact `cake bash check`
/// path against a stub judge without touching settings or spawning processes.
async fn evaluate_with_client(
    client: JudgeClient,
    cwd: &Path,
    command: &str,
) -> anyhow::Result<String> {
    let request = JudgeRequest::new(command.to_string(), cwd.to_path_buf(), None)
        .with_repo_digest(repo_state_digest(cwd));

    let started = Instant::now();
    let verdict = client.judge(request).await?;
    let latency = started.elapsed();

    Ok(render_verdict(&verdict, latency))
}

/// Resolve the judge model config: the `[tools.bash.judge] model` override if
/// set, otherwise the run's `--model` flag, otherwise the agent's
/// `default_model`. The settings override and the flags are `[[models]]` names.
fn resolve_judge_model(
    loaded: &LoadedSettings,
    cli_model: Option<&str>,
) -> anyhow::Result<ResolvedModelConfig> {
    let name = loaded
        .judge
        .model
        .as_deref()
        .or(cli_model)
        .or(loaded.default_model.as_deref())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No model specified for the safety judge. Set default_model (or \
                 [tools.bash.judge] model) in settings.toml with a [[models]] entry."
            )
        })?;
    let definition = loaded.models.get(name).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown model '{name}'. Use a model name from settings.toml, or set \
             default_model and omit the judge model."
        )
    })?;
    ResolvedModelConfig::resolve(definition.to_model_config())
}

/// Read the configured user rubric file, if any. A configured-but-unreadable
/// file is an error: the user asked for the guidance and the judge should not
/// silently judge without it.
fn load_user_rubric(loaded: &LoadedSettings) -> anyhow::Result<Option<String>> {
    let Some(path) = &loaded.judge.rubric_file else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Failed to read judge rubric file {}: {e}", path.display()))?;
    Ok(Some(text))
}

/// Render a verdict as human-readable inspection output on stdout.
fn render_verdict(verdict: &JudgeVerdict, latency: Duration) -> String {
    use std::fmt::Write as _;

    let decision = match verdict.decision {
        crate::clients::judge::JudgeDecision::Block => "block",
        crate::clients::judge::JudgeDecision::Warn => "warn",
        crate::clients::judge::JudgeDecision::Allow => "allow",
    };
    let mut out = format!("Verdict: {decision}\n");
    if let Some(code) = &verdict.code
        && !code.is_empty()
    {
        _ = writeln!(out, "Code: {code}");
    }
    if let Some(confidence) = verdict.confidence {
        _ = writeln!(out, "Confidence: {confidence}");
    }
    _ = writeln!(out, "Message: {}", verdict.message);
    _ = writeln!(out, "Latency: {:.2}s", latency.as_secs_f32());
    out
}

#[cfg(test)]
#[path = "bash_tests.rs"]
mod tests;
