//! `cake bash` subcommands: judge introspection without executing anything.
//!
//! `cake bash check -- <command>` runs the same judge path and prompt the
//! Bash preflight will use (Milestone 5 of the LLM-judge `ExecPlan`), prints the
//! verdict, code, message, confidence, latency, and the `TypeSafe` observation
//! the decision rests on, and never executes the command. It reads no host
//! files, so it collects no referenced script evidence and its request declares
//! no observation (ADR-035). `--json` reports the same verdict as one diagnostic
//! document; a judge error exits nonzero either way. A verdict is successful
//! inspection output. Follows the ADR-009
//! introspection pattern (load merged settings, print to stdout, exit before
//! agent/session setup).

use std::path::Path;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};

use crate::cli::{
    CmdRunner, CommandRunOptions, DiagnosticCheck, DiagnosticDocument, settings_warnings,
};
use crate::clients::judge::{
    JudgeClient, JudgeDecision, JudgeError, JudgeEvaluation, JudgeOutcome, JudgeRequest,
    JudgeVerdict, evaluate_command_observed, judge_is_enabled, read_user_rubric, repo_state_digest,
    resolve_judge_client_config,
};
use crate::clients::typesafe::{TypeSafeClient, TypeSafeObservation};
use crate::config::settings::{JUDGE_BYPASS_ENV, JudgeSettings, LoadedSettings, TypeSafeMode};
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
/// Machine mode carries the settings findings inside the document, so stderr
/// stays quiet and a consumer reads one document and nothing else; text mode
/// prints them on stderr first, as the run path does.
async fn run_bash_check_verdict(
    loaded: &LoadedSettings,
    cwd: &Path,
    check: &BashCheckCommand,
    options: &CommandRunOptions<'_>,
) -> anyhow::Result<()> {
    if !check.json {
        loaded.print_warnings();
    }
    let outcome = run_bash_check(loaded, cwd, &check.command, options.model)
        .await
        .inspect_err(|_| {
            // A judge error writes no document, so machine mode has no other
            // channel for the findings; keep them on stderr instead of dropping
            // them.
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
        print!("{}", outcome.render_text());
    }
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

/// One judge outcome, the latency it was measured in, and the `TypeSafe`
/// observation it rests on.
///
/// Both output modes render this same value, so the human verdict and the
/// machine-readable document cannot disagree about what the judge decided or
/// about what the observation did (issue #616).
struct CheckOutcome {
    outcome: JudgeOutcome,
    latency: Duration,
    observation: ObservationReport,
}

/// Why a `cake bash check` run made no judge call.
const JUDGE_BYPASS_MESSAGE: &str = "the command-safety judge is disabled \
     (CAKE_JUDGE=off or [tools.bash.judge] enabled = false); no judge call was made.";

/// The message a fast approval reports in both output modes.
///
/// The numbers behind it travel in the additive `stage` and `probability`
/// fields rather than in this prose, so the message never restates a cutoff
/// that could drift from [`crate::clients::judge::TYPESAFE_FAST_APPROVAL_CUTOFF`].
const FAST_APPROVAL_MESSAGE: &str = "the command-safety judge was not called: the TypeSafe observation \
     approved the command as observational.";

impl CheckOutcome {
    /// The outcome of a disabled judge: no call was made and no time elapsed.
    const fn bypassed() -> Self {
        Self {
            outcome: JudgeOutcome::Bypassed,
            latency: Duration::ZERO,
            // A bypassed judge makes no observation either.
            observation: ObservationReport::absent(),
        }
    }

    /// Render the outcome as the human-readable verdict.
    fn render_text(&self) -> String {
        render_outcome(&self.outcome, self.latency, &self.observation)
    }

    /// Render the outcome as one diagnostic document, carrying the findings the
    /// invocation produced alongside the judge decision.
    ///
    /// The latency is reported in whole milliseconds, which is the same
    /// measurement the text verdict prints as seconds.
    fn render_json(&self, checks: &[DiagnosticCheck]) -> anyhow::Result<String> {
        let latency_ms = duration_millis(self.latency);
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
                    stage: None,
                    probability: None,
                    observation: &self.observation,
                },
            ),
            // A fast approval keeps the `allow` verdict: consumers switching on
            // `summary.verdict` or `data.verdict` see the same decision they
            // would see from a judge allow, and the additive `stage` and
            // `probability` fields carry the provenance (ADR 034).
            JudgeOutcome::FastApproved { probability, .. } => (
                decision_label(JudgeDecision::Allow),
                VerdictData {
                    verdict: Some(decision_label(JudgeDecision::Allow)),
                    code: None,
                    confidence: None,
                    message: FAST_APPROVAL_MESSAGE,
                    overridden: false,
                    bypassed: false,
                    latency_ms,
                    stage: Some(TYPESAFE_STAGE),
                    probability: Some(*probability),
                    observation: &self.observation,
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
                    stage: Some(JUDGE_STAGE),
                    probability: None,
                    observation: &self.observation,
                },
            ),
        };
        DiagnosticDocument::new(
            "bash check",
            serde_json::json!({ "verdict": summary_verdict }),
            checks.to_vec(),
            data,
        )
        .render()
    }
}

