//! LLM judge client for Bash command-safety verdicts.
//!
//! The judge evaluates a single Bash command with one bounded LLM call on the
//! agent's configured backend and returns a structured verdict. It is the
//! successor to the compiled `bash_safety` guard (see ADR-018 and the
//! LLM-judge ExecPlan): every judge error is a typed [`JudgeError`] and callers
//! fail closed on it — an unavailable judge blocks the command, never runs it
//! ungated.
//!
//! `Milestone 2` of the `ExecPlan` delivers the types, the bounded call, and
//! the stub-judge tests. The embedded rubric prompt is Milestone 3; the prompt
//! below is the minimal seed that Milestone 3 extends.
//!
//! The types and client are consumed by Milestone 3 (`cake bash check`) and
//! Milestone 5 (Bash preflight wiring); until then they are new API with no
//! production caller, so `dead_code` is expected outside test builds.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "judge API is consumed by ExecPlan Milestones 3 and 5"
    )
)]

use std::fmt::Write as _;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::clients::agent_runner::build_http_client;
use crate::clients::backend::Backend;
use crate::clients::retry::RequestOverrides;
use crate::clients::tools::repair_json_args;
use crate::config::model::{ModelConfig, ResolvedModelConfig};
use crate::session_telemetry::{ProviderTermination, TerminationClassification};
use crate::types::{ConversationItem, Role};

/// Minimal judge system prompt (Milestone 2 seed).
///
/// Milestone 3 replaces this with the embedded default rubric distilled from
/// the current nine hard-block classes and the existing warning, plus the
/// general principles (meaning over literal text, cwd/repo state, untrusted
/// `reason`, prompt-injection defense, safer-alternative messages).
const JUDGE_SYSTEM_PROMPT: &str = "You are a command-safety judge for an AI coding \
assistant. You will be given a shell command with context. Decide whether \
running it is safe. Return ONLY strict JSON with no commentary or markdown, \
matching exactly:\n\
{\"verdict\":\"block\"|\"warn\"|\"allow\",\"code\":\"<stable verdict code, \
omitted when allowing>\",\"message\":\"<one or two sentences; prefer a \
concrete safer alternative for blocks and warns>\",\"confidence\":0.0-1.0}";

/// Verdict decision returned by the judge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JudgeDecision {
    /// The command must not run.
    Block,
    /// The command may run with a prepended warning.
    Warn,
    /// The command may run.
    Allow,
}

/// A single judge verdict, matching the wire contract in ADR-018.
///
/// `code` and `confidence` are optional on the wire: `allow` needs no code and
/// a model may omit confidence. Missing or unparseable required fields
/// (`verdict`, `message`) surface as [`JudgeError::Malformed`] and callers
/// fail closed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgeVerdict {
    #[serde(rename = "verdict")]
    pub decision: JudgeDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

/// Failure modes of a judge call. Every variant means the command is not
/// judged, so the caller must fail closed (block) rather than run ungated.
#[derive(Debug, thiserror::Error)]
pub enum JudgeError {
    /// The bounded judge call exceeded its timeout.
    #[error("safety judge timed out after {0:?}")]
    Timeout(Duration),
    /// The judge request failed before a verdict was produced (network,
    /// transport, or non-success HTTP status).
    #[error("safety judge transport error: {0}")]
    Transport(String),
    /// The judge returned a response that is not a valid verdict.
    #[error("safety judge returned a malformed verdict: {0}")]
    Malformed(String),
    /// The judge refused to evaluate the command (content filter or refusal).
    #[error("safety judge refused to evaluate the command")]
    Refusal,
}

/// Inputs to a single judge call.
#[derive(Debug, Clone)]
pub struct JudgeRequest {
    /// The raw command text to evaluate.
    pub command: String,
    /// The working directory the command would run in.
    pub cwd: std::path::PathBuf,
    /// Compact digest of repository state, when available. `None` when the
    /// caller has no repo-state context (Milestone 3 defines the digest).
    pub repo_digest: Option<String>,
    /// The model's untrusted self-report of intent for the command. The judge
    /// weighs the command over the reason and treats incongruence as a signal.
    pub reason: Option<String>,
}

