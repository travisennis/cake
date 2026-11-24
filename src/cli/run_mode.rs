//! Session run-mode and persistence-plan abstractions.
//!
//! Defines [`RunMode`], which translates the CLI flags (`--resume`, `--fork`,
//! `--no-session`) into an enum controlling session lifecycle, and
//! [`SessionPersistencePlan`] for creating or appending a session file.

use crate::session_telemetry::SessionTelemetryRunMode;
use crate::types::SessionRecord;

/// How a session should be started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunMode {
    /// Start a brand-new session (default).
    NewSession,
    /// Run without persisting a session (`--no-session`).
    Ephemeral,
    /// Resume a specific session by UUID (`--resume <UUID>`).
    Resume {
        session_id: uuid::Uuid,
        read_only: bool,
    },
    /// Fork the most recent session (`--fork` without a value).
    ForkLatest,
    /// Fork a specific session by UUID (`--fork <UUID>`).
    Fork { session_id: uuid::Uuid },
}

impl RunMode {
    /// Derive the run mode from parsed CLI arguments.
    pub(crate) fn from_cli(args: &crate::CodingAssistant) -> anyhow::Result<Self> {
        let mode = Self::from_session_flags(args)?;
        if args.read_only
            && !matches!(
                mode,
                Self::Resume {
                    read_only: true,
                    ..
                }
            )
        {
            anyhow::bail!("Invalid read-only run: --read-only requires --resume");
        }
        if args.allow_session_model_change
            && !matches!(
                mode,
                Self::Resume { .. } | Self::ForkLatest | Self::Fork { .. }
            )
        {
            anyhow::bail!(
                "Invalid session model change override: --allow-session-model-change requires --resume or --fork"
            );
        }
        if args.allow_session_cwd_change && !matches!(mode, Self::Resume { .. }) {
            anyhow::bail!(
                "Invalid session working directory change: --allow-session-cwd-change requires --resume"
            );
        }
        Self::validate_strict_flag(args, &mode)?;
        Self::validate_reuse_flag(args, &mode)?;
        Ok(mode)
    }

    fn validate_refresh_flag(args: &crate::CodingAssistant, mode: &Self) -> anyhow::Result<()> {
        if !args.refresh_context {
            return Ok(());
        }
        if !matches!(
            mode,
            Self::Resume { .. } | Self::ForkLatest | Self::Fork { .. }
        ) {
            anyhow::bail!("--refresh-context requires --resume or --fork");
        }
        if args.reuse_last_request {
            anyhow::bail!(
                "--refresh-context cannot be combined with --reuse-last-request; reuse preserves the captured context"
            );
        }
        Ok(())
    }

    fn validate_strict_flag(args: &crate::CodingAssistant, mode: &Self) -> anyhow::Result<()> {
        Self::validate_refresh_flag(args, mode)?;
        if !args.strict_context_drift {
            return Ok(());
        }
        if !matches!(mode, Self::Resume { .. }) {
            anyhow::bail!("Invalid context drift check: --strict-context-drift requires --resume");
        }
        if args.reuse_last_request {
            anyhow::bail!(
                "Invalid context drift check: --strict-context-drift cannot be combined with --reuse-last-request; reuse preserves the captured context"
            );
        }
        Ok(())
    }

    fn validate_reuse_flag(args: &crate::CodingAssistant, mode: &Self) -> anyhow::Result<()> {
        if !args.reuse_last_request {
            return Ok(());
        }
        if !matches!(mode, Self::Resume { .. }) {
            anyhow::bail!("Invalid request reuse: --reuse-last-request requires --resume");
        }
        if args.tool_choice.is_some() {
            anyhow::bail!(
                "Invalid request reuse: --reuse-last-request does not accept --tool-choice; the captured request's tool choice is preserved"
            );
        }
        Ok(())
    }

    fn from_session_flags(args: &crate::CodingAssistant) -> anyhow::Result<Self> {
        if args.no_session {
            return Ok(Self::Ephemeral);
        }
        if let Some(session_id) = args.resume.as_deref() {
            let id = uuid::Uuid::parse_str(session_id).map_err(|_e| {
                anyhow::anyhow!(
                    "Invalid session UUID '{session_id}'. Expected format: e.g., 550e8400-e29b-41d4-a716-446655440000"
                )
            })?;
            return Ok(Self::Resume {
                session_id: id,
                read_only: args.read_only || args.reuse_last_request,
            });
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
        !matches!(
            self,
            Self::Ephemeral
                | Self::Resume {
                    read_only: true,
                    ..
                }
        )
    }

    /// Whether this run leaves no session, telemetry, or hook trace.
    pub(crate) const fn is_read_only(&self) -> bool {
        matches!(
            self,
            Self::Resume {
                read_only: true,
                ..
            }
        )
    }

    /// A human-readable label for the hook source.
    pub(crate) const fn session_start_source(&self) -> &'static str {
        match self {
            Self::ForkLatest | Self::Fork { .. } => "fork",
            Self::Resume { .. } => "resume",
            Self::NewSession | Self::Ephemeral => "startup",
        }
    }

    /// Map this mode to a telemetry run-mode label.
    pub(crate) const fn telemetry_mode(&self) -> SessionTelemetryRunMode {
        match self {
            Self::NewSession | Self::Ephemeral => SessionTelemetryRunMode::New,
            Self::Resume { .. } => SessionTelemetryRunMode::Resume,
            Self::ForkLatest | Self::Fork { .. } => SessionTelemetryRunMode::Fork,
        }
    }
}

/// One complete plan for opening a session's persistence handle.
#[derive(Debug)]
pub enum SessionPersistencePlan {
    /// Create a new session file and write these records after its metadata.
    Create {
        seed_records: Vec<SessionRecord>,
        /// Parent session whose payload blobs are hard-linked into the child
        /// before the seed records are written. A plain create has none.
        blob_source: Option<uuid::Uuid>,
    },
    /// Append to an existing session file.
    Append,
}