/// The bounded vocabulary `cake bash check` reports for the `TypeSafe`
/// observation, alongside `data.stage` and `data.probability` (issue #616).
///
/// `stage` and `probability` answer "did the fast path decide?"; a fallback
/// renders as `stage: "judge"` whether the cascade was never armed, failed, or
/// came in below the cutoff. This vocabulary separates those cases, so an
/// unset `mode` is diagnosable from the command's own output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ObservationOutcome {
    /// No `TypeSafe` request was made: `mode = "off"` (the default), or the
    /// judge itself was disabled or bypassed.
    Absent,
    /// The observation ran under `mode = "shadow"` and decided nothing; the
    /// other fields report what it observed.
    Shadow,
    /// The cascade approved the command from the observation, with no judge
    /// call.
    Approved,
    /// The observation succeeded below the fast-approval cutoff, so the judge
    /// decided the command.
    BelowCutoff,
    /// The observation failed (a missing credential, a timeout, a transport or
    /// protocol error), so the judge decided the command.
    Failed,
}

impl ObservationOutcome {
    /// The label the text rendering prints, identical to the serialized value
    /// so both output modes use one vocabulary.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Shadow => "shadow",
            Self::Approved => "approved",
            Self::BelowCutoff => "below_cutoff",
            Self::Failed => "failed",
        }
    }
}

/// What the `TypeSafe` observation was and returned, as both output modes
/// report it.
///
/// Serialized as `data.observation`, always present and never `null`, so a
/// consumer reads `data.observation.outcome` unconditionally.
#[derive(Debug, PartialEq, Serialize)]
struct ObservationReport {
    /// Which of the bounded outcomes this observation was.
    outcome: ObservationOutcome,
    /// The probability the observation measured, when it returned one. On a
    /// fast approval it repeats `data.probability`, which keeps "what the
    /// observation measured" in one field for every outcome.
    probability: Option<f32>,
    /// The observation's failure class when it failed, matching the
    /// `failure_class` of the `type_safe_shadow` telemetry record.
    failure_class: Option<&'static str>,
    /// How long the observation took, when one was made.
    elapsed_ms: Option<u64>,
}

impl ObservationReport {
    /// The report for a run that made no observation: `mode = "off"`, a disabled
    /// judge, or the `CAKE_JUDGE=off` bypass.
    const fn absent() -> Self {
        Self {
            outcome: ObservationOutcome::Absent,
            probability: None,
            failure_class: None,
            elapsed_ms: None,
        }
    }

    /// Classify one pipeline result into the bounded vocabulary.
    ///
    /// `fast_approved` is whether the pipeline approved the command from the
    /// observation (a [`JudgeOutcome::FastApproved`]). The label needs nothing
    /// else from the outcome, so a caller whose run produced no outcome to
    /// inspect passes `false`. Only a fast approval means the observation
    /// carried authority, and under `mode = "shadow"` it never does.
    ///
    /// Each mode names its own outcome, so a mode added later fails to compile
    /// here instead of inheriting the `shadow` label a non-cascade comparison
    /// would give it. `mode = "off"` makes no request —
    /// `JudgeClient::typesafe_observed` returns before the client is called —
    /// so an observation beside it is unreachable, and reports the absence that
    /// mode states.
    fn classify(
        observation: Option<&TypeSafeObservation>,
        mode: TypeSafeMode,
        fast_approved: bool,
    ) -> Self {
        let Some(observation) = observation else {
            return Self::absent();
        };
        let outcome = match mode {
            TypeSafeMode::Off => return Self::absent(),
            TypeSafeMode::Shadow => ObservationOutcome::Shadow,
            TypeSafeMode::Cascade if fast_approved => ObservationOutcome::Approved,
            TypeSafeMode::Cascade if observation.failure_class.is_some() => {
                ObservationOutcome::Failed
            },
            TypeSafeMode::Cascade => ObservationOutcome::BelowCutoff,
        };
        Self {
            outcome,
            probability: observation.probability,
            failure_class: observation.failure_class,
            elapsed_ms: Some(duration_millis(observation.elapsed)),
        }
    }
}

