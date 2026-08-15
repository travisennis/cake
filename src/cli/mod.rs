//! CLI command runner interface and output/run-mode abstractions.
//!
//! This module defines:
//! - The [`CmdRunner`] trait for CLI commands.
//! - [`CliOutputSink`] for rendering responses.
//! - [`RunMode`] / [`SessionStorage`] for session lifecycle.
//! - [`RunSession`] and [`crate::cli::session_factory::skill_locations`] for session construction.

mod bash;
mod cmd_runner;
mod debug;
mod init;
mod output;
mod replay;
mod run_mode;
mod session_factory;
mod sessions;

#[doc(inline)]
pub use bash::BashCommand;
pub use cmd_runner::{CmdRunner, CommandRunOptions};
pub use debug::DebugCommand;
pub use init::{InitCommand, InitError};
pub use output::{CliOutputSink, TurnResult};
pub use replay::{ReplayCommand, ReplayError};
pub use sessions::SessionsCommand;

pub use run_mode::{RunMode, SessionStorage};
pub use session_factory::RunSession;

/// Top-level CLI subcommands.
#[derive(Clone, Debug, clap::Subcommand)]
pub enum Commands {
    /// Debug and introspection commands
    Debug(DebugCommand),
    /// Session browsing commands
    Sessions(SessionsCommand),
    /// Inspect and explain Bash command-safety decisions
    Bash(BashCommand),
    /// Create `.cake/` project scaffolding and a behavior-preserving settings file
    Init(InitCommand),
    /// Replay an existing session transcript as stream-json events
    Replay(ReplayCommand),
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
            Self::Init(command) => command.run(data_dir, options).await,
            Self::Replay(command) => command.run(data_dir, options).await,
        }
    }
}
