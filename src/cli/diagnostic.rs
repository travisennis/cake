//! The versioned diagnostic JSON document emitted by machine-mode commands.
//!
//! `debug models --json`, `debug skills --json`, `sessions list --json`, and
//! `bash check --json` all emit one object with the same top-level fields, so a
//! consumer parses one shape instead of one dialect per command. The contract's
//! semantics live in [docs/integrations.md](../../docs/integrations.md); the
//! decision is recorded in [ADR 029](../../docs/adr/029-diagnostic-json-document.md).
//!
//! Every document is:
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "command": "debug models",
//!   "status": "ok",
//!   "summary": { "count": 1 },
//!   "checks": [],
//!   "data": { "default_model": null, "models": [] }
//! }
//! ```
//!
//! Human-readable diagnostics stay on stderr, so stdout carries the document
//! and nothing else. Arrays the document contains are ordered by the command
//! that builds them, never by directory iteration or hash order.

use serde::Serialize;
use serde_json::{Value, json};

/// Schema version of the diagnostic JSON document.
///
/// Consumers switch on this value. A change to the document's top-level shape
/// or to the meaning of an existing field increments it.
pub const SCHEMA_VERSION: u32 = 1;

/// Check id for an unrecognized settings key reported by the settings loader.
pub const CHECK_SETTINGS_UNKNOWN_KEY: &str = "settings.unknown_key";

/// Check id for settings that could not be loaded or parsed.
pub const CHECK_SETTINGS_LOAD_FAILED: &str = "settings.load_failed";

/// Check id for a skill discovery, parse, or selection diagnostic.
pub const CHECK_SKILLS_DISCOVERY: &str = "skills.discovery_failed";

/// Check id for a configured grant source path that is missing, invalid, or
/// could not be inspected.
pub const CHECK_GRANTS_PATH: &str = "grants.path_problem";

/// Check id for a selected model name that the loaded settings do not define.
pub const CHECK_CONFIG_MODEL: &str = "config.model_unavailable";

/// Check id for a hook file that could not be read or parsed.
pub const CHECK_CONFIG_HOOKS: &str = "config.hooks_invalid";

/// Check id for authority-bearing project configuration that was not trusted
/// during a configuration report.
pub const CHECK_CONFIG_PROJECT_TRUST: &str = "config.project_trust";

/// Check id for authority-bearing project configuration that was not trusted.
pub const CHECK_SKILLS_PROJECT_TRUST: &str = "skills.project_trust";

/// Check id for a sessions directory that could not be read.
pub const CHECK_SESSIONS_DIRECTORY: &str = "sessions.directory_unreadable";

/// Check id for a session file or directory entry that could not be inspected
/// during discovery.
pub const CHECK_SESSIONS_DISCOVERY: &str = "sessions.discovery_failed";

/// Check id for a checkpoint append that failed.
pub const CHECK_SESSIONS_CHECKPOINT: &str = "sessions.checkpoint_failed";

/// Check id for a blob directory that is missing, unsafe, or unreadable.
pub const CHECK_BLOBS_DIRECTORY: &str = "blobs.directory_problem";

/// Check id for a session file whose payload references could not be read fully.
pub const CHECK_BLOBS_SESSION: &str = "blobs.session_problem";

/// Check id for a referenced payload blob that is missing.
pub const CHECK_BLOBS_REFERENCE_MISSING: &str = "blobs.reference_missing";

/// Check id for a referenced payload blob whose bytes do not verify.
pub const CHECK_BLOBS_REFERENCE_CORRUPT: &str = "blobs.reference_corrupt";

/// Check id for published blobs that no session record references.
pub const CHECK_BLOBS_UNREFERENCED: &str = "blobs.unreferenced";

/// Check id for a stale temporary file left by an interrupted publication.
pub const CHECK_BLOBS_TEMPORARY: &str = "blobs.temporary_file";

/// Check id for a blob directory with no matching session file.
pub const CHECK_BLOBS_ORPHANED_STORE: &str = "blobs.orphaned_store";

/// Check id for a session diagnostic invocation that cannot proceed because
/// its identity or destination is invalid.
pub const CHECK_SESSION_INPUT: &str = "session.invalid_input";

/// Check id for a session transcript that is malformed or internally
/// inconsistent.
pub const CHECK_SESSION_TRANSCRIPT_CORRUPT: &str = "session.transcript_corrupt";

/// Check id for a session transcript that could not be opened or read.
pub const CHECK_SESSION_TRANSCRIPT_UNREADABLE: &str = "session.transcript_unreadable";

/// Check id for a truncated final record in a session artifact.
pub const CHECK_SESSION_PARTIAL_TAIL: &str = "session.partial_tail";

/// Check id for a session telemetry sidecar that is absent.
pub const CHECK_SESSION_TELEMETRY_MISSING: &str = "session.telemetry_missing";

/// Check id for a session telemetry sidecar with a malformed record.
pub const CHECK_SESSION_TELEMETRY_CORRUPT: &str = "session.telemetry_corrupt";

