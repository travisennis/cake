//! `cake init` subcommand: create explicit, reviewable project scaffolding.
//!
//! `cake init` creates `.cake/` and a commented, behavior-preserving
//! `.cake/settings.toml`. `cake init --hooks` also creates an inert
//! `.cake/hooks.json.example` trusted-hook reference. Version 1 has no force or
//! merge mode: if any planned target already exists, nothing is written and the
//! command fails, so re-running after a successful initialization is a safe
//! no-op refusal.

use std::fs;
use std::io;
use std::path::Path;

use anyhow::Context;
use clap::Parser;
use thiserror::Error;

use crate::cli::{CmdRunner, CommandRunOptions};
use crate::config::DataDir;

/// Create `.cake/` project scaffolding and a behavior-preserving settings file.
#[derive(Clone, Debug, Parser)]
pub struct InitCommand {
    /// Also create an inert `.cake/hooks.json.example` trusted-hook reference
    #[arg(long)]
    hooks: bool,
}

impl CmdRunner for InitCommand {
    async fn run(
        &self,
        _data_dir: &DataDir,
        _options: &CommandRunOptions<'_>,
    ) -> anyhow::Result<()> {
        let current_dir = std::env::current_dir()
            .map_err(|e| anyhow::anyhow!("Failed to get current directory: {e}"))?;
        let outcome = initialize(&current_dir, self.hooks)?;
        print!("{outcome}");
        Ok(())
    }
}

/// Files `cake init` planned to create for one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitOutcome {
    /// Display path of the created settings file (for example `.cake/settings.toml`).
    pub settings: String,
    /// Display path of the created hooks example, when `--hooks` was requested.
    pub hooks_example: Option<String>,
}

impl std::fmt::Display for InitOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Created {}", self.settings)?;
        if let Some(hooks) = &self.hooks_example {
            writeln!(f, "Created {hooks}")?;
        }
        Ok(())
    }
}

/// Failure modes for [`initialize`].
#[derive(Debug, Error)]
pub enum InitError {
    /// One or more planned targets already exist; nothing was written.
    #[error("refusing to initialize: {0}")]
    Conflict(String),
    /// Writing a planned target failed.
    #[error("failed to write {path}: {source}")]
    Io {
        /// Display path of the target that could not be written.
        path: String,
        /// Underlying filesystem error.
        source: std::io::Error,
    },
}

/// Render the conflict message with singular or plural verb agreement.
fn conflict_message(targets: &[String]) -> String {
    let listed = targets.join(", ");
    if targets.len() == 1 {
        format!("{listed} already exists")
    } else {
        format!("{listed} already exist")
    }
}

/// Create `.cake/` scaffolding in `project_dir`.
///
/// With `with_hooks`, also create the inert hooks example. Every planned target
/// is checked before anything is written: if any target already exists, or is
/// occupied by a symlink, the call returns [`InitError::Conflict`] and leaves
/// every existing and planned target unchanged. Targets are created
/// exclusively (never truncating a file that appeared after the check) and
/// `.cake` itself must be a real directory, not a file or a symlink that would
/// redirect writes outside the project.
pub fn initialize(project_dir: &Path, with_hooks: bool) -> anyhow::Result<InitOutcome> {
    const SETTINGS_TARGET: &str = ".cake/settings.toml";
    const HOOKS_TARGET: &str = ".cake/hooks.json.example";

    let dot_cake = project_dir.join(".cake");
    let settings_path = project_dir.join(SETTINGS_TARGET);
    let hooks_path = with_hooks.then(|| project_dir.join(HOOKS_TARGET));

    let mut conflicts = Vec::new();
    if !dot_cake_is_plain_directory(&dot_cake) {
        conflicts.push(".cake".to_string());
    }
    if path_occupied(&settings_path) {
        conflicts.push(SETTINGS_TARGET.to_string());
    }
    if let Some(path) = &hooks_path
        && path_occupied(path)
    {
        conflicts.push(HOOKS_TARGET.to_string());
    }
    if !conflicts.is_empty() {
        return Err(InitError::Conflict(conflict_message(&conflicts)).into());
    }

    fs::create_dir_all(&dot_cake)
        .with_context(|| format!("failed to create {}", dot_cake.display()))?;
    write_new(&settings_path, SETTINGS_TARGET, DEFAULT_SETTINGS)?;
    if let Some(path) = &hooks_path {
        write_new(path, HOOKS_TARGET, HOOKS_EXAMPLE)?;
    }

    Ok(InitOutcome {
        settings: SETTINGS_TARGET.to_string(),
        hooks_example: with_hooks.then(|| HOOKS_TARGET.to_string()),
    })
}

/// True when `.cake` is absent (safe to create) or is a real directory.
///
/// A file or symlink at `.cake` is a conflict: `create_dir_all` would fail
/// confusingly on a file, and following a symlink would write outside the
/// project directory.
fn dot_cake_is_plain_directory(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(metadata) => metadata.is_dir() && !metadata.file_type().is_symlink(),
        Err(error) => error.kind() == io::ErrorKind::NotFound,
    }
}

