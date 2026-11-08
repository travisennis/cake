//! Shared settings loading for CLI inspection commands.

use crate::cli::{CHECK_SETTINGS_LOAD_FAILED, CommandRunOptions, report_failure};
use crate::config::SettingsLoader;
use crate::config::settings::SettingsLoad;

/// Load settings and project trust for a read-only inspection command.
///
/// Machine mode carries a settings failure in the command's diagnostic
/// document; text mode keeps it on stderr. Either way the command exits
/// nonzero.
pub fn load_settings(
    command: &'static str,
    json_output: bool,
    options: &CommandRunOptions<'_>,
) -> anyhow::Result<SettingsLoad> {
    let current_dir = std::env::current_dir()
        .map_err(|error| anyhow::anyhow!("Failed to get current directory: {error}"))?;
    SettingsLoader::load_with_project_config(
        &current_dir,
        options.profile,
        options.no_gate,
        &crate::config::config_dir(),
        options.project_config_mode,
    )
    .map_err(|error| {
        report_failure(
            json_output,
            command,
            CHECK_SETTINGS_LOAD_FAILED,
            error.into(),
        )
    })
}
