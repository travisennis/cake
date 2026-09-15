//! `cake bash` subcommands: judge introspection without executing anything.
//!
//! `cake bash check -- <command>` runs the same judge path and prompt the
//! Bash preflight will use (Milestone 5 of the LLM-judge `ExecPlan`), prints the
//! verdict, code, message, confidence, and latency, and never executes the
//! command. `--json` reports the same verdict as one diagnostic document; a
//! judge error exits nonzero either way. A verdict is successful inspection
//! output. Follows the ADR-009 introspection pattern (load merged settings,
//! print to stdout, exit before agent/session setup).

use std::path::Path;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};

use crate::cli::{CmdRunner, CommandRunOptions, DiagnosticDocument};
use crate::clients::judge::{
    JudgeClient, JudgeDecision, JudgeError, JudgeEvaluation, JudgeOutcome, JudgeRequest,
    JudgeVerdict, evaluate_command, evaluate_command_observed, judge_is_enabled, read_user_rubric,
    repo_state_digest, resolve_judge_client_config,
};
use crate::config::settings::{JUDGE_BYPASS_ENV, JudgeSettings, LoadedSettings};
use crate::config::{DataDir, ResolvedModelConfig, SettingsLoader};
use serde::Serialize;

/// Inspect and explain Bash command-safety decisions.
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
    /// Explain how the command-safety judge would decide a command, without
    /// executing it
    Check(BashCheckCommand),
}

#[derive(Clone, Debug, Parser)]
#[command(after_help = "\
Examples:
  cake bash check -- 'git push --force origin master'
  cake bash check --json -- 'rm -rf build' | jq -r '.data.verdict'
  cake bash check --diagnostic -- 'printf %s \"$SECRET\"'")]
struct BashCheckCommand {
    /// Show the sensitive effective prompts, request JSON, and parsed response
    #[arg(long)]
    diagnostic: bool,
    /// Report the verdict as one diagnostic JSON document instead of text
    #[arg(long, conflicts_with = "diagnostic")]
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

/// Run one `cake bash check`, printing the verdict, the machine-readable
/// document, or the raw `--diagnostic` report to stdout.
async fn run_bash_check_command(
    check: &BashCheckCommand,
    options: &CommandRunOptions<'_>,
) -> anyhow::Result<()> {
    let current_dir = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("Failed to get current directory: {e}"))?;
    let loaded = SettingsLoader::load_with_profile(Some(&current_dir), options.profile)?;
    if check.diagnostic {
        run_bash_check_report(&loaded, &current_dir, check, options).await
    } else {
        run_bash_check_verdict(&loaded, &current_dir, check, options).await
    }
}

/// Run `cake bash check --diagnostic`: print the raw sensitive report.
///
/// The report always goes to stdout, even when the judge fails; the fail-closed
/// error is returned separately so it reaches stderr and the exit code without
/// duplicating the sensitive report there.
async fn run_bash_check_report(
    loaded: &LoadedSettings,
    cwd: &Path,
    check: &BashCheckCommand,
    options: &CommandRunOptions<'_>,
) -> anyhow::Result<()> {
    // Surface unrecognized settings keys on stderr, same as the run path, so a
    // typo in `[tools.bash.judge]` is reported here too.
    loaded.print_warnings();
    let report = run_bash_check_diagnostic(loaded, cwd, &check.command, options.model).await?;
    print!("{}", report.report);
    report
        .error
        .map_or_else(|| Ok(()), |error| Err(anyhow::Error::new(error)))
}

/// Run `cake bash check`: print the verdict as text or as one diagnostic
/// document.
///
/// Machine mode keeps stderr quiet so a consumer reads one document and
/// nothing else; the settings warnings are the text and `--diagnostic` modes'
/// to report.
async fn run_bash_check_verdict(
    loaded: &LoadedSettings,
    cwd: &Path,
    check: &BashCheckCommand,
    options: &CommandRunOptions<'_>,
) -> anyhow::Result<()> {
    if !check.json {
        loaded.print_warnings();
    }
    let outcome = run_bash_check(loaded, cwd, &check.command, options.model).await?;
    print!("{}", outcome.render(check.json)?);
    Ok(())
}

