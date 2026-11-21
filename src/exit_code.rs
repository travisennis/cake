//! Exit codes for cake CLI.
//!
//! cake uses structured exit codes so that calling scripts, CI pipelines, and
//! other automation can distinguish between failure modes without parsing
//! stderr.
//!
//! | Code | Meaning       | Description                                              |
//! |------|---------------|----------------------------------------------------------|
//! | `0`  | Success       | The agent completed and produced a response               |
//! | `1`  | Agent error   | The model or a tool encountered an error during execution|
//! | `2`  | API error     | Rate limit, auth, network, or request-timeout error      |
//! | `3`  | Input error   | No prompt, invalid flags, bad configuration, missing key |
//! | `130`| Interrupted   | User interrupted the run with Ctrl-C                     |

use std::process::ExitCode;

use crate::clients::response_stall::BodyReadFailure;
use crate::config::settings::SettingsError;

/// Exit code constants for cake.
///
/// These values are returned from `main()` so that shell scripts and CI
/// pipelines can branch on the reason for failure.
pub mod code {
    /// Successful execution.
    pub const SUCCESS: u8 = 0;
    /// Agent or tool error during execution.
    pub const AGENT_ERROR: u8 = 1;
    /// API error (rate limit, auth, network, or request-timeout failure).
    pub const API_ERROR: u8 = 2;
    /// Input error (no prompt, invalid flags, bad configuration, missing API key).
    pub const INPUT_ERROR: u8 = 3;
    /// Interrupted by user (Ctrl-C).
    pub const INTERRUPTED: u8 = 130;
}

/// A structured API error that preserves the HTTP status code.
///
/// This allows `classify` to inspect the status code directly instead of
/// relying on fragile string matching against status code numbers that could
/// appear anywhere in an error message.
#[derive(Debug, thiserror::Error)]
#[error("{body}")]
pub struct ApiError {
    /// The HTTP status code from the API response.
    pub status: u16,
    /// The formatted error body (model name + response text).
    pub body: String,
    /// The provider's parsed input overflow, when the failure was a genuine
    /// input overflow: the request's input cannot fit alongside a usable output
    /// allowance. `None` for every other failure, including an overflow that
    /// only rejected the requested output allowance. A context-compaction owner
    /// consumes this verdict instead of re-parsing the body.
    pub input_overflow: Option<crate::clients::retry::InputOverflow>,
    /// Why the provider body did not arrive completely, when it did not.
    /// Status-derived remedies and native-constraint fallback require this to
    /// be `None`, so an incomplete body is never treated as a provider verdict.
    pub body_read_failure: Option<BodyReadFailure>,
}

/// Classify an `anyhow::Error` into a `u8` exit code value.
///
/// This is the primary classification function. Returns the raw `u8` code
/// which can be embedded in structured output (e.g. streaming JSON) or
/// converted to `std::process::ExitCode` via [`classify`].
pub fn classify_to_u8(err: &anyhow::Error) -> u8 {
    if let Some(code) = classify_typed_error(err) {
        return code;
    }

    for cause in err.chain() {
        if let Some(code) = classify_error_cause(cause) {
            return code;
        }
    }

    code::AGENT_ERROR
}

fn classify_typed_error(err: &anyhow::Error) -> Option<u8> {
    if let Some(api_err) = err.downcast_ref::<ApiError>() {
        return Some(classify_api_error_status(api_err.status));
    }

    if err
        .downcast_ref::<crate::clients::response_stall::ResponseStall>()
        .is_some()
    {
        return Some(code::API_ERROR);
    }

    if let Some(schema_err) = err.downcast_ref::<crate::config::OutputSchemaError>() {
        return Some(classify_schema_error(schema_err));
    }

    if let Some(settings_err) = err.downcast_ref::<SettingsError>() {
        return Some(classify_settings_error(settings_err));
    }

    if let Some(class) = classify_configuration_error(err) {
        return Some(class);
    }

    if let Some(gate_err) = err.downcast_ref::<crate::gates::composer::GatePipelineError>() {
        return Some(classify_gate_class(&gate_err.class()));
    }

    if is_input_configuration_error(err) {
        return Some(code::INPUT_ERROR);
    }

    if let Some(class) = classify_cli_command_error(err) {
        return Some(class);
    }

    None
}

