//! Session run-mode abstraction.
//!
//! Defines [`RunMode`], which translates the CLI flags (`--continue`, `--resume`,
//! `--fork`, `--no-session`) into an enum controlling session lifecycle, and
//! [`SessionStorage`] for whether a session file is created fresh or appended to.

use crate::session_telemetry::SessionTelemetryRunMode;

/// How a session should be started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunMode {
    /// Start a brand-new session (default).
    NewSession,
    /// Run without persisting a session (`--no-session`).
    Ephemeral,
    /// Continue the most recent session for the current directory (`--continue`).
    ContinueLatest,
    /// Resume a specific session by UUID (`--resume <UUID>`).
    Resume { session_id: uuid::Uuid },
    /// Fork the most recent session (`--fork` without a value).
    ForkLatest,
    /// Fork a specific session by UUID (`--fork <UUID>`).
    Fork { session_id: uuid::Uuid },
}

impl RunMode {
    /// Derive the run mode from parsed CLI arguments.
    pub(crate) fn from_cli(args: &crate::CodingAssistant) -> anyhow::Result<Self> {
        if args.no_session {
            return Ok(Self::Ephemeral);
        }
        if args.continue_session {
            return Ok(Self::ContinueLatest);
        }
        if let Some(session_id) = args.resume.as_deref() {
            let id = uuid::Uuid::parse_str(session_id).map_err(|_e| {
                anyhow::anyhow!(
                    "Invalid session UUID '{session_id}'. Expected format: e.g., 550e8400-e29b-41d4-a716-446655440000"
                )
            })?;
            return Ok(Self::Resume { session_id: id });
        }
        if let Some(fork_id) = args.fork.as_deref() {
            if fork_id.is_empty() {
                return Ok(Self::ForkLatest);
            }
            let id = uuid::Uuid::parse_str(fork_id).map_err(|_e| {
                anyhow::anyhow!(
                    "Invalid session UUID '{fork_id}'. Expected format: e.g., 550e8400-e29b-41d4-a716-446655440000"
                )
            })?;
            return Ok(Self::Fork { session_id: id });
        }

        Ok(Self::NewSession)
    }

    /// Whether the session should be persisted to disk.
    pub(crate) const fn persists_session(&self) -> bool {
        !matches!(self, Self::Ephemeral)
    }

    /// A human-readable label for the hook source.
    pub(crate) const fn session_start_source(&self) -> &'static str {
        match self {
            Self::ForkLatest | Self::Fork { .. } => "fork",
            Self::ContinueLatest | Self::Resume { .. } => "resume",
            Self::NewSession | Self::Ephemeral => "startup",
        }
    }

    /// Map this mode to a telemetry run-mode label.
    pub(crate) const fn telemetry_mode(&self) -> SessionTelemetryRunMode {
        match self {
            Self::NewSession | Self::Ephemeral => SessionTelemetryRunMode::New,
            Self::ContinueLatest => SessionTelemetryRunMode::Continue,
            Self::Resume { .. } => SessionTelemetryRunMode::Resume,
            Self::ForkLatest | Self::Fork { .. } => SessionTelemetryRunMode::Fork,
        }
    }
}

/// Whether to open a fresh session file or append to an existing one.
#[derive(Clone, Copy)]
pub enum SessionStorage {
    /// Create a new session file.
    New,
    /// Append to an existing session file.
    Append,
}
