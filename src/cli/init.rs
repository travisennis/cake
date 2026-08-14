//! `cake init` subcommand: create explicit, reviewable project scaffolding.
//!
//! `cake init` creates `.cake/` and a commented, behavior-preserving
//! `.cake/settings.toml`. `cake init --hooks` also creates an inert
//! `.cake/hooks.json.example` trusted-hook reference. Version 1 has no force or
//! merge mode: if any planned target already exists, nothing is written and the
//! command fails, so re-running after a successful initialization is a safe
//! no-op refusal.

use std::fs;
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
    Conflict(Vec<String>),
}

impl std::fmt::Display for InitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self::Conflict(targets) = self;
        let listed = targets.join(", ");
        if targets.len() == 1 {
            write!(f, "refusing to initialize: {listed} already exists")
        } else {
            write!(f, "refusing to initialize: {listed} already exist")
        }
    }
}

/// Create `.cake/` scaffolding in `project_dir`.
///
/// With `with_hooks`, also create the inert hooks example. Every planned target
/// is checked before anything is written: if any target already exists, the
/// call returns [`InitError::Conflict`] and leaves every existing and planned
/// target unchanged.
pub fn initialize(project_dir: &Path, with_hooks: bool) -> anyhow::Result<InitOutcome> {
    const SETTINGS_TARGET: &str = ".cake/settings.toml";
    const HOOKS_TARGET: &str = ".cake/hooks.json.example";

    let settings_path = project_dir.join(SETTINGS_TARGET);
    let hooks_path = with_hooks.then(|| project_dir.join(HOOKS_TARGET));

    let mut conflicts = Vec::new();
    if settings_path.exists() {
        conflicts.push(SETTINGS_TARGET.to_string());
    }
    if let Some(path) = &hooks_path
        && path.exists()
    {
        conflicts.push(HOOKS_TARGET.to_string());
    }
    if !conflicts.is_empty() {
        return Err(InitError::Conflict(conflicts).into());
    }

    fs::create_dir_all(project_dir.join(".cake"))
        .with_context(|| format!("failed to create {}", project_dir.join(".cake").display()))?;
    fs::write(&settings_path, DEFAULT_SETTINGS)
        .with_context(|| format!("failed to write {}", settings_path.display()))?;
    if let Some(path) = &hooks_path {
        fs::write(path, HOOKS_EXAMPLE)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }

    Ok(InitOutcome {
        settings: SETTINGS_TARGET.to_string(),
        hooks_example: with_hooks.then(|| HOOKS_TARGET.to_string()),
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
