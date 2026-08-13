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
/// converted to `std::process::ExitCode` via [`classify`].
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
        return Some(classify_api_error_status(api_err.status));
    }

    // Typed output-schema errors: pre-run schema file problems are input
    // errors; a run that cannot satisfy the schema is an agent error.
    if let Some(schema_err) = err.downcast_ref::<crate::config::OutputSchemaError>() {
        return Some(classify_schema_error(schema_err));
    }

    // Judge failures are provider/agent errors: a timeout or transport
    // failure is an API/network error, while a malformed verdict or refusal
    // is a judge (agent) error. Matches the ApiError convention: auth and
    // rate-limit HTTP statuses are API errors.
    if let Some(judge_err) = err.downcast_ref::<crate::clients::judge::JudgeError>() {
        return Some(classify_judge_error(judge_err));
    }

    None
}

/// Classify a structured [`ApiError`]'s HTTP status.
const fn classify_api_error_status(status: u16) -> u8 {
    match status {
        401 | 403 | 429 => code::API_ERROR,
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

/// Classify a judge failure: timeouts and network/auth/rate-limit transport
/// failures are API errors; other HTTP failures, malformed verdicts, and
/// refusals are judge errors.
fn classify_judge_error(error: &crate::clients::judge::JudgeError) -> u8 {
    match error {
        crate::clients::judge::JudgeError::Timeout(_) => code::API_ERROR,
        crate::clients::judge::JudgeError::Transport {
            status: Some(status),
            ..
        } => classify_api_error_status(*status),
        crate::clients::judge::JudgeError::Transport {
            status: None,
            detail,
        } => {
            if is_api_network_error(detail) {
                code::API_ERROR
            } else {
                code::AGENT_ERROR
            }
        },
        crate::clients::judge::JudgeError::Malformed(_)
        | crate::clients::judge::JudgeError::Refusal => code::AGENT_ERROR,
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

const INPUT_ERROR_PATTERNS: &[&str] = &[
    "No input provided",
    "stdin input exceeds",
    "Invalid model name",
    "Unknown model",
    "No model specified",
    "is not configured in settings.toml",
    "Invalid session UUID",
    "Invalid session reference",
    "No previous session found",
    "Failed to open session file",
    "Failed to read judge rubric file",
    "Working directory mismatch",
    "Session model mismatch",
    "Failed to cd into worktree",
    "Failed to get current directory",
];

const COMPOUND_INPUT_ERROR_PATTERNS: &[&[&str]] = &[
    &["Environment variable", "is not set", "API key"],
    &["Environment variable", "is set but empty"],
    &["Session", "not found"],
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
    fn input_error_patterns_are_recognized() {
        let cases = [
            (
                "missing API key",
                "Environment variable TOKEN is not set; provide an API key",
            ),
            (
                "empty API key",
                "Environment variable TOKEN is set but empty",
            ),
            ("missing prompt", "No input provided"),
            ("missing stdin", "No input provided via stdin"),
            ("oversized stdin", "stdin input exceeds the maximum size"),
            ("invalid model", "Invalid model name 'BAD'"),
            ("unknown model", "Unknown model 'missing'"),
            ("missing model", "No model specified"),
            (
                "unconfigured session model",
                "Session model is not configured in settings.toml",
            ),
            ("invalid session UUID", "Invalid session UUID 'bad'"),
            (
                "invalid session reference",
                "Invalid session reference 'bad'",
            ),
            ("missing previous session", "No previous session found"),
            ("missing session", "Session abc not found"),
            ("unreadable session file", "Failed to open session file"),
            ("invalid session file", "Failed to parse session file"),
            ("working directory mismatch", "Working directory mismatch"),
            ("session model mismatch", "Session model mismatch"),
            ("clap error", "error: invalid option\nUSAGE: cake"),
            ("invalid worktree", "Failed to cd into worktree"),
            (
                "unavailable current directory",
                "Failed to get current directory",
            ),
        ];

        for (name, message) in cases {
            assert!(is_input_error(message), "{name}: {message}");
        }
    }

    #[test]
    fn compound_input_error_patterns_require_every_fragment() {
        let messages = [
            "Environment variable TOKEN is not set",
            "Environment variable TOKEN needs an API key",
            "TOKEN is not set; provide an API key",
            "Environment variable TOKEN is empty",
            "TOKEN is set but empty",
            "Session exists",
            "record not found",
            "Failed to parse settings file",
            "invalid session file",
            "error: invalid option",
            "USAGE: cake",
        ];

        for message in messages {
            assert!(!is_input_error(message), "{message}");
        }
    }

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
    fn classify_judge_timeout_as_api_error() {
        let err = anyhow::Error::new(crate::clients::judge::JudgeError::Timeout(
            std::time::Duration::from_secs(30),
        ));
        assert_eq!(classify_to_u8(&err), code::API_ERROR);
    }

    #[test]
    fn classify_judge_auth_transport_as_api_error() {
        let err = anyhow::Error::new(crate::clients::judge::JudgeError::Transport {
            status: Some(401),
            detail: "invalid api key".to_string(),
        });
        assert_eq!(classify_to_u8(&err), code::API_ERROR);

        let err = anyhow::Error::new(crate::clients::judge::JudgeError::Transport {
            status: Some(429),
            detail: "rate limited".to_string(),
        });
        assert_eq!(classify_to_u8(&err), code::API_ERROR);
    }

    #[test]
    fn classify_judge_network_transport_as_api_error() {
        let err = anyhow::Error::new(crate::clients::judge::JudgeError::Transport {
            status: None,
            detail: "error sending request: connection refused".to_string(),
        });
        assert_eq!(classify_to_u8(&err), code::API_ERROR);
    }

    #[test]
    fn classify_judge_server_transport_as_agent_error() {
        let err = anyhow::Error::new(crate::clients::judge::JudgeError::Transport {
            status: Some(500),
            // Provider bodies often quote upstream failures; the structured
            // status must win over status-like text in the body.
            detail: "upstream said HTTP 401: auth failed".to_string(),
        });
        assert_eq!(classify_to_u8(&err), code::AGENT_ERROR);

        let err = anyhow::Error::new(crate::clients::judge::JudgeError::Transport {
            status: Some(502),
            detail: "error sending request to upstream".to_string(),
        });
        assert_eq!(classify_to_u8(&err), code::AGENT_ERROR);
    }

    #[test]
    fn classify_judge_malformed_and_refusal_as_agent_error() {
        let err = anyhow::Error::new(crate::clients::judge::JudgeError::Malformed(
            "bad json".to_string(),
        ));
        assert_eq!(classify_to_u8(&err), code::AGENT_ERROR);

        let err = anyhow::Error::new(crate::clients::judge::JudgeError::Refusal);
        assert_eq!(classify_to_u8(&err), code::AGENT_ERROR);
    }

    #[test]
    fn classify_unreadable_judge_rubric_as_input_error() {
        let err = anyhow::anyhow!(
            "Failed to read judge rubric file /work/rubric.md: No such file or directory"
        );
        assert_eq!(classify_to_u8(&err), code::INPUT_ERROR);
    }

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