/// Run one `cake bash check` evaluation against the configured judge.
///
/// Resolves the judge model (the `[tools.bash.judge] model` override, falling
/// back to the `--model` flag, then the agent's `default_model`), appends any
/// configured user rubric file, and returns the outcome without executing
/// anything.
async fn run_bash_check(
    loaded: &LoadedSettings,
    cwd: &Path,
    command: &str,
    cli_model: Option<&str>,
) -> anyhow::Result<CheckOutcome> {
    let (client, bypass_env) = resolve_run_judge_client(loaded, cli_model)?;
    let Some(client) = client else {
        return Ok(CheckOutcome::bypassed());
    };
    evaluate_with_client(client, &loaded.judge, bypass_env.as_deref(), cwd, command).await
}

/// One judge outcome and the latency it was measured in.
///
/// Both output modes render this same value, so the human verdict and the
/// machine-readable document cannot disagree about what the judge decided.
struct CheckOutcome {
    outcome: JudgeOutcome,
    latency: Duration,
}

/// Why a `cake bash check` run made no judge call.
const JUDGE_BYPASS_MESSAGE: &str = "the command-safety judge is disabled \
     (CAKE_JUDGE=off or [tools.bash.judge] enabled = false); no judge call was made.";

impl CheckOutcome {
    /// The outcome of a disabled judge: no call was made and no time elapsed.
    const fn bypassed() -> Self {
        Self {
            outcome: JudgeOutcome::Bypassed,
            latency: Duration::ZERO,
        }
    }

    /// Render the outcome as the human-readable verdict.
    fn render_text(&self) -> String {
        render_outcome(&self.outcome, self.latency)
    }

    /// Render the outcome as the human-readable verdict or as one diagnostic
    /// document, which is the only difference between the two output modes.
    fn render(&self, json: bool) -> anyhow::Result<String> {
        if json {
            self.render_json()
        } else {
            Ok(self.render_text())
        }
    }

    /// Render the outcome as one diagnostic document.
    ///
    /// The latency is reported in whole milliseconds, which is the same
    /// measurement the text verdict prints as seconds.
    fn render_json(&self) -> anyhow::Result<String> {
        let latency_ms = u64::try_from(self.latency.as_millis()).unwrap_or(u64::MAX);
        let (summary_verdict, data) = match &self.outcome {
            JudgeOutcome::Bypassed => (
                "bypassed",
                VerdictData {
                    verdict: None,
                    code: None,
                    confidence: None,
                    message: JUDGE_BYPASS_MESSAGE,
                    overridden: false,
                    bypassed: true,
                    latency_ms: 0,
                },
            ),
            JudgeOutcome::Verdict {
                verdict,
                overridden,
            } => (
                decision_label(verdict.decision),
                VerdictData {
                    verdict: Some(decision_label(verdict.decision)),
                    code: verdict_code(verdict),
                    confidence: verdict.confidence,
                    message: &verdict.message,
                    overridden: *overridden,
                    bypassed: false,
                    latency_ms,
                },
            ),
        };
        DiagnosticDocument::new(
            "bash check",
            serde_json::json!({ "verdict": summary_verdict }),
            Vec::new(),
            data,
        )
        .render()
    }
}

/// The `bash check` payload: what the judge decided, as both output modes
/// report it.
#[derive(Debug, Serialize)]
struct VerdictData<'a> {
    /// The decision, or `null` when the judge is disabled.
    verdict: Option<&'static str>,
    /// The judge's stable verdict code, when it supplied a non-empty one.
    code: Option<&'a str>,
    confidence: Option<f32>,
    message: &'a str,
    /// Whether the allowlist overrode a block to allow the command.
    overridden: bool,
    /// Whether the judge is disabled and made no call.
    bypassed: bool,
    latency_ms: u64,
}

/// Run `cake bash check --diagnostic`: render the raw inspection report, which
/// always goes to stdout even when the judge fails; the fail-closed error is
/// carried separately so the caller can propagate it for exit classification.
async fn run_bash_check_diagnostic(
    loaded: &LoadedSettings,
    cwd: &Path,
    command: &str,
    cli_model: Option<&str>,
) -> anyhow::Result<DiagnosticReport> {
    let (client, bypass_env) = resolve_run_judge_client(loaded, cli_model)?;
    let Some(client) = client else {
        return Ok(DiagnosticReport {
            report: render_outcome(&JudgeOutcome::Bypassed, Duration::ZERO),
            error: None,
        });
    };
    evaluate_with_client_diagnostic(client, &loaded.judge, bypass_env.as_deref(), cwd, command)
        .await
}

