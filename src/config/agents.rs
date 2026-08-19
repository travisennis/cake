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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Runs `body` with a fully isolated instruction environment: empty temp
    /// home and config roots and no data dir, so nothing leaks from the
    /// developer's machine. `HOME` and `XDG_CONFIG_HOME` are set to empty
    /// directories rather than cleared, because `dirs::home_dir()` otherwise
    /// falls back to the OS account home even when `$HOME` is unset.
    fn isolated_env<R>(body: impl FnOnce(PathBuf) -> R) -> R {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let empty_home = root.join("empty-home");
        let empty_config = root.join("empty-config");
        fs::create_dir_all(&empty_home).unwrap();
        fs::create_dir_all(&empty_config).unwrap();
        temp_env::with_vars(
            [
                ("HOME", Some(&empty_home)),
                ("XDG_CONFIG_HOME", Some(&empty_config)),
                ("CAKE_DATA_DIR", None::<&PathBuf>),
            ],
            || body(root),
        )
    }

    #[test]
    fn returns_empty_when_no_files() {
        isolated_env(|root| {
            let working_dir = root.join("workspace");
            fs::create_dir_all(&working_dir).unwrap();
            let files = read_agents_files(&working_dir);
            assert!(files.is_empty());
        });
    }

    #[test]
    fn read_agents_discovers_all_three_in_documented_order() {
        isolated_env(|root| {
            let home = root.join("home");
            let config = root.join("config");
            let working_dir = root.join("workspace");

            fs::create_dir_all(home.join(".cake")).unwrap();
            fs::create_dir_all(&config).unwrap();
            fs::create_dir_all(&working_dir).unwrap();
            fs::write(home.join(".cake").join("AGENTS.md"), "user").unwrap();
            fs::write(config.join("AGENTS.md"), "config").unwrap();
            fs::write(working_dir.join("AGENTS.md"), "project").unwrap();

            let files = temp_env::with_vars(
                [("HOME", Some(&home)), ("XDG_CONFIG_HOME", Some(&config))],
                || read_agents_files(&working_dir),
            );

            let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
            assert_eq!(
                paths,
                vec![
                    "~/.cake/AGENTS.md",
                    "$XDG_CONFIG_HOME/AGENTS.md",
                    "./AGENTS.md"
                ]
            );
            let contents: Vec<&str> = files.iter().map(|f| f.content.as_str()).collect();
            assert_eq!(contents, vec!["user", "config", "project"]);
        });
    }

    #[test]
    fn read_agents_custom_cake_data_dir_does_not_move_or_suppress_instructions() {
        isolated_env(|root| {
            let home = root.join("home");
            let working_dir = root.join("workspace");
            // A poison data root that, under the old behavior, would be walked
            // upward to find a fake "home" .cake/AGENTS.md.
            let data_dir = root.join("data").join("nested").join("deeper");

            fs::create_dir_all(home.join(".cake")).unwrap();
            fs::create_dir_all(working_dir.join(".cake")).unwrap();
            fs::create_dir_all(&data_dir).unwrap();
            fs::write(home.join(".cake").join("AGENTS.md"), "user").unwrap();
            fs::write(working_dir.join("AGENTS.md"), "project").unwrap();
            // This must NOT be loaded as the user-level file.
            fs::write(data_dir.join("AGENTS.md"), "poison").unwrap();

            let files = temp_env::with_vars(
                [("HOME", Some(&home)), ("CAKE_DATA_DIR", Some(&data_dir))],
                || read_agents_files(&working_dir),
            );

            let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
            assert_eq!(paths, vec!["~/.cake/AGENTS.md", "./AGENTS.md"]);
            let contents: Vec<&str> = files.iter().map(|f| f.content.as_str()).collect();
            assert_eq!(contents, vec!["user", "project"]);
        });
    }

    #[test]
    fn custom_home_resolves_user_file_from_home() {
        isolated_env(|root| {
            let home = root.join("home");
            let working_dir = root.join("workspace");

            fs::create_dir_all(home.join(".cake")).unwrap();
            fs::create_dir_all(&working_dir).unwrap();
            fs::write(home.join(".cake").join("AGENTS.md"), "user-from-home").unwrap();

            let files = temp_env::with_var("HOME", Some(&home), || read_agents_files(&working_dir));

            assert_eq!(files.len(), 1);
            assert_eq!(files[0].path, "~/.cake/AGENTS.md");
            assert_eq!(files[0].content, "user-from-home");
        });
    }

    #[test]
    fn empty_xdg_config_home_falls_back_to_config_label() {
        isolated_env(|root| {
            let home = root.join("home");
            let working_dir = root.join("workspace");

            fs::create_dir_all(home.join(".cake")).unwrap();
            fs::create_dir_all(home.join(".config")).unwrap();
            fs::create_dir_all(&working_dir).unwrap();
            fs::write(home.join(".cake").join("AGENTS.md"), "user").unwrap();
            fs::write(home.join(".config").join("AGENTS.md"), "config").unwrap();

            let files = temp_env::with_vars(
                // XDG_CONFIG_HOME set but empty -> treated as unset, so the
                // config root falls back to dirs::home_dir() + .config.
                [
                    ("HOME", Some(home.to_str().unwrap())),
                    ("XDG_CONFIG_HOME", Some("")),
                ],
                || read_agents_files(&working_dir),
            );

            let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
            assert_eq!(paths, vec!["~/.cake/AGENTS.md", "~/.config/AGENTS.md"]);
            let contents: Vec<&str> = files.iter().map(|f| f.content.as_str()).collect();
            assert_eq!(contents, vec!["user", "config"]);
        });
    }

    #[test]
    fn xdg_config_home_label_and_resolution() {
        isolated_env(|root| {
            let home = root.join("home");
            let config = root.join("custom-config");
            let working_dir = root.join("workspace");

            fs::create_dir_all(&config).unwrap();
            fs::create_dir_all(&working_dir).unwrap();
            fs::write(config.join("AGENTS.md"), "config").unwrap();

            let files = temp_env::with_vars(
                [("HOME", Some(&home)), ("XDG_CONFIG_HOME", Some(&config))],
                || read_agents_files(&working_dir),
            );

            assert_eq!(files.len(), 1);
            assert_eq!(files[0].path, "$XDG_CONFIG_HOME/AGENTS.md");
            assert_eq!(files[0].content, "config");
        });
    }
}
