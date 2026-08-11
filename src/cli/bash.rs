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
use crate::clients::judge::{
    JudgeClient, JudgeError, JudgeEvaluation, JudgeOutcome, JudgeRequest, JudgeVerdict,
    evaluate_command, evaluate_command_observed, judge_is_enabled, read_user_rubric,
    repo_state_digest, resolve_judge_client_config,
};
use crate::config::settings::{JUDGE_BYPASS_ENV, JudgeSettings, LoadedSettings};
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
    /// Show the sensitive effective prompts, request JSON, and parsed response
    #[arg(long)]
    diagnostic: bool,
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
                let output = run_bash_check_with_diagnostic(
                    &loaded,
                    &current_dir,
                    &check.command,
                    options.model,
                    check.diagnostic,
                )
                .await?;
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
#[cfg(test)]
async fn run_bash_check(
    loaded: &LoadedSettings,
    cwd: &Path,
    command: &str,
    cli_model: Option<&str>,
) -> anyhow::Result<String> {
    run_bash_check_with_diagnostic(loaded, cwd, command, cli_model, false).await
}

async fn run_bash_check_with_diagnostic(
    loaded: &LoadedSettings,
    cwd: &Path,
    command: &str,
    cli_model: Option<&str>,
    include_raw_diagnostic: bool,
) -> anyhow::Result<String> {
    // The emergency bypass short-circuits before any judge setup: a disabled
    // judge must not fail on an unusable model or rubric, because the bypass
    // is the recovery path when judge configuration is broken.
    let bypass_env = std::env::var(JUDGE_BYPASS_ENV).ok();
    if !judge_is_enabled(&loaded.judge, bypass_env.as_deref()) {
        return Ok(render_outcome(&JudgeOutcome::Bypassed, Duration::ZERO));
    }
    let model = resolve_judge_model(loaded, cli_model)?;
    let client = JudgeClient::new(model, Duration::from_secs(loaded.judge.timeout_secs))
        .with_user_rubric(read_user_rubric(&loaded.judge).map_err(anyhow::Error::msg)?);
    if include_raw_diagnostic {
        evaluate_with_client_diagnostic(client, &loaded.judge, bypass_env.as_deref(), cwd, command)
            .await
    } else {
        evaluate_with_client(client, &loaded.judge, bypass_env.as_deref(), cwd, command).await
    }
}

/// Evaluate one command with an already-configured judge client and render the
/// outcome. Exposed to tests so they can drive the exact `cake bash check`
/// path against a stub judge without touching settings or spawning processes.
///
/// The command runs through the full judge path (bypass check, allowlist
/// override), so the rendered output reflects what the Bash preflight will do.
/// `bypass_env` is the `CAKE_JUDGE` value passed to the judge path; tests pass
/// an explicit value so they are hermetic against the ambient environment.
async fn evaluate_with_client(
    client: JudgeClient,
    settings: &JudgeSettings,
    bypass_env: Option<&str>,
    cwd: &Path,
    command: &str,
) -> anyhow::Result<String> {
    let request = JudgeRequest::new(command.to_string(), cwd.to_path_buf(), None)
        .with_repo_digest(repo_state_digest(cwd));

    let started = Instant::now();
    let outcome = evaluate_command(&client, settings, request, bypass_env).await?;
    let latency = started.elapsed();

    Ok(render_outcome(&outcome, latency))
}

async fn evaluate_with_client_diagnostic(
    client: JudgeClient,
    settings: &JudgeSettings,
    bypass_env: Option<&str>,
    cwd: &Path,
    command: &str,
) -> anyhow::Result<String> {
    let api_key = client.api_key().to_string();
    let request = JudgeRequest::new(command.to_string(), cwd.to_path_buf(), None)
        .with_repo_digest(repo_state_digest(cwd));
    let evaluation = evaluate_command_observed(&client, settings, request, bypass_env, true).await;
    render_diagnostic_evaluation(evaluation, &api_key)
}