/// Check id for a session telemetry sidecar that could not be opened or read.
pub const CHECK_SESSION_TELEMETRY_UNREADABLE: &str = "session.telemetry_unreadable";

/// Check id for a session liveness probe that could not determine whether a
/// writer holds the session.
pub const CHECK_SESSION_LIVENESS_UNKNOWN: &str = "session.liveness_unknown";

/// Check id for a started task with no terminal completion record.
pub const CHECK_SESSION_INCOMPLETE_TASK: &str = "session.incomplete_task";

/// Check id for a session diagnostic that omitted the current configuration
/// context because settings could not be loaded.
pub const CHECK_SESSION_CONFIG_UNAVAILABLE: &str = "session.current_config_unavailable";

/// Check id for a log directory or log entry that could not be inspected.
pub const CHECK_SESSION_LOGS_UNREADABLE: &str = "session.logs_unreadable";

/// Check id for a telemetry invocation that could not be correlated with a
/// transcript task.
pub const CHECK_SESSION_INVOCATION_UNMATCHED: &str = "session.invocation_unmatched";

/// Check id for an exact-name tool selection entry that resolves to no known
/// built-in, toolbox, or MCP source.
pub const CHECK_TOOLS_SELECTION: &str = "tools.unknown_selection";

/// Check id for an MCP configuration problem or a skipped server definition.
pub const CHECK_MCP_CONFIG: &str = "mcp.config_invalid";

/// Check id for an MCP server that could not start or handshake.
pub const CHECK_MCP_SERVER: &str = "mcp.server_failed";

/// Check id for a `cake mcp get` name that the resolved sources do not define.
pub const CHECK_MCP_NOT_FOUND: &str = "mcp.server_not_found";

/// Overall status of a diagnostic document.
///
/// The status describes the invocation, not the subject it inspects: a
/// `bash check` document that reports a `block` verdict is still `ok`, because
/// the inspection itself completed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticStatus {
    /// The invocation completed and has nothing to report.
    Ok,
    /// The invocation completed and reported non-fatal findings.
    Warning,
    /// The invocation reported a problem and exits nonzero.
    Error,
}

impl DiagnosticStatus {
    /// A stable label for human-readable diagnostics.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// One structured finding in a diagnostic document.
///
/// `id` is a stable machine-readable identifier, `status` its severity, and
/// `message` the human-readable detail. Consumers switch on `id`; the
/// identifiers this module defines are stable for their schema version.
#[derive(Clone, Debug, Serialize)]
pub struct DiagnosticCheck {
    pub id: String,
    pub status: DiagnosticStatus,
    pub message: String,
}

impl DiagnosticCheck {
    /// A non-fatal finding: the invocation still exits successfully.
    pub fn warning(id: &str, message: impl Into<String>) -> Self {
        Self {
            id: id.to_string(),
            status: DiagnosticStatus::Warning,
            message: message.into(),
        }
    }

    /// A fatal finding: the invocation reports a problem and exits nonzero.
    pub fn error(id: &str, message: impl Into<String>) -> Self {
        Self {
            id: id.to_string(),
            status: DiagnosticStatus::Error,
            message: message.into(),
        }
    }
}

/// One versioned diagnostic document.
///
/// `summary` carries the scalar identifiers and counts a consumer scripts
/// against; `data` carries the command's payload. Both are command-specific
/// JSON objects, and `checks` is empty when there is nothing to report.
///
/// The payload is any serializable value rather than a [`Value`] so a command
/// can keep a typed payload and serialize each field at its own width.
#[derive(Debug, Serialize)]
pub struct DiagnosticDocument<D> {
    schema_version: u32,
    command: &'static str,
    status: DiagnosticStatus,
    summary: Value,
    checks: Vec<DiagnosticCheck>,
    data: D,
}

impl<D: Serialize> DiagnosticDocument<D> {
    /// Build a document and derive its status from its findings, so a command
    /// cannot report a healthy status alongside an error finding.
    pub fn new(
        command: &'static str,
        summary: Value,
        checks: Vec<DiagnosticCheck>,
        data: D,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            command,
            status: status_from_checks(&checks),
            summary,
            checks,
            data,
        }
    }

    /// Render the document as pretty-printed JSON followed by one newline.
    pub fn render(&self) -> anyhow::Result<String> {
        serde_json::to_string_pretty(self)
            .map(|document| format!("{document}\n"))
            .map_err(|e| anyhow::anyhow!("Failed to serialize {} output: {e}", self.command))
    }

    /// Write the document to stdout.
    ///
    /// A render failure is reported on stderr instead of replacing the
    /// command's own error, which stays the cause of the nonzero exit.
    pub fn print(&self) {
        match self.render() {
            Ok(document) => print!("{document}"),
            Err(error) => eprintln!("warning: {error}"),
        }
    }
}

