//! Per-attempt observability for the command-safety judge.

use std::time::{Duration, Instant};

use crate::clients::backend::Backend;
use crate::clients::retry::RequestOverrides;
use crate::session_telemetry::{
    JudgeAttemptTelemetry, JudgeAttemptTerminalClass, ProviderTermination,
};
use crate::types::{ConversationItem, Role};

use crate::clients::judge::{
    JudgeClient, JudgeDiagnostic, JudgeError, JudgeRequest, JudgeVerdict, assistant_message,
    build_judge_history, parse_verdict, refusal_error,
};

pub(super) struct JudgeCall {
    pub(super) result: Result<JudgeVerdict, JudgeError>,
    pub(super) attempt: JudgeAttemptTelemetry,
    pub(super) diagnostic: Option<JudgeDiagnostic>,
}

pub(super) async fn judge_observed(
    client: &JudgeClient,
    request: &JudgeRequest,
    include_raw_diagnostic: bool,
) -> JudgeCall {
    let observed = match ObservedJudgeCall::start(client, request, include_raw_diagnostic) {
        Ok(observed) => observed,
        Err(call) => return *call,
    };
    let (response, observed) = match observed.send(client).await {
        Ok(sent) => sent,
        Err(call) => return *call,
    };
    if response.status().is_success() {
        observed.finish_success(client, response).await
    } else {
        observed.finish_http_error(client, response).await
    }
}

struct ObservedJudgeCall {
    backend: Backend,
    total_start: Instant,
    request_json: Vec<u8>,
    attempt: JudgeAttemptTelemetry,
    diagnostic: Option<JudgeDiagnostic>,
    /// The API key plus the judge inputs (command, reason, cwd) that must
    /// never appear in telemetry, in case a provider echoes them back in a
    /// request id or termination status/reason.
    redaction_secrets: Vec<String>,
}

impl ObservedJudgeCall {
    fn start(
        client: &JudgeClient,
        request: &JudgeRequest,
        include_raw_diagnostic: bool,
    ) -> Result<Self, Box<JudgeCall>> {
        let total_start = Instant::now();
        let build_start = Instant::now();
        let history = build_judge_history(request, client.user_rubric.as_deref());
        let backend = Backend::from_api_type(client.config.model_config.api_type);
        let mut attempt = initial_attempt(client, &history);
        let request_json = backend.build_request_json(
            &client.config,
            &history,
            &[],
            &RequestOverrides::default(),
            None,
        );
        attempt.request_build_ms = elapsed_ms(build_start);
        let request_json = request_json.map_err(|error| {
            attempt.total_ms = elapsed_ms(total_start);
            Box::new(JudgeCall::transport(attempt.clone(), None, None, error))
        })?;
        let diagnostic =
            include_raw_diagnostic.then(|| initial_diagnostic(client, &history, &request_json));
        Ok(Self {
            backend,
            total_start,
            request_json,
            attempt,
            diagnostic,
            redaction_secrets: redaction_secrets(client, request),
        })
    }

    async fn send(
        mut self,
        client: &JudgeClient,
    ) -> Result<(reqwest::Response, Self), Box<JudgeCall>> {
        let request_start = Instant::now();
        let result = tokio::time::timeout(
            self.remaining(client.timeout),
            self.backend
                .send_request_json(&client.client, &client.config, &self.request_json),
        )
        .await;
        self.attempt.request_ms = elapsed_ms(request_start);
        match result {
            Err(_) => Err(Box::new(self.timeout(client.timeout))),
            Ok(Err(error)) => Err(Box::new(self.transport(None, error))),
            Ok(Ok(response)) => {
                self.attempt.status_code = Some(response.status().as_u16());
                self.attempt.provider_request_id = provider_request_id(response.headers());
                Ok((response, self))
            },
        }
    }

    async fn finish_http_error(
        mut self,
        client: &JudgeClient,
        response: reqwest::Response,
    ) -> JudgeCall {
        let status = response.status();
        let parse_start = Instant::now();
        let body = tokio::time::timeout(self.remaining(client.timeout), response.text()).await;
        self.attempt.response_parse_ms = elapsed_ms(parse_start);
        match body {
            Err(_) => self.timeout(client.timeout),
            Ok(body) => {
                self.attempt.terminal_class = JudgeAttemptTerminalClass::HttpError;
                self.finish(Err(JudgeError::Transport {
                    status: Some(status.as_u16()),
                    detail: format!("HTTP {status}: {}", body.unwrap_or_default()),
                }))
            },
        }
    }

