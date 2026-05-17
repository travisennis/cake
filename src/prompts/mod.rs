//! Initial prompt construction for the AI agent.
//!
//! This module builds the stable system prompt plus mutable context messages
//! that are sent to the AI model at the start of each conversation.
//!
//! # System prompt resolution
//!
//! The system prompt is resolved from three sources in precedence order
//! (highest to lowest):
//!
//! 1. **Project-level override**: `.cake/system.md` in the working directory
//! 2. **User-level override**: `system.md` in the user config directory
//!    (typically `~/.config/cake/system.md`)
//! 3. **Built-in default**: `system.md` embedded at compile time
//!
//! The first readable file found wins. Override files replace the default
//! prompt entirely. Empty files are valid (intentional blank prompt).
//! Unreadable files are skipped with a warning.

use std::path::Path;

use chrono::Local;
use tracing::{debug, warn};

use crate::config::{AgentsFile, SkillCatalog};
use crate::models::Role;

/// Built-in default system prompt, embedded at compile time from `system.md`.
const BUILTIN_SYSTEM_PROMPT: &str = include_str!("system.md");

const SKILL_USAGE_INSTRUCTIONS: &str = r"<skill_instructions>
The following skills provide specialized instructions for specific tasks.
When a task matches a skill's description, use your file-read tool to load
the SKILL.md at the listed location before proceeding.
When a skill references relative paths, resolve them against the skill's
directory (the parent of SKILL.md) and use absolute paths in tool calls.
</skill_instructions>";

/// Resolves the system prompt from override files or the built-in default.
///
/// Checks for override files in precedence order:
/// 1. Project-level: `working_dir/.cake/system.md`
/// 2. User-level: `config_dir/system.md`
///
/// The first readable file found is used. Empty files are valid.
/// Unreadable files produce a warning and are skipped.
/// If no override is found, the built-in default is used.
pub fn resolve_system_prompt(working_dir: &Path, config_dir: &Path) -> String {
    let project_path = working_dir.join(".cake").join("system.md");
    let user_path = config_dir.join("system.md");

    if project_path.exists() {
        match std::fs::read_to_string(&project_path) {
            Ok(content) => {
                debug!(
                    "Using project-level system prompt: {}",
                    project_path.display()
                );
                return content.trim().to_string();
            },
            Err(e) => {
                warn!(
                    "Skipping unreadable system prompt file at {}: {e}",
                    project_path.display()
                );
            },
        }
    }

    if user_path.exists() {
        match std::fs::read_to_string(&user_path) {
            Ok(content) => {
                debug!("Using user-level system prompt: {}", user_path.display());
                return content.trim().to_string();
            },
            Err(e) => {
                warn!(
                    "Skipping unreadable system prompt file at {}: {e}",
                    user_path.display()
                );
            },
        }
    }

    debug!("Using built-in system prompt (no override found)");
    BUILTIN_SYSTEM_PROMPT.trim().to_string()
}

/// Builds all initial prompt messages for the AI agent.
///
/// The first message is the stable system prompt. Mutable context such as
/// AGENTS.md contents, available skills, and environment context is emitted as
/// separate developer messages so it is not tied to the system prompt.
pub fn build_initial_prompt_messages(
    working_dir: &Path,
    config_dir: &Path,
    agents_files: &[AgentsFile],
    skill_catalog: &SkillCatalog,
) -> Vec<(Role, String)> {
    let mut messages = vec![(Role::System, resolve_system_prompt(working_dir, config_dir))];
    let context = format_agents_context(agents_files);
    if !context.is_empty() {
        messages.push((Role::Developer, context));
    }

    if !skill_catalog.skills.is_empty() {
        let catalog_xml = skill_catalog.to_prompt_xml();
        if !catalog_xml.is_empty() {
            messages.push((
                Role::Developer,
                format!("## Skills\n\n{SKILL_USAGE_INSTRUCTIONS}\n\n{catalog_xml}"),
            ));
        }
    }

    let today = Local::now().format("%Y-%m-%d").to_string();
    let working_dir_str = working_dir.to_string_lossy();
    messages.push((
        Role::Developer,
        format!("Current working directory: {working_dir_str}\nToday's date: {today}"),
    ));

    messages
}

