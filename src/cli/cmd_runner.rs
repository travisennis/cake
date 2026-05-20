use crate::config::DataDir;

/// A trait representing a command runner.
///
/// This trait defines the interface for commands that can be executed by the CLI.
/// Implementations handle command-specific logic, service interactions, and
/// necessary actions based on the command's purpose.
///
/// # Examples
///
/// Implementors define the `run` method which receives a reference to the
/// [`DataDir`](crate::config::DataDir) and returns `anyhow::Result<()>`.
pub trait CmdRunner {
    /// Executes the command's logic.
    ///
    /// # Errors
    ///
    /// Returns an error if the command execution fails.
    async fn run(&self, data_dir: &DataDir) -> anyhow::Result<()>;
}