/// Resolve the judge client for one `cake bash check` run, or `None` when the
/// judge is disabled.
///
/// The emergency bypass short-circuits before any judge setup: a disabled
/// judge must not fail on an unusable model or rubric, because the bypass is
/// the recovery path when judge configuration is broken. The `CAKE_JUDGE`
/// value is returned alongside so the shared judge path re-reads the same
/// value without another environment access.
fn resolve_run_judge_client(
    loaded: &LoadedSettings,
    cli_model: Option<&str>,
) -> anyhow::Result<(Option<JudgeClient>, Option<String>)> {
    let bypass_env = std::env::var(JUDGE_BYPASS_ENV).ok();
    if !judge_is_enabled(&loaded.judge, bypass_env.as_deref()) {
        return Ok((None, bypass_env));
    }
    let model = resolve_judge_model(loaded, cli_model)?;
    let client = JudgeClient::new(
        model,
        Duration::from_secs(loaded.judge.timeout_secs),
        Duration::from_secs(loaded.judge.retry_budget_secs),
    )
    .with_user_rubric(read_user_rubric(&loaded.judge).map_err(anyhow::Error::msg)?);
    Ok((Some(client), bypass_env))
}

/// Evaluate one command with an already-configured judge client and render the
/// outcome as human-readable inspection output. Exposed to tests so they can
/// drive the exact `cake bash check` path against a stub judge without touching
/// settings or spawning processes.
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
) -> anyhow::Result<CheckOutcome> {
    let request = JudgeRequest::new(command.to_string(), cwd.to_path_buf(), None)
        .with_repo_digest(repo_state_digest(cwd));

    let started = Instant::now();
    let outcome = evaluate_command(&client, settings, request, bypass_env).await?;
    let latency = started.elapsed();

    Ok(CheckOutcome { outcome, latency })
}

/// Rendered `--diagnostic` output and the fail-closed judge outcome.
///
/// The report always goes to stdout, even when the judge fails; `error` is
/// `Some` only then, so the caller can propagate a redacted [`JudgeError`] for
/// exit classification without duplicating the report on stderr.
struct DiagnosticReport {
    report: String,
    error: Option<JudgeError>,
}

async fn evaluate_with_client_diagnostic(
    client: JudgeClient,
    settings: &JudgeSettings,
    bypass_env: Option<&str>,
    cwd: &Path,
    command: &str,
) -> anyhow::Result<DiagnosticReport> {
    let secrets = diagnostic_redaction_secrets(&client);
    let request = JudgeRequest::new(command.to_string(), cwd.to_path_buf(), None)
        .with_repo_digest(repo_state_digest(cwd));
    let evaluation = evaluate_command_observed(&client, settings, request, bypass_env, true).await;
    let (report, result) = render_diagnostic_evaluation(evaluation, &secrets);
    Ok(DiagnosticReport {
        report,
        error: result.err(),
    })
}

/// The values the `--diagnostic` output must omit: the resolved API key plus
/// any configured provider header values (for example `OpenRouter`'s
/// `HTTP-Referer` and `X-Title`), which an endpoint could echo back in a
/// response body or error. Ordered longest-first so a value containing
/// another secret (for example a header value containing the API key) is
/// redacted before its inner secret is replaced.
fn diagnostic_redaction_secrets(client: &JudgeClient) -> Vec<String> {
    let mut secrets = vec![client.api_key().to_string()];
    secrets.extend(client.provider_header_values());
    secrets.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    secrets.dedup();
    secrets
}

