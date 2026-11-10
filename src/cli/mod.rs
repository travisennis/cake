//! CLI command runner interface and output/run-mode abstractions.
//!
//! This module defines:
//! - The [`CmdRunner`] trait for CLI commands.
//! - [`CliOutputSink`] for rendering responses.
//! - [`RunMode`] / [`SessionPersistencePlan`] for session lifecycle.
//! - [`session_factory::RunSession`] and [`session_factory::skill_locations`] for session construction.

mod bash;
mod checkpoint;
mod cmd_runner;
mod debug;
mod diagnostic;
mod gates;
mod init;
mod mcp;
mod output;
mod persistence;
mod replay;
mod run_mode;
mod sandbox_grants;
mod session_factory;
mod sessions;
mod settings;
mod trust;

#[doc(inline)]
pub use bash::BashCommand;
#[doc(inline)]
pub use checkpoint::CheckpointError;
pub use cmd_runner::{CmdRunner, CommandRunOptions};
pub use debug::{DebugCommand, SessionDiagnosticError};
pub use diagnostic::{
    CHECK_CONFIG_HOOKS, CHECK_CONFIG_MODEL, CHECK_CONFIG_PROJECT_TRUST, CHECK_GRANTS_PATH,
    CHECK_MCP_CONFIG, CHECK_MCP_NOT_FOUND, CHECK_MCP_SERVER, CHECK_SESSION_CONFIG_UNAVAILABLE,
    CHECK_SESSION_INCOMPLETE_TASK, CHECK_SESSION_INPUT, CHECK_SESSION_INVOCATION_UNMATCHED,
    CHECK_SESSION_LIVENESS_UNKNOWN, CHECK_SESSION_LOGS_UNREADABLE, CHECK_SESSION_PARTIAL_TAIL,
    CHECK_SESSION_TELEMETRY_CORRUPT, CHECK_SESSION_TELEMETRY_MISSING,
    CHECK_SESSION_TELEMETRY_UNREADABLE, CHECK_SESSION_TRANSCRIPT_CORRUPT,
    CHECK_SESSION_TRANSCRIPT_UNREADABLE, CHECK_SESSIONS_CHECKPOINT, CHECK_SESSIONS_DIRECTORY,
    CHECK_SESSIONS_DISCOVERY, CHECK_SETTINGS_LOAD_FAILED, CHECK_SKILLS_DISCOVERY,
    CHECK_SKILLS_PROJECT_TRUST, CHECK_TOOLS_SELECTION, DiagnosticCheck, DiagnosticDocument,
    report_failure, settings_warnings,
};
pub use gates::GatesCommand;
pub use init::{InitCommand, InitError};
#[doc(inline)]
pub use mcp::McpCommand;
pub use output::{CliOutputSink, TurnResult};
pub use persistence::execute_persistence_plan;
pub use replay::{ReplayCommand, ReplayError};
pub use sessions::SessionsCommand;
pub use trust::TrustCommand;

pub use run_mode::{RunMode, SessionPersistencePlan};
pub use sandbox_grants::{SandboxGrantError, prepare_user_skills_dir, prepare_writable_dirs};

/// A root agent-run option was supplied alongside a subcommand.
#[derive(Debug, thiserror::Error)]
#[error(
    "invalid usage: {options} cannot be used with subcommand '{command}'; these are agent-run options and require no subcommand"
)]
pub struct InvalidSubcommandOptions {
    command: &'static str,
    options: String,
}

impl InvalidSubcommandOptions {
    /// Build an input error with the rejected options in CLI order.
    pub(crate) fn new(command: &'static str, options: &[&'static str]) -> Self {
        Self {
            command,
            options: options.join(", "),
        }
    }
}

/// Top-level CLI subcommands.
#[derive(Clone, Debug, clap::Subcommand)]
pub enum Commands {
    /// Debug and introspection commands
    Debug(DebugCommand),
    /// Session browsing commands
    Sessions(SessionsCommand),
    /// Inspect and explain Bash command-safety decisions
    Bash(BashCommand),
    /// Inspect and validate configured policy gates
    Gates(GatesCommand),
    /// Create `.cake/` project scaffolding and a behavior-preserving settings file
    Init(InitCommand),
    /// Manage configured MCP server definitions
    Mcp(McpCommand),
    /// Replay an existing session transcript as stream-json events
    Replay(ReplayCommand),
    /// Review, inspect, and revoke project configuration trust
    Trust(TrustCommand),
}

impl CmdRunner for Commands {
    async fn run(
        &self,
        data_dir: &crate::config::DataDir,
        options: &CommandRunOptions<'_>,
    ) -> anyhow::Result<()> {
        self.dispatch(data_dir, options).await
    }
}

impl Commands {
    /// Return the command name used in user-facing validation errors.
    pub(crate) const fn name(&self) -> &'static str {
        match self {
            Self::Debug(_) => "debug",
            Self::Sessions(_) => "sessions",
            Self::Bash(_) => "bash",
            Self::Gates(_) => "gates",
            Self::Init(_) => "init",
            Self::Mcp(_) => "mcp",
            Self::Replay(_) => "replay",
            Self::Trust(_) => "trust",
        }
    }

    /// The project-configuration surface this command uses.
    pub(crate) const fn project_config_use(&self) -> crate::config::trust::ProjectConfigUse {
        use crate::config::trust::ProjectConfigUse;
        match self {
            Self::Debug(_) | Self::Mcp(_) => ProjectConfigUse::Inspect,
            Self::Bash(_) => ProjectConfigUse::Enforce,
            Self::Gates(command) => command.project_config_use(),
            Self::Sessions(_) | Self::Init(_) | Self::Replay(_) | Self::Trust(_) => {
                ProjectConfigUse::Unused
            },
        }
    }

    /// Whether the command refuses the one-run project-trust flags.
    pub(crate) const fn rejects_project_trust_flags(&self) -> bool {
        matches!(
            self,
            Self::Sessions(_) | Self::Init(_) | Self::Replay(_) | Self::Trust(_)
        )
    }

    /// Dispatch to the selected subcommand's [`CmdRunner`].
    ///
    /// Extracted so [`CmdRunner::run`] stays at baseline complexity as
    /// subcommands are added.
    async fn dispatch(
        &self,
        data_dir: &crate::config::DataDir,
        options: &CommandRunOptions<'_>,
    ) -> anyhow::Result<()> {
        match self {
            Self::Debug(command) => command.run(data_dir, options).await,
            Self::Sessions(command) => command.run(data_dir, options).await,
            Self::Bash(command) => command.run(data_dir, options).await,
            Self::Gates(command) => command.run(data_dir, options).await,
            Self::Init(command) => command.run(data_dir, options).await,
            Self::Mcp(command) => command.run(data_dir, options).await,
            Self::Replay(command) => command.run(data_dir, options).await,
            Self::Trust(command) => command.run(data_dir, options).await,
        }
    }
}
