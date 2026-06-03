//! CLI command runner interface and output/run-mode abstractions.
//!
//! This module defines:
//! - The [`CmdRunner`] trait for CLI commands.
//! - [`CliOutputSink`] for rendering responses.
//! - [`RunMode`] / [`SessionStorage`] for session lifecycle.
//! - [`RunSession`] and [`skill_locations`] for session construction.

mod cmd_runner;
mod debug;
mod output;
mod run_mode;
mod session_factory;

#[doc(inline)]
pub use cmd_runner::CmdRunner;
pub use debug::DebugCommand;
pub use output::{CliOutputSink, TurnResult};

// Re-exported for test assertions; unused in production builds.
#[cfg(test)]
pub use output::{format_done_summary, format_retry_message, format_spinner_message};
pub use run_mode::{RunMode, SessionStorage};

/// Top-level CLI subcommands.
#[derive(Clone, Debug, clap::Subcommand)]
pub enum Commands {
    /// Debug and introspection commands
    Debug(DebugCommand),
}

impl CmdRunner for Commands {
    async fn run(&self, data_dir: &crate::config::DataDir) -> anyhow::Result<()> {
        match self {
            Self::Debug(command) => command.run(data_dir).await,
        }
    }
}