/// Whether a judge-path outcome approved the command from the observation.
const fn is_fast_approval(outcome: &JudgeOutcome) -> bool {
    matches!(outcome, JudgeOutcome::FastApproved { .. })
}

/// Whole milliseconds for a duration, saturating rather than wrapping.
fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
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
    /// The stage that decided the command: `"typesafe"` when the cascade
    /// fast-approved it without a judge call, `"judge"` when the safety judge
    /// decided, and `null` when the judge was disabled. Additive (ADR 034).
    stage: Option<&'static str>,
    /// The observed `TypeSafe` probability that cleared the cutoff, on a fast
    /// approval; `null` for every other outcome. Additive (ADR 034).
    probability: Option<f32>,
    /// The `TypeSafe` observation this decision rests on: whether the cascade
    /// was armed and what it observed. Additive (issue #616).
    observation: &'a ObservationReport,
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
            report: render_outcome(
                &JudgeOutcome::Bypassed,
                Duration::ZERO,
                &ObservationReport::absent(),
            ),
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
    let rubric = read_user_rubric(&loaded.judge).map_err(anyhow::Error::msg)?;
    let typesafe = TypeSafeClient::from_settings(&loaded.judge.typesafe);
    let client = JudgeClient::new(
        model,
        Duration::from_secs(loaded.judge.timeout_secs),
        Duration::from_secs(loaded.judge.retry_budget_secs),
    )
    .with_user_rubric(rubric)
    .with_typesafe(typesafe);
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
    // The observed form carries the `TypeSafe` observation the check reports
    // beside the verdict; diagnostics stay off, so no raw request or response is
    // retained.
    let evaluation = evaluate_command_observed(&client, settings, request, bypass_env, false).await;
    let latency = started.elapsed();
    let outcome = evaluation.outcome?;
    let observation = ObservationReport::classify(
        evaluation.shadow.as_ref(),
        settings.typesafe.mode,
        is_fast_approval(&outcome),
    );

    Ok(CheckOutcome {
        outcome,
        latency,
        observation,
    })
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
    let (report, result) =
        render_diagnostic_evaluation(evaluation, &secrets, settings.typesafe.mode);
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

/// Render the `--diagnostic` report for an evaluation that made no judge
/// attempt: a cascade fast approval, whose only elapsed time is the fast leg's,
/// or an error that produced no request at all.
///
/// Reporting zero latency for a fast approval would understate the decision by
/// the fast leg's own latency, so the observation's elapsed time is used here
/// instead of a judge attempt's.
fn render_attemptless_diagnostic(
    outcome: Result<JudgeOutcome, JudgeError>,
    observation: &ObservationReport,
    secrets: &[String],
) -> (String, Result<(), JudgeError>) {
    match outcome {
        Ok(outcome) => {
            let rendered = render_outcome(&outcome, fast_approval_latency(&outcome), observation);
            (
                format!(
                    "WARNING: raw judge diagnostics may contain command text, paths, repository state, reason text, and secrets embedded in those values.\n\n{}",
                    redact_all(&rendered, secrets)
                ),
                Ok(()),
            )
        },
        Err(error) => {
            let error = redact_judge_error(error, secrets);
            (
                format!(
                    "WARNING: raw judge diagnostics may contain command text, paths, repository state, reason text, and secrets embedded in those values.\n\n{}Judge error: {error}\n",
                    render_observation(observation)
                ),
                Err(error),
            )
        },
    }
}