impl DiagnosticDocument<Value> {
    /// Build the document for a failure the command cannot complete around.
    ///
    /// The command still exits nonzero; the document keeps the failure visible
    /// on stdout so a consumer that reads only machine output learns what
    /// happened instead of inferring it from empty output.
    pub fn failure(command: &'static str, check_id: &str, message: impl Into<String>) -> Self {
        Self::new(
            command,
            json!({}),
            vec![DiagnosticCheck::error(check_id, message)],
            json!({}),
        )
    }
}

/// The status implied by a document's findings: the most severe one wins.
fn status_from_checks(checks: &[DiagnosticCheck]) -> DiagnosticStatus {
    if checks
        .iter()
        .any(|check| check.status == DiagnosticStatus::Error)
    {
        DiagnosticStatus::Error
    } else if checks
        .iter()
        .any(|check| check.status == DiagnosticStatus::Warning)
    {
        DiagnosticStatus::Warning
    } else {
        DiagnosticStatus::Ok
    }
}

/// Findings for the settings loader's warnings, in loader order.
///
/// The loader's warnings are unrecognized settings keys, which is why they
/// become findings in machine mode instead of stderr text: a consumer
/// validating configuration wants them in the document.
pub fn settings_warnings(warnings: &[String]) -> Vec<DiagnosticCheck> {
    warnings
        .iter()
        .map(|warning| DiagnosticCheck::warning(CHECK_SETTINGS_UNKNOWN_KEY, warning.clone()))
        .collect()
}

/// Report a failure the command cannot complete around and return it.
///
/// Machine mode emits one document that carries the failure as an error
/// finding; either way the command exits nonzero. The finding's message matches
/// the stderr diagnostic, so both channels name the same problem.
pub fn report_failure(
    json: bool,
    command: &'static str,
    check_id: &str,
    error: anyhow::Error,
) -> anyhow::Error {
    if json {
        DiagnosticDocument::failure(command, check_id, format!("{error:#}")).print();
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_has_the_declared_top_level_fields_in_order() {
        let document = DiagnosticDocument::new(
            "debug models",
            json!({ "count": 0 }),
            Vec::new(),
            json!({ "models": [] }),
        );

        let rendered = document.render().unwrap();

        // The emitted bytes lead with the envelope, so a reader (and a diff)
        // meets the identity of the document before its payload.
        assert!(rendered.starts_with(
            "{\n  \"schema_version\": 1,\n  \"command\": \"debug models\",\n  \"status\": \"ok\",\n"
        ));
        assert!(rendered.contains("\n  \"summary\":"));
        assert!(rendered.contains("\n  \"checks\": []"));
        assert!(rendered.contains("\n  \"data\":"));
        assert!(rendered.ends_with('\n'));

        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed["schema_version"], SCHEMA_VERSION);
        assert_eq!(parsed["command"], "debug models");
        assert_eq!(parsed["status"], "ok");
    }

    #[test]
    fn status_derives_from_the_most_severe_finding() {
        let warning = || vec![DiagnosticCheck::warning("a", "a warning")];
        let error = || vec![DiagnosticCheck::error("b", "an error")];

        let ok = DiagnosticDocument::new("debug models", json!({}), Vec::new(), json!({}));
        assert_eq!(ok.status, DiagnosticStatus::Ok);

        let warned = DiagnosticDocument::new("debug models", json!({}), warning(), json!({}));
        assert_eq!(warned.status, DiagnosticStatus::Warning);

        let mut both = warning();
        both.extend(error());
        let failed = DiagnosticDocument::new("debug models", json!({}), both, json!({}));
        assert_eq!(failed.status, DiagnosticStatus::Error);
    }

    #[test]
    fn failure_document_reports_the_error_check() {
        let document = DiagnosticDocument::failure("debug models", "settings.load_failed", "boom");
        let rendered = document.render().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();

        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["checks"][0]["id"], "settings.load_failed");
        assert_eq!(parsed["checks"][0]["status"], "error");
        assert_eq!(parsed["checks"][0]["message"], "boom");
    }

    #[test]
    fn settings_warnings_become_findings_in_loader_order() {
        let warnings = vec!["first".to_string(), "second".to_string()];
        let checks = settings_warnings(&warnings);

        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].id, CHECK_SETTINGS_UNKNOWN_KEY);
        assert_eq!(checks[0].message, "first");
        assert_eq!(checks[1].message, "second");
        assert_eq!(checks[0].status, DiagnosticStatus::Warning);
    }

    #[test]
    fn rendering_is_byte_stable_across_calls() {
        let first = DiagnosticDocument::new(
            "sessions list",
            json!({ "count": 1 }),
            Vec::new(),
            json!({ "sessions": [ { "session_id": "id" } ] }),
        );
        let second = DiagnosticDocument::new(
            "sessions list",
            json!({ "count": 1 }),
            Vec::new(),
            json!({ "sessions": [ { "session_id": "id" } ] }),
        );

        assert_eq!(first.render().unwrap(), second.render().unwrap());
    }
}