/// Format AGENTS.md files into a Project Context section.
/// Returns an empty string if no files have non-empty content.
fn format_agents_context(agents_files: &[AgentsFile]) -> String {
    // Filter to only files with non-empty content
    let non_empty_files: Vec<_> = agents_files
        .iter()
        .filter(|f| !f.content.trim().is_empty())
        .collect();

    if non_empty_files.is_empty() {
        return String::new();
    }

    let mut context = String::from("## Additional Context\n\n");

    context.push_str("Project and user instructions are shown below. Be sure to adhere to these instructions. IMPORTANT: These instructions OVERRIDE any default behavior and you MUST follow them exactly as written.");
    context.push_str("\n\n");

    for file in non_empty_files {
        let entry = format!(
            "### {}\n\n<instructions>\n{}\n</instructions>\n\n",
            file.path,
            file.content.trim()
        );
        context.push_str(&entry);
    }

    context
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::skills::{Skill, SkillScope};
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn render_messages(messages: &[(Role, String)]) -> String {
        messages
            .iter()
            .map(|(role, content)| format!("{}:\n{}", role.as_str(), content))
            .collect::<Vec<_>>()
            .join("\n\n---\n\n")
    }

    fn assert_prompt_snapshot(name: &str, messages: &[(Role, String)]) {
        let prompt = render_messages(messages);
        insta::with_settings!({
            filters => vec![(r"Today's date: \d{4}-\d{2}-\d{2}", "Today's date: [DATE]")]
        }, {
            insta::assert_snapshot!(name, prompt);
        });
    }

    // --- resolve_system_prompt tests ---

    #[test]
    fn resolve_uses_builtin_when_no_override() {
        let dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();
        let prompt = resolve_system_prompt(dir.path(), config_dir.path());
        assert!(prompt.starts_with("You are cake."));
    }

    #[test]
    fn resolve_uses_project_level_override() {
        let working_dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();

        let cake_dir = working_dir.path().join(".cake");
        std::fs::create_dir_all(&cake_dir).unwrap();
        std::fs::write(cake_dir.join("system.md"), "Project prompt").unwrap();

        let prompt = resolve_system_prompt(working_dir.path(), config_dir.path());
        assert_eq!(prompt, "Project prompt");
    }

    #[test]
    fn resolve_uses_user_level_override_when_no_project_override() {
        let working_dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();

        std::fs::write(config_dir.path().join("system.md"), "User prompt").unwrap();

        let prompt = resolve_system_prompt(working_dir.path(), config_dir.path());
        assert_eq!(prompt, "User prompt");
    }

    #[test]
    fn resolve_project_level_takes_precedence_over_user_level() {
        let working_dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();

        let cake_dir = working_dir.path().join(".cake");
        std::fs::create_dir_all(&cake_dir).unwrap();
        std::fs::write(cake_dir.join("system.md"), "Project prompt").unwrap();
        std::fs::write(config_dir.path().join("system.md"), "User prompt").unwrap();

        let prompt = resolve_system_prompt(working_dir.path(), config_dir.path());
        assert_eq!(prompt, "Project prompt");
    }

    #[test]
    fn resolve_empty_file_is_valid() {
        let working_dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();

        let cake_dir = working_dir.path().join(".cake");
        std::fs::create_dir_all(&cake_dir).unwrap();
        std::fs::write(cake_dir.join("system.md"), "").unwrap();

        let prompt = resolve_system_prompt(working_dir.path(), config_dir.path());
        assert_eq!(prompt, "");
    }

    #[test]
    fn resolve_whitespace_only_file_is_trimmed_to_empty() {
        let working_dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();

        let cake_dir = working_dir.path().join(".cake");
        std::fs::create_dir_all(&cake_dir).unwrap();
        std::fs::write(cake_dir.join("system.md"), "   \n\n  ").unwrap();

        let prompt = resolve_system_prompt(working_dir.path(), config_dir.path());
        assert_eq!(prompt, "");
    }

    #[test]
    fn resolve_unreadable_project_file_falls_back_to_user_level() {
        let working_dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();

        let cake_dir = working_dir.path().join(".cake");
        std::fs::create_dir_all(&cake_dir).unwrap();
        let project_file = cake_dir.join("system.md");
        std::fs::write(&project_file, "Unreadable").unwrap();
        // Remove read permission
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&project_file, std::fs::Permissions::from_mode(0o000))
                .unwrap();
        }

        std::fs::write(config_dir.path().join("system.md"), "User prompt").unwrap();

        let prompt = resolve_system_prompt(working_dir.path(), config_dir.path());

        #[cfg(unix)]
        {
            assert_eq!(prompt, "User prompt");
        }
        #[cfg(not(unix))]
        {
            // On non-Unix, file permissions may not apply, so project file wins
            assert_eq!(prompt, "Unreadable");
        }

        // Clean up: restore permissions so TempDir can delete the file
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            drop(std::fs::set_permissions(
                &project_file,
                std::fs::Permissions::from_mode(0o644),
            ));
        }
    }

    #[test]
    fn resolve_unreadable_files_fall_back_to_builtin() {
        let working_dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();

        let cake_dir = working_dir.path().join(".cake");
        std::fs::create_dir_all(&cake_dir).unwrap();
        let project_file = cake_dir.join("system.md");
        std::fs::write(&project_file, "Unreadable project").unwrap();
        let user_file = config_dir.path().join("system.md");
        std::fs::write(&user_file, "Unreadable user").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&project_file, std::fs::Permissions::from_mode(0o000))
                .unwrap();
            std::fs::set_permissions(&user_file, std::fs::Permissions::from_mode(0o000)).unwrap();
        }

        let prompt = resolve_system_prompt(working_dir.path(), config_dir.path());

        #[cfg(unix)]
        {
            assert!(prompt.starts_with("You are cake."));
        }
        #[cfg(not(unix))]
        {
            assert!(prompt.starts_with("Unreadable"));
        }

        // Clean up
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            drop(std::fs::set_permissions(
                &project_file,
                std::fs::Permissions::from_mode(0o644),
            ));
            drop(std::fs::set_permissions(
                &user_file,
                std::fs::Permissions::from_mode(0o644),
            ));
        }
    }

    #[test]
    fn resolve_builtin_is_trimmed() {
        let dir = TempDir::new().unwrap();
        let config_dir = TempDir::new().unwrap();
        let prompt = resolve_system_prompt(dir.path(), config_dir.path());
        assert!(!prompt.starts_with('\n'));
        assert!(!prompt.ends_with('\n'));
    }

    // --- build_initial_prompt_messages tests ---

    fn default_config_dir() -> TempDir {
        TempDir::new().unwrap()
    }

    #[test]
    fn empty_agents_files() {
        let config_dir = default_config_dir();
        let messages = build_initial_prompt_messages(
            Path::new("/tmp"),
            config_dir.path(),
            &[],
            &SkillCatalog::empty(),
        );
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].0, Role::System);
        assert!(messages[0].1.starts_with(
            "You are cake. You are running as a coding agent in a CLI on the user's computer."
        ));
        assert_eq!(messages[1].0, Role::Developer);
        assert!(messages[1].1.contains("Current working directory: /tmp"));
        assert!(messages[1].1.contains("Today's date:"));
    }

    #[test]
    fn with_agents_files() {
        let config_dir = default_config_dir();
        let files = vec![
            AgentsFile {
                path: "~/.cake/AGENTS.md".to_string(),
                content: "User level instructions".to_string(),
            },
            AgentsFile {
                path: "./AGENTS.md".to_string(),
                content: "Project level instructions".to_string(),
            },
        ];
        let messages = build_initial_prompt_messages(
            Path::new("/tmp"),
            config_dir.path(),
            &files,
            &SkillCatalog::empty(),
        );
        let prompt = render_messages(&messages);
        assert!(prompt.contains("## Additional Context"));
        assert!(prompt.contains("~/.cake/AGENTS.md"));
        assert!(prompt.contains("./AGENTS.md"));
        assert!(prompt.contains("<instructions>"));
        assert!(prompt.contains("User level instructions"));
        assert!(prompt.contains("Project level instructions"));
        assert!(prompt.contains("Current working directory: /tmp"));
        assert!(prompt.contains("Today's date:"));
    }

    #[test]
    fn only_user_agents_file() {
        let config_dir = default_config_dir();
        let files = vec![AgentsFile {
            path: "~/.cake/AGENTS.md".to_string(),
            content: "User instructions".to_string(),
        }];
        let messages = build_initial_prompt_messages(
            Path::new("/tmp"),
            config_dir.path(),
            &files,
            &SkillCatalog::empty(),
        );
        let prompt = render_messages(&messages);
        assert!(prompt.contains("## Additional Context"));
        assert!(prompt.contains("~/.cake/AGENTS.md"));
        assert!(!prompt.contains("./AGENTS.md"));
        assert!(prompt.contains("Current working directory: /tmp"));
        assert!(prompt.contains("Today's date:"));
    }

    #[test]
    fn empty_content_skipped() {
        let config_dir = default_config_dir();
        let files = vec![
            AgentsFile {
                path: "~/.cake/AGENTS.md".to_string(),
                content: String::new(),
            },
            AgentsFile {
                path: "./AGENTS.md".to_string(),
                content: "   ".to_string(), // whitespace only
            },
        ];
        let messages = build_initial_prompt_messages(
            Path::new("/tmp"),
            config_dir.path(),
            &files,
            &SkillCatalog::empty(),
        );
        let prompt = render_messages(&messages);
        // Should not include Project Context section since all files are empty
        assert!(!prompt.contains("## Additional Context"));
        // But should still include working directory and date
        assert!(prompt.contains("Current working directory: /tmp"));
        assert!(prompt.contains("Today's date:"));
    }

    #[test]
    fn with_skill_catalog() {
        let config_dir = default_config_dir();
        let mut catalog = SkillCatalog::empty();
        catalog.skills.push(Skill {
            name: "debugging".to_string(),
            description: "How to debug things".to_string(),
            location: PathBuf::from("/path/SKILL.md"),
            base_directory: PathBuf::from("/path"),
            scope: SkillScope::Project,
        });

        let messages =
            build_initial_prompt_messages(Path::new("/tmp"), config_dir.path(), &[], &catalog);
        let prompt = render_messages(&messages);
        assert!(prompt.contains("## Skills"));
        assert!(prompt.contains("<skill_instructions>"));
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("<name>debugging</name>"));
        assert!(prompt.contains("<description>How to debug things</description>"));
        assert!(prompt.contains("Current working directory: /tmp"));
    }

    #[test]
    fn with_agents_and_skills() {
        let config_dir = default_config_dir();
        let files = vec![AgentsFile {
            path: "./AGENTS.md".to_string(),
            content: "Project instructions".to_string(),
        }];
        let mut catalog = SkillCatalog::empty();
        catalog.skills.push(Skill {
            name: "test-skill".to_string(),
            description: "A test".to_string(),
            location: PathBuf::from("/a/SKILL.md"),
            base_directory: PathBuf::from("/a"),
            scope: SkillScope::Project,
        });

        let messages =
            build_initial_prompt_messages(Path::new("/tmp"), config_dir.path(), &files, &catalog);
        let prompt = render_messages(&messages);
        // AGENTS.md comes before Skills
        let agents_pos = prompt.find("## Additional Context").unwrap();
        let skills_pos = prompt.find("## Skills").unwrap();
        assert!(agents_pos < skills_pos);
    }

    #[test]
    fn snapshot_empty_prompt() {
        let config_dir = default_config_dir();
        let messages = build_initial_prompt_messages(
            Path::new("/tmp"),
            config_dir.path(),
            &[],
            &SkillCatalog::empty(),
        );
        assert_prompt_snapshot("prompt_empty", &messages);
    }

    #[test]
    fn snapshot_with_project_agents() {
        let config_dir = default_config_dir();
        let files = vec![AgentsFile {
            path: "./AGENTS.md".to_string(),
            content: "You are a Rust expert. Follow all project conventions.".to_string(),
        }];
        let messages = build_initial_prompt_messages(
            Path::new("/project"),
            config_dir.path(),
            &files,
            &SkillCatalog::empty(),
        );
        assert_prompt_snapshot("prompt_with_project_agents", &messages);
    }

    #[test]
    fn snapshot_with_user_and_project_agents() {
        let config_dir = default_config_dir();
        let files = vec![
            AgentsFile {
                path: "~/.cake/AGENTS.md".to_string(),
                content: "User-level global instructions.".to_string(),
            },
            AgentsFile {
                path: "./AGENTS.md".to_string(),
                content: "Project-level overrides.".to_string(),
            },
        ];
        let messages = build_initial_prompt_messages(
            Path::new("/project"),
            config_dir.path(),
            &files,
            &SkillCatalog::empty(),
        );
        assert_prompt_snapshot("prompt_with_user_and_project_agents", &messages);
    }

    #[test]
    fn snapshot_with_skill_catalog() {
        let config_dir = default_config_dir();
        let mut catalog = SkillCatalog::empty();
        catalog.skills.push(Skill {
            name: "debugging".to_string(),
            description: "How to debug Rust programs".to_string(),
            location: PathBuf::from("/project/.agents/skills/debugging/SKILL.md"),
            base_directory: PathBuf::from("/project/.agents/skills/debugging"),
            scope: SkillScope::Project,
        });
        let messages =
            build_initial_prompt_messages(Path::new("/project"), config_dir.path(), &[], &catalog);
        assert_prompt_snapshot("prompt_with_skill_catalog", &messages);
    }

    #[test]
    fn snapshot_with_agents_and_skills() {
        let config_dir = default_config_dir();
        let files = vec![AgentsFile {
            path: "./AGENTS.md".to_string(),
            content: "Project instructions for all contributors.".to_string(),
        }];
        let mut catalog = SkillCatalog::empty();
        catalog.skills.push(Skill {
            name: "debugging".to_string(),
            description: "How to debug Rust programs".to_string(),
            location: PathBuf::from("/project/.agents/skills/debugging/SKILL.md"),
            base_directory: PathBuf::from("/project/.agents/skills/debugging"),
            scope: SkillScope::Project,
        });
        let messages = build_initial_prompt_messages(
            Path::new("/project"),
            config_dir.path(),
            &files,
            &catalog,
        );
        assert_prompt_snapshot("prompt_with_agents_and_skills", &messages);
    }
}
