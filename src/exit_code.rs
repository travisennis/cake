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
//! | `2`  | API error     | Rate limit, auth failure, or network error               |
//! | `3`  | Input error   | No prompt provided, invalid flags, missing API key       |
//! | `130`| Interrupted   | User interrupted the run with Ctrl-C                     |

use std::process::ExitCode;

/// Exit code constants for cake.
///
/// These values are returned from `main()` so that shell scripts and CI
/// pipelines can branch on the reason for failure.
pub mod code {
    /// Successful execution.
    pub const SUCCESS: u8 = 0;
    /// Agent or tool error during execution.
    pub const AGENT_ERROR: u8 = 1;
    /// API error (rate limit, auth failure, network error).
    pub const API_ERROR: u8 = 2;
    /// Input error (no prompt, invalid flags, missing API key).
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
}

/// Classify an `anyhow::Error` into a `u8` exit code value.
///
/// This is the primary classification function. Returns the raw `u8` code
/// which can be embedded in structured output (e.g. streaming JSON) or
/// converted to `std::process::ExitCode` via [`classify_to_exit_code`].
pub fn classify_to_u8(err: &anyhow::Error) -> u8 {
    if let Some(code) = classify_typed_error(err) {
        return code;
    }

    // Walk the error chain for reqwest::Error and string-based patterns.
    for cause in err.chain() {
        if let Some(code) = classify_error_cause(cause) {
            return code;
        }
    }

    // Default: agent/tool error
    code::AGENT_ERROR
}

fn classify_typed_error(err: &anyhow::Error) -> Option<u8> {
    // Check for structured ApiError first — this gives us reliable status codes.
    if let Some(api_err) = err.downcast_ref::<ApiError>() {
        return Some(match api_err.status {
            401 | 403 | 429 => code::API_ERROR,
            _ => code::AGENT_ERROR,
        });
    }

    // Typed output-schema errors: pre-run schema file problems are input
    // errors; a run that cannot satisfy the schema is an agent error.
    if let Some(schema_err) = err.downcast_ref::<crate::config::OutputSchemaError>() {
        return Some(match schema_err {
            crate::config::OutputSchemaError::Unreadable { .. }
            | crate::config::OutputSchemaError::InvalidJson { .. }
            | crate::config::OutputSchemaError::InvalidSchema { .. } => code::INPUT_ERROR,
            crate::config::OutputSchemaError::Unsatisfied { .. } => code::AGENT_ERROR,
        });
    }

    None
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

    // These cover network/connection errors that appear as string messages
    // rather than typed reqwest errors (e.g. when re-wrapped by anyhow).
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
    // Auth failures and rate limiting (401/403/429)
    if let Some(status) = req_err.status()
        && matches!(status.as_u16(), 401 | 403 | 429)
    {
        return true;
    }
    // Connection failures
    if req_err.is_connect() {
        return true;
    }
    // Timeouts
    if req_err.is_timeout() {
        return true;
    }
    false
}

/// Determine if an error message indicates an input/validation error.
fn is_input_error(msg: &str) -> bool {
    // Missing API key
    if msg.contains("Environment variable") && msg.contains("is not set") && msg.contains("API key")
    {
        return true;
    }
    if msg.contains("Environment variable") && msg.contains("is set but empty") {
        return true;
    }

    // Missing prompt
    if msg.contains("No input provided") {
        return true;
    }
    if msg.contains("No input provided via stdin") {
        return true;
    }
    if msg.contains("stdin input exceeds") {
        return true;
    }

    // Invalid model name
    if msg.contains("Invalid model name") {
        return true;
    }
    if msg.contains("Unknown model") {
        return true;
    }

    // No model specified (no --model and no default_model in settings)
    if msg.contains("No model specified") {
        return true;
    }

    // Session model not configured
    if msg.contains("is not configured in settings.toml") {
        return true;
    }

    // Invalid session reference
    if msg.contains("Invalid session UUID") || msg.contains("Invalid session reference") {
        return true;
    }

    // Session not found
    if msg.contains("No previous session found") {
        return true;
    }
    if msg.contains("Session") && msg.contains("not found") {
        return true;
    }

    // Resume/fork file path errors
    if msg.contains("Failed to open session file") {
        return true;
    }
    if msg.contains("Failed to parse") && msg.contains("session file") {
        return true;
    }

    // Working directory mismatch for file-based resume/fork
    if msg.contains("Working directory mismatch") {
        return true;
    }

    // Model mismatch when resuming a session
    if msg.contains("Session model mismatch") {
        return true;
    }

    // clap argument errors (e.g. required arguments missing, bad flag values)
    if msg.contains("error:") && msg.contains("USAGE") {
        return true;
    }

    // Worktree errors that are input-related
    if msg.contains("Failed to cd into worktree") {
        return true;
    }

    // Failed to get current directory (unlikely but input-related)
    if msg.contains("Failed to get current directory") {
        return true;
    }

    false
}