/// True when `path` is occupied by any filesystem entry, including a dangling
/// symlink.
///
/// `Path::exists()` follows symlinks and returns `false` for a dangling one,
/// but the path is still occupied: exclusive creation would refuse it. The
/// preflight must see it so a conflict surfaces before anything is written.
fn path_occupied(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(error) => error.kind() != io::ErrorKind::NotFound,
    }
}

/// Write a target file with exclusive creation.
///
/// `create_new` never truncates a file that appeared after the conflict check
/// and never follows a symlink at the target path. An `AlreadyExists` result is
/// the race window for that check and is reported as a conflict.
fn write_new(path: &Path, display: &str, content: &str) -> Result<(), InitError> {
    use std::io::Write;

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| match source.kind() {
            io::ErrorKind::AlreadyExists => {
                InitError::Conflict(format!("{display} already exists"))
            },
            _ => InitError::Io {
                path: display.to_string(),
                source,
            },
        })?;
    file.write_all(content.as_bytes())
        .map_err(|source| InitError::Io {
            path: display.to_string(),
            source,
        })
}

/// Generated `.cake/settings.toml` content.
///
/// Everything is commented out so the file is valid TOML and changes no
/// behavior. It references the current `[tools.bash.judge]` vocabulary without
/// disabling the judge, adding allowlist entries, selecting a model, or adding
/// rubric guidance, and it does not claim any model or timeout is
/// reliability-qualified (the judge benchmark owns that evidence).
const DEFAULT_SETTINGS: &str = r#"# Cake project settings, created by `cake init`.
#
# Cake loads project settings from `.cake/settings.toml` and overlays them on
# your global settings at `~/.config/cake/settings.toml`. Everything below is
# commented out, so this file changes no behavior: it is a reference for the
# keys Cake supports. Uncomment the keys you need.
#
# The full contract is in docs/configuration.md. `cake --help` is the authority
# for CLI flags.

# The model used when `--model` is not given. The value is the `name` of a
# `[[models]]` entry, defined below or in your global settings.
# default_model = "my-model"

# Named models. Each model needs a unique `name` (lowercase letters, numbers,
# and hyphens), a provider model identifier, a base URL, and the environment
# variable that holds its API key.
# [[models]]
# name = "my-model"
# model = "provider/model-id"
# base_url = "https://api.example.com/v1"
# api_key_env = "MY_MODEL_API_KEY"
# api_type = "chat_completions"

# The LLM command-safety judge evaluates every Bash command before it runs and
# is the command-safety gate above the OS sandbox. It is default-on and
# fail-closed: an unavailable judge blocks the command instead of running it
# ungated. The keys below are commented out so this file preserves Cake's
# defaults. A `model` value is a `[[models]]` name, the same vocabulary as
# `default_model` and `--model`. See docs/configuration.md and ADR-018 for the
# full judge contract.
# [tools.bash.judge]
# model = "my-model"             # optional: a [[models]] name for the judge
# timeout_secs = 30              # default 30; never below 1
# retry_budget_secs = 15         # default 15; 0 disables recovery
# rubric_file = ".cake/judge-rubric.md"  # optional extra judge guidance
# enabled = true                 # default true; false is the emergency bypass
# allowlist = []                 # exact raw commands whose block is overridden

# Optional agent-loop limits bound how long a run may continue. Every key is
# off by default: an uncapped loop is deliberate, and no limit fires unless
# you configure one. Turns and tool calls are independent resource boundaries.
# See docs/configuration.md for the full contract.
# [limits]
# max_turns = 10        # stop after 10 agent-loop turns
# max_tool_calls = 50   # stop after 50 executed tool calls
"#;

/// Generated inert `.cake/hooks.json.example` content.
///
/// Cake never loads `.example` files, so this reference is inert. The
/// `_comment` field is ignored by Cake's parser; it exists so the file explains
/// the trust boundary by itself. A user who copies the file to `hooks.json` or
/// `hooks.local.json` activates trusted commands.
const HOOKS_EXAMPLE: &str = r#"{
  "_comment": "Cake hook reference, created by `cake init --hooks`. Cake never loads this .example file. To activate hooks, copy it to .cake/hooks.json (shared project hooks) or .cake/hooks.local.json (personal hooks). Hooks are trusted local commands that run outside the model tool sandbox with your full host authority: enable only hook files you trust. See docs/configuration.md and docs/security.md.",
  "version": 1,
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "echo \"hello from a cake hook\"",
            "timeout": 10,
            "fail_closed": false,
            "status_message": "Ran a session-start hook"
          }
        ]
      }
    ]
  }
}
"#;

#[cfg(test)]
#[path = "init_tests.rs"]
mod tests;