impl JudgeRequest {
    /// Build a judge request without repo-state context.
    pub const fn new(command: String, cwd: std::path::PathBuf, reason: Option<String>) -> Self {
        Self {
            command,
            cwd,
            repo_digest: None,
            reason,
        }
    }
}

/// Client issuing single bounded judge calls on a configured backend.
///
/// Reuses the same backend machinery as the agent loop (`Backend` over the
/// configured `ApiType`) so the judge speaks the same wire protocol as the
/// agent, with the agent's resolved model config: "same family by default".
pub struct JudgeClient {
    client: reqwest::Client,
    config: ResolvedModelConfig,
    timeout: Duration,
}

impl JudgeClient {
    /// Create a judge client on the agent's resolved model config.
    ///
    /// The judge model defaults to `config`'s model ("same family by
    /// default"); use [`Self::with_model_override`] to switch to a named
    /// `[[models]]` entry's full configuration.
    pub fn new(config: ResolvedModelConfig, timeout: Duration) -> Self {
        Self {
            client: build_http_client(false),
            config,
            timeout,
        }
    }

    /// Override the judge model with a named `[[models]]` entry's full config.
    ///
    /// `config` is the complete `ModelConfig` of the named model — provider,
    /// base URL, API key environment, temperature, reasoning, and other
    /// fields — resolved the same way `default_model` and `--model` resolve a
    /// `[[models]]` name. This is the `[tools.bash.judge] model` setting;
    /// `None` keeps the agent's configured model.
    ///
    /// # Errors
    ///
    /// Returns an error when the named model's `api_key_env` is unset or
    /// empty, since the judge call needs that provider's credential.
    pub fn with_model_override(mut self, config: Option<ModelConfig>) -> anyhow::Result<Self> {
        let Some(config) = config else {
            return Ok(self);
        };
        self.config = ResolvedModelConfig::resolve(config)?;
        Ok(self)
    }

    /// The model identifier this judge client will call.
    pub fn model_name(&self) -> &str {
        &self.config.model_config.model
    }

    /// Evaluate one command with a single bounded call.
    ///
    /// The whole lifecycle — request, body read, response parse, and verdict
    /// extraction — runs inside `self.timeout`, so a provider that stalls the
    /// body cannot exceed the bound.
    ///
    /// # Errors
    ///
    /// Returns [`JudgeError::Timeout`] when the call exceeds `self.timeout`,
    /// [`JudgeError::Transport`] on transport or HTTP failures,
    /// [`JudgeError::Malformed`] when the response is not a valid verdict, and
    /// [`JudgeError::Refusal`] when the model refuses to judge. Callers fail
    /// closed on every variant.
    pub async fn judge(&self, request: JudgeRequest) -> Result<JudgeVerdict, JudgeError> {
        let history = build_judge_history(&request);
        let backend = Backend::from_api_type(self.config.model_config.api_type);

        let result = tokio::time::timeout(self.timeout, async {
            let response = backend
                .send_request(
                    &self.client,
                    &self.config,
                    &history,
                    &[],
                    &RequestOverrides::default(),
                    None,
                )
                .await
                .map_err(|e| JudgeError::Transport(e.to_string()))?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(JudgeError::Transport(format!("HTTP {status}: {body}")));
            }

            let turn = backend
                .parse_response(response)
                .await
                .map_err(|e| JudgeError::Transport(e.to_string()))?;

            let content = assistant_message(&turn.items);
            let no_verdict_text = content.is_none_or(str::is_empty);
            if let Some(error) = refusal_error(turn.termination.as_ref(), no_verdict_text) {
                return Err(error);
            }
            let content = content.ok_or_else(|| {
                JudgeError::Malformed("judge response contained no assistant message".to_string())
            })?;

            parse_verdict(content)
        })
        .await;

