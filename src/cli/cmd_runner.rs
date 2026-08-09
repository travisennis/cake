use crate::config::DataDir;

/// Run-mode options a subcommand may need to mirror the agent run's model and
/// profile selection.
///
/// Passed to [`CmdRunner::run`] so introspection commands like
/// `cake bash check` resolve the judge model the same way an agent turn would
/// (`--model`, then `default_model` under the selected profile).
#[derive(Debug, Clone, Copy, Default)]
pub struct CommandRunOptions<'a> {
    /// The top-level `--model` flag value, when provided.
    pub model: Option<&'a str>,
    /// The top-level `--profile` flag value, when provided.
    pub profile: Option<&'a str>,
}

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
    async fn run(&self, data_dir: &DataDir, options: &CommandRunOptions<'_>) -> anyhow::Result<()>;
}