fn render_diagnostic_evaluation(
    evaluation: JudgeEvaluation,
    secrets: &[String],
) -> (String, Result<(), JudgeError>) {
    use std::fmt::Write as _;

    let JudgeEvaluation {
        outcome,
        attempts,
        diagnostic,
    } = evaluation;
    let Some(attempt) = attempts.last() else {
        return match outcome {
            Ok(outcome) => (
                format!(
                    "WARNING: raw judge diagnostics may contain command text, paths, repository state, reason text, and secrets embedded in those values.\n\n{}",
                    redact_all(&render_outcome(&outcome, Duration::ZERO), secrets)
                ),
                Ok(()),
            ),
            Err(error) => {
                let error = redact_judge_error(error, secrets);
                (
                    format!(
                        "WARNING: raw judge diagnostics may contain command text, paths, repository state, reason text, and secrets embedded in those values.\n\nJudge error: {error}\n"
                    ),
                    Err(error),
                )
            },
        };
    };
    let Some(raw) = diagnostic else {
        let report = "WARNING: raw judge diagnostics contain command text, paths, repository state, reason text, and may contain secrets embedded in those values. Handle this output as sensitive.\n\nJudge diagnostic data was unavailable.\n"
            .to_string();
        let result = outcome
            .err()
            .map(|error| redact_judge_error(error, secrets));
        return (report, result.map_or(Ok(()), Err));
    };

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
        serde_json::to_string_pretty(&raw.request_json).unwrap_or_default()
    );
    _ = writeln!(
        out,
        "\nParsed response:\n{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "assistant_content": raw.assistant_content,
            "usage": raw.usage,
            "termination": raw.termination,
        }))
        .unwrap_or_default()
    );
    _ = writeln!(
        out,
        "\nAttempt metadata:\n{}",
        serde_json::to_string_pretty(attempt).unwrap_or_default()
    );
    match outcome {
        Ok(outcome) => {
            let rendered = render_outcome(&outcome, Duration::from_millis(attempt.total_ms));
            _ = write!(out, "\n{}", redact_all(&rendered, secrets));
            (redact_all(&out, secrets), Ok(()))
        },
        Err(error) => {
            let error = redact_judge_error(error, secrets);
            _ = write!(out, "\nJudge error: {error}\n");
            (redact_all(&out, secrets), Err(error))
        },
    }
}

/// Replace every occurrence of a resolved secret (API key, configured provider
/// header value) with `<redacted>`. An empty secret is left untouched so
/// callers with no configured key are unaffected.
fn redact_secret(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        text.to_string()
    } else {
        text.replace(secret, "<redacted>")
    }
}

/// Apply [`redact_secret`] for every configured secret.
fn redact_all(text: &str, secrets: &[String]) -> String {
    let mut redacted = text.to_string();
    for secret in secrets {
        redacted = redact_secret(&redacted, secret);
    }
    redacted
}

/// Redact the configured secrets from a [`JudgeError`]'s human-readable fields
/// while preserving the concrete type, so exit-code classification via
/// `downcast_ref::<JudgeError>()` still works.
fn redact_judge_error(error: JudgeError, secrets: &[String]) -> JudgeError {
    match error {
        JudgeError::Transport { status, detail } => JudgeError::Transport {
            status,
            detail: redact_all(&detail, secrets),
        },
        JudgeError::Malformed(message) => JudgeError::Malformed(redact_all(&message, secrets)),
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
            format!("Verdict: bypassed\nMessage: {JUDGE_BYPASS_MESSAGE}\n")
        },
        JudgeOutcome::Verdict {
            verdict,
            overridden,
        } => render_verdict(verdict, *overridden, latency),
    }
}

/// The label used for a judge decision in both output modes.
const fn decision_label(decision: JudgeDecision) -> &'static str {
    match decision {
        JudgeDecision::Block => "block",
        JudgeDecision::Warn => "warn",
        JudgeDecision::Allow => "allow",
    }
}

/// The verdict's stable code, when the judge supplied a non-empty one.
///
/// An empty code is treated the same as a missing code in both output modes.
fn verdict_code(verdict: &JudgeVerdict) -> Option<&str> {
    verdict.code.as_deref().filter(|code| !code.is_empty())
}

/// Render a verdict as human-readable inspection output on stdout.
fn render_verdict(verdict: &JudgeVerdict, overridden: bool, latency: Duration) -> String {
    use std::fmt::Write as _;

    let mut out = format!("Verdict: {}\n", decision_label(verdict.decision));
    if let Some(code) = verdict_code(verdict) {
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