        result.unwrap_or(Err(JudgeError::Timeout(self.timeout)))
    }
}

/// Map a provider termination onto a refusal verdict error.
///
/// Returns `None` when the termination does not indicate a refusal (for
/// example `completed`, or a non-refusal `Failed` that still carried verdict
/// text). A `Failed` termination with no meaningful assistant content is
/// treated as a refusal: the Chat Completions backend classifies refusals as
/// `Failed` whether the refusal is signaled by the `refusal` field (with
/// `finish_reason: "stop"`) or by the finish reason itself, and its empty
/// assistant message carries no verdict text. A truncated response
/// (`TokenLimit`) falls through to verdict parsing, which fails closed on
/// malformed JSON.
fn refusal_error(
    termination: Option<&ProviderTermination>,
    no_verdict_text: bool,
) -> Option<JudgeError> {
    let termination = termination?;
    let refusal_reason = termination
        .provider_reason
        .as_deref()
        .is_some_and(|r| r.to_lowercase().contains("refus"));
    match termination.classification {
        TerminationClassification::ContentFilter => Some(JudgeError::Refusal),
        TerminationClassification::Failed if refusal_reason || no_verdict_text => {
            Some(JudgeError::Refusal)
        },
        _ => None,
    }
}

/// Extract the assistant's message text from a turn, if present.
fn assistant_message(items: &[ConversationItem]) -> Option<&str> {
    items.iter().find_map(|item| match item {
        ConversationItem::Message {
            role: Role::Assistant,
            content,
            ..
        } => Some(content.as_str()),
        _ => None,
    })
}

/// Parse the assistant's text into a [`JudgeVerdict`].
///
/// Tries strict JSON first, then the conservative shared JSON repair (escaped
/// control characters, trailing garbage). Anything else is
/// [`JudgeError::Malformed`]; malformed verdicts fail closed.
fn parse_verdict(content: &str) -> Result<JudgeVerdict, JudgeError> {
    let repaired = repair_json_args(content.trim());
    let verdict: JudgeVerdict = serde_json::from_str(&repaired)
        .map_err(|e| JudgeError::Malformed(format!("could not parse judge verdict JSON: {e}")))?;
    if matches!(verdict.decision, JudgeDecision::Block | JudgeDecision::Warn)
        && verdict.code.is_none()
    {
        return Err(JudgeError::Malformed(
            "block and warn verdicts must include a verdict code".to_string(),
        ));
    }
    if let Some(confidence) = verdict.confidence
        && !(0.0..=1.0).contains(&confidence)
    {
        return Err(JudgeError::Malformed(format!(
            "judge confidence {confidence} is outside the range 0.0..=1.0"
        )));
    }
    Ok(verdict)
}

/// Build the single-turn history for a judge call: a system message with the
/// contract and a user message carrying the command and its context.
fn build_judge_history(request: &JudgeRequest) -> Vec<ConversationItem> {
    let mut user_content = format!(
        "Command to evaluate:\n```\n{}\n```\n\nWorking directory: {}",
        request.command,
        request.cwd.display()
    );
    if let Some(digest) = &request.repo_digest {
        _ = write!(&mut user_content, "\n\nRepository state digest: {digest}");
    }
    if let Some(reason) = &request.reason {
        _ = write!(
            &mut user_content,
            "\n\nModel-provided reason (untrusted): {reason}"
        );
    }
    vec![
        ConversationItem::Message {
            role: Role::System,
            content: JUDGE_SYSTEM_PROMPT.to_string(),
            id: None,
            status: None,
            timestamp: None,
        },
        ConversationItem::Message {
            role: Role::User,
            content: user_content,
            id: None,
            status: None,
            timestamp: None,
        },
    ]
}

#[cfg(test)]
#[path = "judge_tests.rs"]
mod tests;