fn render_diagnostic_evaluation(
    evaluation: JudgeEvaluation,
    api_key: &str,
) -> anyhow::Result<String> {
    use std::fmt::Write as _;

    let JudgeEvaluation {
        outcome,
        attempts,
        diagnostic,
    } = evaluation;
    let Some(attempt) = attempts.last() else {
        let outcome = outcome?;
        return Ok(format!(
            "WARNING: raw judge diagnostics may contain command text, paths, repository state, reason text, and secrets embedded in those values.\n\n{}",
            redact_secret(&render_outcome(&outcome, Duration::ZERO), api_key)
        ));
    };
    let raw = diagnostic.ok_or_else(|| anyhow::anyhow!("judge diagnostic data was unavailable"))?;

    let mut out = String::from(
        "WARNING: raw judge diagnostics contain command text, paths, repository state, reason text, and may contain secrets embedded in those values. Handle this output as sensitive.\n\n",
    );
    _ = writeln!(out, "Resolved model: {}", attempt.model);
    _ = writeln!(out, "API type: {:?}", attempt.api_type);
    _ = writeln!(out, "Reasoning effort: {:?}", attempt.reasoning_effort);
    _ = writeln!(out, "Temperature: {:?}", attempt.temperature);
    _ = writeln!(out, "Top-p: {:?}", attempt.top_p);
    _ = writeln!(out, "Max output tokens: {:?}", attempt.max_output_tokens);
    _ = writeln!(
        out,
        "Reasoning max tokens: {:?}",
        attempt.reasoning_max_tokens
    );
    _ = writeln!(
        out,
        "Configured timeout: {}ms",
        attempt.configured_timeout_ms
    );
    _ = writeln!(out, "Tool count: {}", attempt.tool_count);
    _ = writeln!(out, "Tool choice: {:?}", attempt.tool_choice);
    _ = writeln!(out, "\nSystem prompt:\n{}", raw.system_prompt);
    _ = writeln!(out, "\nUser prompt:\n{}", raw.user_prompt);
    _ = writeln!(
        out,
        "\nTransformed request JSON:\n{}",
        serde_json::to_string_pretty(&raw.request_json)?
    );
    _ = writeln!(
        out,
        "\nParsed response:\n{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "assistant_content": raw.assistant_content,
            "usage": raw.usage,
            "termination": raw.termination,
        }))?
    );
    _ = writeln!(
        out,
        "\nAttempt metadata:\n{}",
        serde_json::to_string_pretty(attempt)?
    );
    match outcome {
        Ok(outcome) => {
            let rendered = render_outcome(&outcome, Duration::from_millis(attempt.total_ms));
            _ = write!(out, "\n{}", redact_secret(&rendered, api_key));
            Ok(out)
        },
        Err(error) => {
            let error = redact_judge_error(error, api_key);
            _ = write!(out, "\nJudge error: {error}\n");
            Err(anyhow::Error::new(error).context(out))
        },
    }
}

/// Replace every occurrence of a resolved API key with `<redacted>`. An empty
/// secret is left untouched so callers with no configured key are unaffected.
fn redact_secret(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        text.to_string()
    } else {
        text.replace(secret, "<redacted>")
    }
}

/// Redact the API key from a [`JudgeError`]'s human-readable fields while
/// preserving the concrete type, so exit-code classification via
/// `downcast_ref::<JudgeError>()` still works.
fn redact_judge_error(error: JudgeError, secret: &str) -> JudgeError {
    match error {
        JudgeError::Transport { status, detail } => JudgeError::Transport {
            status,
            detail: redact_secret(&detail, secret),
        },
        JudgeError::Malformed(message) => JudgeError::Malformed(redact_secret(&message, secret)),
        other => other,
    }
}

/// Resolve the judge model config: the `[tools.bash.judge] model` override if
/// set, otherwise the run's `--model` flag, otherwise `default_model`. The
/// override and the flags are `[[models]]` names; the shared resolution in
/// [`resolve_judge_client_config`] keeps this identical to the Bash preflight.
fn resolve_judge_model(
    loaded: &LoadedSettings,
    cli_model: Option<&str>,
) -> anyhow::Result<ResolvedModelConfig> {
    // `[tools.bash.judge] model` (a `[[models]]` name) wins, then `--model`,
    // then `default_model`; the shared resolution in
    // [`resolve_judge_client_config`] applies the override so this stays
    // identical to the Bash preflight.
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
    let default = ResolvedModelConfig::resolve(definition.to_model_config())?;
    resolve_judge_client_config(&loaded.judge, &default, &loaded.models).map_err(anyhow::Error::msg)
}

/// Render a judge-path outcome as human-readable inspection output on stdout.
fn render_outcome(outcome: &JudgeOutcome, latency: Duration) -> String {
    match outcome {
        JudgeOutcome::Bypassed => {
            "Verdict: bypassed\nMessage: the command-safety judge is disabled \
             (CAKE_JUDGE=off or [tools.bash.judge] enabled = false); no judge call was made.\n"
                .to_string()
        },
        JudgeOutcome::Verdict {
            verdict,
            overridden,
        } => render_verdict(verdict, *overridden, latency),
    }
}

/// Render a verdict as human-readable inspection output on stdout.
fn render_verdict(verdict: &JudgeVerdict, overridden: bool, latency: Duration) -> String {
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
    if overridden {
        _ = writeln!(out, "Overridden: allowlist");
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