fn classify_cli_command_error(err: &anyhow::Error) -> Option<u8> {
    if let Some(replay_err) = err.downcast_ref::<crate::cli::ReplayError>() {
        return Some(replay_err.exit_code());
    }

    if let Some(session_err) = err.downcast_ref::<crate::cli::SessionDiagnosticError>() {
        return Some(session_err.exit_code());
    }

    if let Some(checkpoint_err) = err.downcast_ref::<crate::cli::CheckpointError>() {
        return Some(checkpoint_err.exit_code());
    }

    None
}

/// Configuration-state conflicts, root agent-run options combined with a
/// subcommand, and data-directory preparation failures are input errors.
fn is_input_configuration_error(err: &anyhow::Error) -> bool {
    is_cli_input_error(err) || err.downcast_ref::<crate::config::DataDirError>().is_some()
}

fn is_cli_input_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<crate::cli::SandboxGrantError>()
        .is_some()
        || err
            .downcast_ref::<crate::cli::InvalidSubcommandOptions>()
            .is_some()
        || matches!(
            err.downcast_ref::<crate::cli::InitError>(),
            Some(crate::cli::InitError::Conflict(_))
        )
}

/// Classify a structured [`ApiError`]'s HTTP status.
///
/// Class 2 is the provider-failure class: authentication (`401`, `403`), rate
/// limiting (`429`), and request timeouts (`408`). A `408` is the status
/// counterpart of the client-side timeouts that already map here ---
/// `reqwest::Error::is_timeout()` and a typed request timeout --- so the same
/// provider failure exits the same way whichever layer observed it.
///
/// Retryability is a separate transport-recovery concern and does not decide
/// the class: `409` (lock timeout), `5xx`, and `529` are retried but stay agent
/// errors, because the provider answered. A tool-level timeout (a Bash tool
/// waiting on its own overrides) is a tool error for the same reason.
const fn classify_api_error_status(status: u16) -> u8 {
    match status {
        401 | 403 | 408 | 429 => code::API_ERROR,
        _ => code::AGENT_ERROR,
    }
}

/// Classify a typed output-schema error.
const fn classify_schema_error(error: &crate::config::OutputSchemaError) -> u8 {
    match error {
        crate::config::OutputSchemaError::Unreadable { .. }
        | crate::config::OutputSchemaError::InvalidJson { .. }
        | crate::config::OutputSchemaError::InvalidSchema { .. } => code::INPUT_ERROR,
        crate::config::OutputSchemaError::Unsatisfied { .. } => code::AGENT_ERROR,
    }
}

/// Project trust, explicit configuration files, and MCP management failures.
///
/// Trust and named configuration-file failures are configuration defects: the
/// invocation cannot use the checkout until the user trusts it or selects an
/// explicit one-run mode, and a named config file that is missing or malformed
/// is input the user supplied. They share the settings input-error class. An
/// MCP management failure carries its own class: invalid definitions and
/// missing names are input errors, and file read or write failures are
/// operational errors.
fn classify_configuration_error(err: &anyhow::Error) -> Option<u8> {
    if let Some(mcp_err) = err.downcast_ref::<crate::config::mcp::McpManageError>() {
        return Some(mcp_err.exit_code());
    }
    if err
        .downcast_ref::<crate::config::provider_identity::ProviderIdentityError>()
        .is_some()
    {
        return Some(code::INPUT_ERROR);
    }
    let is_project_error = err
        .downcast_ref::<crate::config::trust::TrustError>()
        .is_some()
        || err
            .downcast_ref::<crate::config::trust::ConfigLoadError>()
            .is_some()
        || err
            .downcast_ref::<crate::config::trust::ProjectConfigModeError>()
            .is_some()
        || err
            .downcast_ref::<crate::config::mcp::McpConfigError>()
            .is_some();
    is_project_error.then_some(code::INPUT_ERROR)
}

/// Classify a settings load or parse failure.
///
/// Every variant is a defect in the configuration input --- an unreadable,
/// unparsable, or self-contradictory settings file --- so all of them are input
/// errors. The match is exhaustive with no wildcard arm, so a new variant must
/// state its class here instead of inheriting one by accident.
const fn classify_settings_error(error: &SettingsError) -> u8 {
    match error {
        SettingsError::InvalidModelName { .. }
        | SettingsError::InvalidModelBaseUrl { .. }
        | SettingsError::InvalidModelApiKeyEnv { .. }
        | SettingsError::DuplicateModelName { .. }
        | SettingsError::DuplicateReasoningEffort { .. }
        | SettingsError::GeneratedModelNameConflict { .. }
        | SettingsError::InvalidProfileName { .. }
        | SettingsError::ProfileModelsUnsupported { .. }
        | SettingsError::ProjectModelsUnsupported { .. }
        | SettingsError::ProjectToolsBroadened { .. }
        | SettingsError::UnknownProfile { .. }
        | SettingsError::DefaultModelNotFound { .. }
        | SettingsError::CompactionModelNotFound { .. }
        | SettingsError::JudgeModelNotFound { .. }
        | SettingsError::InvalidGateConfiguration { .. }
        | SettingsError::ParseError(_)
        | SettingsError::SerializeError(_)
        | SettingsError::IoError(_) => code::INPUT_ERROR,
    }
}

