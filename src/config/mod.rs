//! Configuration management for cake.
//!
//! This module provides configuration loading, session management, and data
//! directory handling for the cake CLI. Configuration is loaded from TOML files
//! and can be overridden via command-line arguments.
//!
//! # Key Types
//!
//! - [`DataDir`] - Manages the data directory for session storage
//! - [`Session`] - Represents a conversation session
//! - [`ModelConfig`] - Model provider configuration
//! - [`SettingsLoader`] - Loads settings from TOML files

mod agents;
mod config_dir;
mod data_dir;
pub mod git;
pub mod hooks;
pub mod model;
pub mod output_schema;
pub mod session;
pub mod session_jsonl;
pub mod settings;
pub mod skills;
pub mod toolbox;
pub mod worktree;

use std::path::PathBuf;

#[doc(inline)]
pub use agents::{AgentsFile, read_agents_files};
#[doc(inline)]
pub use config_dir::config_dir;
#[doc(inline)]
pub use data_dir::DataDir;
#[doc(inline)]
pub use hooks::{HookSource, HooksLoader};
#[doc(inline)]
pub use model::{ModelConfig, ReasoningEffort, ResolvedModelConfig};
#[doc(inline)]
pub use output_schema::{OutputSchema, OutputSchemaError};
#[doc(inline)]
pub use session::{Session, SessionWriter};
#[doc(inline)]
pub use settings::{ModelDefinition, SettingsLoader};
#[doc(inline)]
pub use skills::{
    DiagnosticLevel, SkillCatalog, discover_skills, discover_skills_with_paths,
    parse_skill_path_list,
};

/// Expand a leading `~` (or a bare `~`) to the user's home directory.
///
/// Returns the path unchanged when it is not a home-relative path, when the
/// home directory cannot be determined, or when the path is not valid UTF-8.
pub fn expand_home(path: PathBuf) -> PathBuf {
    let Some(path_str) = path.to_str() else {
        return path;
    };

    if path_str == "~" {
        if let Some(home_dir) = dirs::home_dir() {
            return home_dir;
        }
        return path;
    }

    if let Some(rest) = path_str
        .strip_prefix("~/")
        .or_else(|| path_str.strip_prefix("~\\"))
        && let Some(home_dir) = dirs::home_dir()
    {
        return home_dir.join(rest);
    }

    path
}
