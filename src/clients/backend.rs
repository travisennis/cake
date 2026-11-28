use crate::config::model::ApiType;
use crate::types::ReportedUsage;

/// A successful provider response whose body does not match the backend's
/// JSON envelope.
///
/// Keeping this as a typed boundary error lets callers distinguish a body
/// decode failure, which may be transient, from later semantic parsing errors
/// that should remain terminal.
#[derive(Debug, thiserror::Error)]
#[error(
    "failed to decode {backend} response body: {source}; first {preview_len} bytes: {preview:?}"
)]
pub struct ResponseDecodeError {
    backend: &'static str,
    preview_len: usize,
    preview: String,
    reported_usage: Option<ReportedUsage>,
    #[source]
    source: serde_json::Error,
}

impl ResponseDecodeError {
    pub fn new(
        backend: &'static str,
        body: &[u8],
        reported_usage: Option<ReportedUsage>,
        source: serde_json::Error,
    ) -> Self {
        let preview_len = body.len().min(400);
        Self {
            backend,
            preview_len,
            preview: String::from_utf8_lossy(&body[..preview_len]).into_owned(),
            reported_usage,
            source,
        }
    }

    pub(super) const fn usage(&self) -> Option<ReportedUsage> {
        self.reported_usage
    }

    /// Detail without the response-body preview, safe for telemetry.
    pub fn telemetry_detail(&self) -> String {
        format!(
            "failed to decode {} response body: {:?} error at line {} column {}",
            self.backend,
            self.source.classify(),
            self.source.line(),
            self.source.column()
        )
    }
}

/// A valid provider response whose content could not be converted into
/// conversation items. The normalized usage remains available to the runner
/// even though the response itself is discarded.
#[derive(Debug, thiserror::Error)]
#[error("{source}")]
pub struct ResponseParseError {
    #[source]
    source: anyhow::Error,
    telemetry_detail: String,
    reported_usage: Option<ReportedUsage>,
}

impl ResponseParseError {
    pub fn new(source: anyhow::Error, reported_usage: Option<ReportedUsage>) -> Self {
        let telemetry_detail = format!("{source:#}");
        Self {
            source,
            telemetry_detail,
            reported_usage,
        }
    }

    /// Construct a parse error whose telemetry detail omits a body preview
    /// that the human-facing `Display` text still carries.
    pub const fn new_with_telemetry_detail(
        source: anyhow::Error,
        reported_usage: Option<ReportedUsage>,
        telemetry_detail: String,
    ) -> Self {
        Self {
            source,
            telemetry_detail,
            reported_usage,
        }
    }

    pub(super) const fn usage(&self) -> Option<ReportedUsage> {
        self.reported_usage
    }

    /// Detail without the response-body preview, safe for telemetry.
    pub fn telemetry_detail(&self) -> &str {
        &self.telemetry_detail
    }
}

/// Native structured-output constraint attached to correction-turn requests
/// when `--output-schema` enforcement needs a corrected final message.
///
/// Local validation remains authoritative; this is best-effort acceleration.
/// It is threaded as its own parameter rather than a `RequestOverrides` field
/// because overrides drive HTTP retry semantics and telemetry snapshots.
#[derive(Debug, Clone, Copy)]
pub(super) struct FinalOutputConstraint<'a> {
    pub(super) name: &'a str,
    pub(super) schema: &'a serde_json::Value,
}

/// The provider-level choice among the tools offered in a request.
///
/// This changes only `tool_choice`, never the tool set, so a constrained run
/// keeps the same tool schemas and prompt prefix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum ToolChoice {
    /// Let the model decide whether to call a tool.
    #[default]
    Auto,
    /// Forbid tool calls for this run.
    None,
    /// Require at least one tool call.
    Required,
}

impl ToolChoice {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::None => "none",
            Self::Required => "required",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) enum Backend {
    Responses,
    ChatCompletions,
}

impl Backend {
    pub(super) const fn from_api_type(api_type: ApiType) -> Self {
        match api_type {
            ApiType::Responses => Self::Responses,
            ApiType::ChatCompletions => Self::ChatCompletions,
        }
    }
}
