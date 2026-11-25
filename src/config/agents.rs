//! AGENTS.md instruction discovery.
//!
//! User-level, config-level, and project-level agent instruction files are
//! discovered here. Resolution is independent of [`super::DataDir`]: a
//! `CAKE_DATA_DIR` override relocates session and cache storage only, and
//! never the instruction roots. User and config roots are derived from
//! `$HOME`/`dirs::home_dir()` and [`super::config_dir()`] respectively.

use std::{
    fs,
    path::{Path, PathBuf},
};

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

/// An `AGENTS.md` file discovered from a tool-accessed directory.
///
/// Scoped files are separate from the startup [`AgentsFile`] set: they are
/// discovered per invocation from the paths a successful tool call enters, and
/// carry a canonical path so the agent loop can inject each file once per task.
#[derive(Debug, Clone)]
pub struct ScopedAgentsFile {
    /// Canonical path used for per-invocation deduplication.
    pub canonical_path: PathBuf,
    /// Display path relative to the invocation working directory.
    pub path: String,
    /// Content of the file.
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

    if let Some(home) = dirs::home_dir() {
        let user_agents_path = home.join(".cake").join("AGENTS.md");
        if let Ok(content) = fs::read_to_string(&user_agents_path) {
            files.push(AgentsFile {
                path: "~/.cake/AGENTS.md".to_string(),
                content,
            });
        }
    }

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

    let project_agents_path = working_dir.join("AGENTS.md");
    if let Ok(content) = fs::read_to_string(&project_agents_path) {
        files.push(AgentsFile {
            path: "./AGENTS.md".to_string(),
            content,
        });
    }

    files
}

/// Reads `AGENTS.md` files from the directories a successful tool call entered.
///
/// `touched_path` is the canonical tool target for `Read`, `Edit`, and `Write`,
/// or the resolved working directory for `Bash`. The search starts at that
/// directory (or the target's parent for a file), walks parent-first toward
/// `working_dir`, and stops before the workspace root because its `AGENTS.md`
/// is already part of the startup prompt.
///
/// Candidates are canonicalized and must remain inside the workspace, so a
/// scoped file cannot follow a symlink outside it. Missing, unreadable, and
/// non-file candidates are skipped. The workspace-root file is also skipped
/// when a scoped path is a symlink to it.
pub fn read_scoped_agents_files(working_dir: &Path, touched_path: &Path) -> Vec<ScopedAgentsFile> {
    let Ok(workspace) = working_dir.canonicalize() else {
        return Vec::new();
    };
    let Ok(touched) = touched_path.canonicalize() else {
        return Vec::new();
    };
    let Some(start) = scoped_start_directory(&workspace, &touched) else {
        return Vec::new();
    };
    let root_agents = workspace.join("AGENTS.md").canonicalize().ok();

    scoped_directories(&workspace, &start)
        .into_iter()
        .filter_map(|directory| {
            read_scoped_agents_file(&workspace, &directory, root_agents.as_deref())
        })
        .collect()
}

/// Resolve the first directory whose scoped instructions apply to a target.
fn scoped_start_directory(workspace: &Path, touched: &Path) -> Option<PathBuf> {
    if !touched.starts_with(workspace) {
        return None;
    }
    if touched.is_dir() {
        return Some(touched.to_path_buf());
    }
    let parent = touched.parent()?;
    parent.starts_with(workspace).then(|| parent.to_path_buf())
}

/// List the scoped directories from `start` to the workspace root, exclusive.
fn scoped_directories(workspace: &Path, start: &Path) -> Vec<PathBuf> {
    let mut directories = Vec::new();
    let mut current = start.to_path_buf();
    while current != workspace && current.starts_with(workspace) {
        directories.push(current.clone());
        let Some(parent) = current.parent() else {
            break;
        };
        current = parent.to_path_buf();
    }
    directories.reverse();
    directories
}

/// Read one directory's `AGENTS.md`, rejecting files outside the workspace.
fn read_scoped_agents_file(
    workspace: &Path,
    directory: &Path,
    root_agents: Option<&Path>,
) -> Option<ScopedAgentsFile> {
    let canonical_path = directory.join("AGENTS.md").canonicalize().ok()?;
    if !canonical_path.starts_with(workspace) || !canonical_path.is_file() {
        return None;
    }
    if root_agents.is_some_and(|root| root == canonical_path.as_path()) {
        return None;
    }
    let content = fs::read_to_string(&canonical_path).ok()?;
    let relative = directory.strip_prefix(workspace).unwrap_or(directory);
    Some(ScopedAgentsFile {
        canonical_path,
        path: scoped_display_path(relative),
        content,
    })
}

/// Render a scoped file's workspace-relative display path.
fn scoped_display_path(relative: &Path) -> String {
    if relative.as_os_str().is_empty() {
        "AGENTS.md".to_string()
    } else {
        format!(
            "{}/AGENTS.md",
            relative.to_string_lossy().replace('\\', "/")
        )
    }
}