/// Classify a gate failure: timeouts and network/auth/rate-limit failures are
/// API errors, configuration is an input error, and other failures are agent
/// errors.
const fn classify_gate_class(class: &crate::gates::protocol::GateErrorClass) -> u8 {
    use crate::gates::protocol::GateErrorClass;

    match class {
        GateErrorClass::Timeout
        | GateErrorClass::Network
        | GateErrorClass::Auth
        | GateErrorClass::RateLimit => code::API_ERROR,
        GateErrorClass::Config => code::INPUT_ERROR,
        GateErrorClass::Transport
        | GateErrorClass::Malformed
        | GateErrorClass::Refusal
        | GateErrorClass::Unsupported
        | GateErrorClass::Internal
        | GateErrorClass::Protocol
        | GateErrorClass::Crash
        | GateErrorClass::Other(_) => code::AGENT_ERROR,
    }
}

fn classify_error_cause(cause: &(dyn std::error::Error + 'static)) -> Option<u8> {
    if let Some(req_err) = cause.downcast_ref::<reqwest::Error>()
        && is_reqwest_api_error(req_err)
    {
        return Some(code::API_ERROR);
    }

    let msg = cause.to_string();
    if is_input_error(&msg) {
        return Some(code::INPUT_ERROR);
    }

    if is_api_network_error(&msg) {
        return Some(code::API_ERROR);
    }

    None
}

/// Classify an `anyhow::Error` into an `ExitCode`.
///
/// Convenience wrapper around [`classify_to_u8`] for use in `main()`.
pub fn classify(err: &anyhow::Error) -> ExitCode {
    ExitCode::from(classify_to_u8(err))
}

/// Check if a `reqwest::Error` represents an API-level failure.
fn is_reqwest_api_error(req_err: &reqwest::Error) -> bool {
    if let Some(status) = req_err.status()
        && classify_api_error_status(status.as_u16()) == code::API_ERROR
    {
        return true;
    }
    if req_err.is_connect() {
        return true;
    }
    if req_err.is_timeout() {
        return true;
    }
    false
}

const INPUT_ERROR_PATTERNS: &[&str] = &[
    "No input provided",
    "stdin input exceeds",
    "Invalid model name",
    "Invalid model configuration",
    "Unknown model",
    "No model specified",
    "is not configured in settings.toml",
    "Invalid session UUID",
    "Invalid session reference",
    "No previous session found",
    "Failed to open session file",
    "Working directory mismatch",
    "Session model mismatch",
    "Invalid session model change override",
    "Failed to cd into worktree",
    "Failed to get current directory",
];

const COMPOUND_INPUT_ERROR_PATTERNS: &[&[&str]] = &[
    &["Environment variable", "is not set", "API key"],
    &["Environment variable", "is set but empty"],
    &["Session", "not found"],
    &["Session model", "is ambiguous"],
    &["Failed to parse", "session file"],
    &["error:", "USAGE"],
];

/// Determine if an error message indicates an input/validation error.
fn is_input_error(msg: &str) -> bool {
    INPUT_ERROR_PATTERNS
        .iter()
        .any(|pattern| msg.contains(pattern))
        || COMPOUND_INPUT_ERROR_PATTERNS
            .iter()
            .any(|patterns| patterns.iter().all(|pattern| msg.contains(pattern)))
}

/// Determine if an error message indicates a network/connection error.
///
/// This only matches network-level patterns (connection refused, DNS, timeout).
/// HTTP status code classification is handled structurally via [`ApiError`].
pub fn is_api_network_error(msg: &str) -> bool {
    if msg.contains("error sending request")
        || msg.contains("connection refused")
        || msg.contains("connection timed out")
        || msg.contains("dns error")
        || msg.contains("resolve error")
    {
        return true;
    }

    false
}