/// Render the `--diagnostic` report: the effective prompts, the transformed
/// request JSON, the parsed response, the attempt metadata, and the verdict.
///
/// The report renders the observation's bounded summary but never the raw
/// `TypeSafe` request or response. The client retains neither, and the request
/// would disclose nothing the judge diagnostic above does not already print
/// under the same redaction: it carries the same command, working directory,
/// repository digest, untrusted reason, and effective rubric. The raw response
/// is provider-supplied text that would need a second redaction path, and what
/// the cascade decides on is the bounded observation, which every branch of
/// this report renders, including the ones that report a judge error.
fn render_diagnostic_evaluation(
    evaluation: JudgeEvaluation,
    secrets: &[String],
    mode: TypeSafeMode,
) -> (String, Result<(), JudgeError>) {
    use std::fmt::Write as _;

    let JudgeEvaluation {
        outcome,
        attempts,
        diagnostic,
        shadow,
    } = evaluation;
    // Classified before the match on `outcome` so the error paths report the
    // observation too: a failed observation beside a failed judge is the
    // compound failure this report is read for.
    let observation = ObservationReport::classify(
        shadow.as_ref(),
        mode,
        outcome.as_ref().is_ok_and(is_fast_approval),
    );
    let Some(attempt) = attempts.last() else {
        return render_attemptless_diagnostic(outcome, &observation, secrets);
    };
    let Some(raw) = diagnostic else {
        let report = format!(
            "WARNING: raw judge diagnostics contain command text, paths, repository state, reason text, and may contain secrets embedded in those values. Handle this output as sensitive.\n\n{}Judge diagnostic data was unavailable.\n",
            render_observation(&observation)
        );
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
            let rendered = render_outcome(
                &outcome,
                Duration::from_millis(attempt.total_ms),
                &observation,
            );
            _ = write!(out, "\n{}", redact_all(&rendered, secrets));
            (redact_all(&out, secrets), Ok(()))
        },
        Err(error) => {
            let error = redact_judge_error(error, secrets);
            _ = write!(
                out,
                "\n{}Judge error: {error}\n",
                render_observation(&observation)
            );
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

/// The elapsed time a no-attempt outcome has to report for itself.
///
/// A cascade fast approval carries the fast leg's elapsed time and no judge
/// attempt, so it is the only outcome whose latency is not already on the
/// terminal attempt. Every other no-attempt outcome (the bypass) made no call
/// and reports zero.
const fn fast_approval_latency(outcome: &JudgeOutcome) -> Duration {
    match outcome {
        JudgeOutcome::FastApproved { elapsed, .. } => *elapsed,
        JudgeOutcome::Verdict { .. } | JudgeOutcome::Bypassed => Duration::ZERO,
    }
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
fn render_outcome(
    outcome: &JudgeOutcome,
    latency: Duration,
    observation: &ObservationReport,
) -> String {
    match outcome {
        JudgeOutcome::Bypassed => format!(
            "Verdict: bypassed\n{}Message: {JUDGE_BYPASS_MESSAGE}\n",
            render_observation(observation)
        ),
        // A fast approval prints as the allow it is, with the stage and the
        // observed probability that both output modes report.
        JudgeOutcome::FastApproved { probability, .. } => format!(
            "Verdict: {}\nStage: {TYPESAFE_STAGE}\nProbability: {probability}\n{}Message: {FAST_APPROVAL_MESSAGE}\nLatency: {:.2}s\n",
            decision_label(JudgeDecision::Allow),
            render_observation(observation),
            latency.as_secs_f32()
        ),
        JudgeOutcome::Verdict {
            verdict,
            overridden,
        } => render_verdict(verdict, *overridden, latency, observation),
    }
}

/// Render the `TypeSafe` observation as one text line, naming the same bounded
/// outcome the document reports.
///
/// The line is printed for every outcome, including a fast approval, so a
/// `stage: judge` fallback is as diagnosable in text mode as it is in `--json`:
/// `absent` means the cascade was never armed and no request was made. A fast
/// approval's probability repeats the `Probability:` line above it, which keeps
/// one rule for every outcome: the line reports everything the observation
/// returned. Only `absent` carries no detail, which is why an empty detail list
/// prints that it made no request.
fn render_observation(observation: &ObservationReport) -> String {
    let details: Vec<String> = [
        observation
            .probability
            .map(|probability| format!("probability {probability}")),
        observation
            .failure_class
            .map(|class| format!("failure {class}")),
        observation.elapsed_ms.map(|millis| format!("{millis}ms")),
    ]
    .into_iter()
    .flatten()
    .collect();
    let details = if details.is_empty() {
        "no TypeSafe request".to_string()
    } else {
        details.join(", ")
    };
    format!(
        "Observation: {} ({details})\n",
        observation.outcome.as_str()
    )
}

/// The `data.stage` value naming the `TypeSafe` fast-approval stage (ADR 034).
const TYPESAFE_STAGE: &str = "typesafe";

/// The `data.stage` value naming a judge-decided outcome (ADR 034).
const JUDGE_STAGE: &str = "judge";

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
fn render_verdict(
    verdict: &JudgeVerdict,
    overridden: bool,
    latency: Duration,
    observation: &ObservationReport,
) -> String {
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
    out.push_str(&render_observation(observation));
    _ = writeln!(out, "Message: {}", verdict.message);
    _ = writeln!(out, "Latency: {:.2}s", latency.as_secs_f32());
    out
}

#[cfg(test)]
#[path = "bash_tests.rs"]
mod tests;