    async fn finish_success(
        mut self,
        client: &JudgeClient,
        response: reqwest::Response,
    ) -> JudgeCall {
        let parse_start = Instant::now();
        let turn = tokio::time::timeout(
            self.remaining(client.timeout),
            self.backend.parse_response(response),
        )
        .await;
        self.attempt.response_parse_ms = elapsed_ms(parse_start);
        match turn {
            Err(_) => self.timeout(client.timeout),
            Ok(Err(error)) => {
                self.attempt.terminal_class = JudgeAttemptTerminalClass::ResponseParse;
                self.finish(Err(JudgeError::Transport {
                    status: None,
                    detail: error.to_string(),
                }))
            },
            Ok(Ok(turn)) => self.finish_turn(client, &turn),
        }
    }

    fn finish_turn(
        mut self,
        client: &JudgeClient,
        turn: &crate::clients::agent::TurnResult,
    ) -> JudgeCall {
        self.attempt.usage = turn.usage;
        self.attempt.termination.clone_from(&turn.termination);
        if self.attempt.provider_request_id.is_none() {
            self.attempt
                .provider_request_id
                .clone_from(&turn.provider_request_id);
        }
        let content = assistant_message(&turn.items).map(str::to_string);
        update_diagnostic(&mut self.diagnostic, client, turn, content.as_deref());
        let no_verdict_text = content.as_deref().is_none_or(str::is_empty);
        if let Some(error) = refusal_error(turn.termination.as_ref(), no_verdict_text) {
            self.attempt.terminal_class = JudgeAttemptTerminalClass::Refusal;
            return self.finish(Err(error));
        }

        let verdict_start = Instant::now();
        let result = content.as_deref().map_or_else(
            || {
                Err(JudgeError::Malformed(
                    "judge response contained no assistant message".to_string(),
                ))
            },
            parse_verdict,
        );
        self.attempt.verdict_parse_ms = elapsed_ms(verdict_start);
        self.attempt.terminal_class = if result.is_ok() {
            JudgeAttemptTerminalClass::Verdict
        } else {
            JudgeAttemptTerminalClass::MalformedVerdict
        };
        self.finish(result)
    }

    fn remaining(&self, timeout: Duration) -> Duration {
        timeout
            .checked_sub(self.total_start.elapsed())
            .unwrap_or(Duration::ZERO)
    }

    fn timeout(mut self, timeout: Duration) -> JudgeCall {
        self.attempt.terminal_class = JudgeAttemptTerminalClass::Timeout;
        self.finish(Err(JudgeError::Timeout(timeout)))
    }

    fn transport(mut self, status: Option<u16>, error: impl std::fmt::Display) -> JudgeCall {
        self.attempt.terminal_class = JudgeAttemptTerminalClass::Transport;
        self.finish(Err(JudgeError::Transport {
            status,
            detail: error.to_string(),
        }))
    }

    fn finish(mut self, result: Result<JudgeVerdict, JudgeError>) -> JudgeCall {
        self.attempt.total_ms = elapsed_ms(self.total_start);
        redact_attempt_provider_metadata(&mut self.attempt, &self.redaction_secrets);
        JudgeCall {
            result,
            attempt: self.attempt,
            diagnostic: self.diagnostic,
        }
    }
}

impl JudgeCall {
    fn transport(
        attempt: JudgeAttemptTelemetry,
        diagnostic: Option<JudgeDiagnostic>,
        status: Option<u16>,
        error: impl std::fmt::Display,
    ) -> Self {
        Self {
            result: Err(JudgeError::Transport {
                status,
                detail: error.to_string(),
            }),
            attempt,
            diagnostic,
        }
    }
}

fn initial_attempt(client: &JudgeClient, history: &[ConversationItem]) -> JudgeAttemptTelemetry {
    let (system_prompt, user_prompt) = prompt_text_refs(history);
    JudgeAttemptTelemetry {
        attempt: 1,
        retry_ordinal: 0,
        request_build_ms: 0,
        request_ms: 0,
        response_parse_ms: 0,
        verdict_parse_ms: 0,
        total_ms: 0,
        history_items: history.len(),
        system_prompt_bytes: system_prompt.len(),
        user_prompt_bytes: user_prompt.len(),
        model: client.config.model_config.model.clone(),
        api_type: client.config.model_config.api_type,
        reasoning_effort: client.config.model_config.reasoning_effort,
        temperature: client.config.model_config.temperature,
        top_p: client.config.model_config.top_p,
        max_output_tokens: client.config.model_config.max_output_tokens,
        reasoning_max_tokens: client.config.model_config.reasoning_max_tokens,
        configured_timeout_ms: duration_ms(client.timeout),
        tool_count: 0,
        tool_choice: None,
        status_code: None,
        provider_request_id: None,
        terminal_class: JudgeAttemptTerminalClass::Transport,
        usage: None,
        termination: None,
    }
}