/// Determine if an error message indicates a network/connection error.
///
/// This only matches network-level patterns (connection refused, DNS, timeout).
/// HTTP status code classification is handled structurally via [`ApiError`].
fn is_api_network_error(msg: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_code_is_zero() {
        assert_eq!(code::SUCCESS, 0);
    }

    #[test]
    fn agent_error_code_is_one() {
        assert_eq!(code::AGENT_ERROR, 1);
    }

    #[test]
    fn api_error_code_is_two() {
        assert_eq!(code::API_ERROR, 2);
    }

    #[test]
    fn input_error_code_is_three() {
        assert_eq!(code::INPUT_ERROR, 3);
    }

    // --- Input error classification ---

    #[test]
    fn classify_missing_api_key() {
        let err = anyhow::anyhow!(
            "Environment variable 'OPENCODE_ZEN_API_TOKEN' is not set. \
             Please set it to your API key: environment variable not found"
        );
        assert_eq!(classify_to_u8(&err), code::INPUT_ERROR);
    }

    #[test]
    fn classify_empty_api_key() {
        let err = anyhow::anyhow!("Environment variable 'OPENCODE_ZEN_API_TOKEN' is set but empty");
        assert_eq!(classify_to_u8(&err), code::INPUT_ERROR);
    }

    #[test]
    fn classify_no_input() {
        let err = anyhow::anyhow!(
            "No input provided. Provide a prompt as an argument, use 'cake -' for stdin, or pipe input to cake."
        );
        assert_eq!(classify_to_u8(&err), code::INPUT_ERROR);
    }

    #[test]
    fn classify_no_stdin() {
        let err = anyhow::anyhow!("No input provided via stdin");
        assert_eq!(classify_to_u8(&err), code::INPUT_ERROR);
    }

    #[test]
    fn classify_stdin_exceeds_size_limit() {
        let err = anyhow::anyhow!(
            "stdin input exceeds the maximum allowed size (10 MB). \
             Pipe the content to a file first and reference the file path instead."
        );
        assert_eq!(classify_to_u8(&err), code::INPUT_ERROR);
    }

    #[test]
    fn classify_invalid_model_name() {
        let err = anyhow::anyhow!(
            "Invalid model name 'Invalid Name!': names must contain only lowercase letters"
        );
        assert_eq!(classify_to_u8(&err), code::INPUT_ERROR);
    }

    #[test]
    fn classify_unknown_model() {
        let err = anyhow::anyhow!(
            "Unknown model 'nonexistent': claude, deepseek. Use a model name from settings.toml"
        );
        assert_eq!(classify_to_u8(&err), code::INPUT_ERROR);
    }

    #[test]
    fn classify_no_model_specified() {
        let err = anyhow::anyhow!("No model specified. cake needs a model configuration to run.");
        assert_eq!(classify_to_u8(&err), code::INPUT_ERROR);
    }

    #[test]
    fn classify_session_model_not_configured() {
        let err = anyhow::anyhow!(
            "Session model 'glm-5' is not configured in settings.toml. \
             Add a [[models]] entry for 'glm-5' to continue this session"
        );
        assert_eq!(classify_to_u8(&err), code::INPUT_ERROR);
    }

    #[test]
    fn classify_invalid_session_uuid() {
        let err = anyhow::anyhow!("Invalid session UUID 'not-a-uuid': invalid character");
        assert_eq!(classify_to_u8(&err), code::INPUT_ERROR);
    }

    #[test]
    fn classify_no_previous_session() {
        let err = anyhow::anyhow!("No previous session found for this directory");
        assert_eq!(classify_to_u8(&err), code::INPUT_ERROR);
    }

    #[test]
    fn classify_session_not_found() {
        let err = anyhow::anyhow!("Session abc123 not found");
        assert_eq!(classify_to_u8(&err), code::INPUT_ERROR);
    }

    // --- Output schema error classification ---

    #[test]
    fn classify_output_schema_unreadable_as_input_error() {
        let err = anyhow::Error::new(crate::config::OutputSchemaError::Unreadable {
            path: "/tmp/missing.json".into(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "not found"),
        });
        assert_eq!(classify_to_u8(&err), code::INPUT_ERROR);
    }

    #[test]
    fn classify_output_schema_invalid_json_as_input_error() {
        let source = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let err = anyhow::Error::new(crate::config::OutputSchemaError::InvalidJson {
            path: "/tmp/schema.json".into(),
            source,
        });
        assert_eq!(classify_to_u8(&err), code::INPUT_ERROR);
    }

    #[test]
    fn classify_output_schema_invalid_schema_as_input_error() {
        let err = anyhow::Error::new(crate::config::OutputSchemaError::InvalidSchema {
            path: "/tmp/schema.json".into(),
            detail: "123 is not of type 'string'".to_string(),
        });
        assert_eq!(classify_to_u8(&err), code::INPUT_ERROR);
    }

    #[test]
    fn classify_output_schema_unsatisfied_as_agent_error() {
        let err = anyhow::Error::new(crate::config::OutputSchemaError::Unsatisfied {
            attempts: 3,
            detail: "\"summary\" is a required property".to_string(),
        });
        assert_eq!(classify_to_u8(&err), code::AGENT_ERROR);
    }

    #[test]
    fn classify_output_schema_unsatisfied_with_context_as_agent_error() {
        // The typed downcast must win even when the error is wrapped with
        // context whose message could pattern-match string-based rules.
        let err = anyhow::Error::new(crate::config::OutputSchemaError::Unsatisfied {
            attempts: 3,
            detail: "connection refused is a required property".to_string(),
        })
        .context("agent run failed");
        assert_eq!(classify_to_u8(&err), code::AGENT_ERROR);
    }

    // --- API error classification via structured ApiError ---

    #[test]
    fn classify_rate_limit_via_api_error() {
        let err = anyhow::Error::new(ApiError {
            status: 429,
            body: "glm-5\n\nRate limit exceeded".to_string(),
        });
        assert_eq!(classify_to_u8(&err), code::API_ERROR);
    }

    #[test]
    fn classify_auth_failure_via_api_error() {
        let err = anyhow::Error::new(ApiError {
            status: 401,
            body: "glm-5\n\nInvalid API key".to_string(),
        });
        assert_eq!(classify_to_u8(&err), code::API_ERROR);
    }

    #[test]
    fn classify_forbidden_via_api_error() {
        let err = anyhow::Error::new(ApiError {
            status: 403,
            body: "glm-5\n\nForbidden".to_string(),
        });
        assert_eq!(classify_to_u8(&err), code::API_ERROR);
    }

    #[test]
    fn classify_server_error_via_api_error() {
        let err = anyhow::Error::new(ApiError {
            status: 500,
            body: "glm-5\n\nInternal server error".to_string(),
        });
        assert_eq!(classify_to_u8(&err), code::AGENT_ERROR);
    }

    // --- API error classification via network patterns ---

    #[test]
    fn classify_connection_refused() {
        let err = anyhow::anyhow!("connection refused");
        assert_eq!(classify_to_u8(&err), code::API_ERROR);
    }

    #[test]
    fn classify_dns_error() {
        let err = anyhow::anyhow!("dns error: could not resolve host");
        assert_eq!(classify_to_u8(&err), code::API_ERROR);
    }

    // --- Agent error classification (default) ---

    #[test]
    fn classify_generic_error_as_agent_error() {
        let err = anyhow::anyhow!("Something unexpected went wrong");
        assert_eq!(classify_to_u8(&err), code::AGENT_ERROR);
    }

    #[test]
    fn classify_parse_error_as_agent_error() {
        let err = anyhow::anyhow!("Failed to deserialize API response");
        assert_eq!(classify_to_u8(&err), code::AGENT_ERROR);
    }

    // --- Verify that bare status code numbers don't cause false positives ---

    #[test]
    fn bare_429_in_message_is_not_api_error() {
        let err = anyhow::anyhow!("Found 429 results in the database");
        assert_eq!(classify_to_u8(&err), code::AGENT_ERROR);
    }

    #[test]
    fn bare_401_in_message_is_not_api_error() {
        let err = anyhow::anyhow!("File at /path/401/index.html not found");
        assert_eq!(classify_to_u8(&err), code::AGENT_ERROR);
    }

    // --- classify() returns correct ExitCode ---

    #[test]
    fn classify_returns_exit_code() {
        let err = anyhow::anyhow!("Something unexpected went wrong");
        assert_eq!(classify(&err), ExitCode::from(code::AGENT_ERROR));

        let err = anyhow::anyhow!("No input provided");
        assert_eq!(classify(&err), ExitCode::from(code::INPUT_ERROR));
    }
}
