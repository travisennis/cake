//! AGENTS.md instruction discovery.
//!
//! User-level, config-level, and project-level agent instruction files are
//! discovered here. Resolution is independent of [`super::DataDir`]: a
//! `CAKE_DATA_DIR` override relocates session and cache storage only, and
//! never the instruction roots. User and config roots are derived from
//! `$HOME`/`dirs::home_dir()` and [`super::config_dir()`] respectively.

use std::{fs, path::Path};

/// Represents an AGENTS.md file with its path and content.
///
/// AGENTS.md files contain instructions for the AI agent about project-specific
/// context and behavior. They are loaded from user-level (`~/.cake/AGENTS.md`),
/// XDG config (`~/.config/AGENTS.md`), and project-level (`./AGENTS.md`) locations.
#[derive(Debug, Clone)]
pub struct AgentsFile {
    /// Display path (e.g., "~/.cake/AGENTS.md" or "./AGENTS.md")
    pub path: String,
    /// Content of the file
    pub content: String,
}

/// Reads AGENTS.md files from user-level, config-level, and project-level locations.
///
/// Returns a list of found AGENTS.md files with their paths and content.
/// Files that don't exist are silently skipped. Resolution never consults
/// `CAKE_DATA_DIR`; user and config roots come from the home and XDG config
/// directories, so relocating data storage cannot repoint or suppress
/// instructions.
///
/// The search order is:
/// 1. User-level: `~/.cake/AGENTS.md`
/// 2. XDG config: `~/.config/AGENTS.md` (or `$XDG_CONFIG_HOME/AGENTS.md`)
/// 3. Project-level: `./AGENTS.md`
///
/// # Examples
///
/// ```ignore
/// let agents_files = read_agents_files(Path::new("/project"));
/// for file in &agents_files {
///     println!("Found AGENTS.md at: {}", file.path);
/// }
/// ```
pub fn read_agents_files(working_dir: &Path) -> Vec<AgentsFile> {
    let mut files = Vec::new();

    // User-level AGENTS.md: ~/.cake/AGENTS.md
    if let Some(home) = dirs::home_dir() {
        let user_agents_path = home.join(".cake").join("AGENTS.md");
        if let Ok(content) = fs::read_to_string(&user_agents_path) {
            files.push(AgentsFile {
                path: "~/.cake/AGENTS.md".to_string(),
                content,
            });
        }
    }

    // XDG config AGENTS.md: ~/.config/AGENTS.md (or $XDG_CONFIG_HOME/AGENTS.md)
    let xdg_agents_path = super::config_dir().join("AGENTS.md");
    if let Ok(content) = fs::read_to_string(&xdg_agents_path) {
        let display_path = if std::env::var("XDG_CONFIG_HOME").is_ok_and(|d| !d.is_empty()) {
            "$XDG_CONFIG_HOME/AGENTS.md".to_string()
        } else {
            "~/.config/AGENTS.md".to_string()
        };
        files.push(AgentsFile {
            path: display_path,
            content,
        });
    }

    // Project-level AGENTS.md: ./AGENTS.md
    let project_agents_path = working_dir.join("AGENTS.md");
    if let Ok(content) = fs::read_to_string(&project_agents_path) {
        files.push(AgentsFile {
            path: "./AGENTS.md".to_string(),
            content,
        });
    }

    files
}