fn initial_diagnostic(
    client: &JudgeClient,
    history: &[ConversationItem],
    request_json: &[u8],
) -> JudgeDiagnostic {
    let (system_prompt, user_prompt) = prompt_text_refs(history);
    // The request body already serialized successfully in `build_request_json`,
    // so parsing it back is best-effort; fall back to null rather than losing
    // the whole diagnostic.
    let request_json = serde_json::from_slice(request_json).unwrap_or(serde_json::Value::Null);
    JudgeDiagnostic {
        system_prompt: redact_secret(system_prompt, &client.config.api_key),
        user_prompt: redact_secret(user_prompt, &client.config.api_key),
        request_json: redact_json(request_json, &client.config.api_key),
        assistant_content: None,
        usage: None,
        termination: None,
    }
}

fn update_diagnostic(
    diagnostic: &mut Option<JudgeDiagnostic>,
    client: &JudgeClient,
    turn: &crate::clients::agent::TurnResult,
    content: Option<&str>,
) {
    let Some(raw) = diagnostic else {
        return;
    };
    raw.assistant_content = content.map(|content| redact_secret(content, &client.config.api_key));
    raw.usage = turn.usage;
    raw.termination = turn
        .termination
        .clone()
        .map(|termination| redact_termination(termination, &client.config.api_key));
}

fn prompt_text_refs(history: &[ConversationItem]) -> (&str, &str) {
    let mut system = "";
    let mut user = "";
    for item in history {
        if let ConversationItem::Message { role, content, .. } = item {
            match role {
                Role::System => system = content,
                Role::User => user = content,
                _ => {},
            }
        }
    }
    (system, user)
}

fn provider_request_id(headers: &reqwest::header::HeaderMap) -> Option<String> {
    ["x-request-id", "request-id", "openai-request-id"]
        .into_iter()
        .find_map(|name| headers.get(name))
        .and_then(|value| value.to_str().ok())
        .map(|value| value.chars().take(256).collect())
}

fn redact_secret(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        text.to_string()
    } else {
        text.replace(secret, "<redacted>")
    }
}

fn redact_json(mut value: serde_json::Value, secret: &str) -> serde_json::Value {
    match &mut value {
        serde_json::Value::String(text) => *text = redact_secret(text, secret),
        serde_json::Value::Array(items) => {
            for item in items {
                *item = redact_json(std::mem::take(item), secret);
            }
        },
        serde_json::Value::Object(fields) => {
            for item in fields.values_mut() {
                *item = redact_json(std::mem::take(item), secret);
            }
        },
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {},
    }
    value
}

fn redact_termination(mut termination: ProviderTermination, secret: &str) -> ProviderTermination {
    termination.provider_status = termination
        .provider_status
        .as_deref()
        .map(|value| redact_secret(value, secret));
    termination.provider_reason = termination
        .provider_reason
        .as_deref()
        .map(|value| redact_secret(value, secret));
    termination
}

/// The API key and judge inputs that must never reach telemetry or the
/// diagnostic attempt metadata: the resolved key plus the command, reason, and
/// cwd sent to the judge. A provider could echo any of them in its request id
/// or termination status/reason.
fn redaction_secrets(client: &JudgeClient, request: &JudgeRequest) -> Vec<String> {
    let mut secrets = vec![client.config.api_key.clone()];
    secrets.push(request.command.clone());
    if let Some(reason) = &request.reason {
        secrets.push(reason.clone());
    }
    secrets.push(request.cwd.to_string_lossy().into_owned());
    secrets
}

/// Redact the API key and judge inputs from the provider-controlled attempt
/// fields (request id, termination status/reason) before the attempt is
/// recorded, so telemetry and the diagnostic attempt metadata cannot carry
/// credentials or command/reason/cwd values echoed back by a provider.
fn redact_attempt_provider_metadata(attempt: &mut JudgeAttemptTelemetry, secrets: &[String]) {
    if let Some(id) = &mut attempt.provider_request_id {
        *id = redact_secrets(id, secrets);
    }
    if let Some(termination) = &mut attempt.termination {
        if let Some(status) = &mut termination.provider_status {
            *status = redact_secrets(status, secrets);
        }
        if let Some(reason) = &mut termination.provider_reason {
            *reason = redact_secrets(reason, secrets);
        }
    }
}

fn redact_secrets(text: &str, secrets: &[String]) -> String {
    let mut redacted = text.to_string();
    for secret in secrets {
        redacted = redact_secret(&redacted, secret);
    }
    redacted
}

fn elapsed_ms(started: Instant) -> u64 {
    duration_ms(started.elapsed())
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}
